"""Persistence for the UI contract: client preferences and the single local user.

The fork expects aioncore's shapes, so this module keeps them verbatim rather
than inventing our own: `/api/settings/client?keys=` returns a flat map of the
keys the renderer asks for, and writes are a partial update of the same map.
"""

from __future__ import annotations

import json
import os
import sqlite3
import tempfile
import threading
from pathlib import Path
from typing import Any

from ..config import get_settings
from ..cli_overrides import current_settings


def _db_path() -> Path:
    settings = get_settings()
    settings.ensure_dirs()
    return settings.db_path


class Store:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._init()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(_db_path())
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA journal_mode=WAL")
        return conn

    def _init(self) -> None:
        with self._lock, self._connect() as conn:
            conn.executescript(
                """
                CREATE TABLE IF NOT EXISTS client_prefs (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS conversations (
                    id          TEXT PRIMARY KEY,
                    title       TEXT,
                    project_id  TEXT,
                    agent_ids   TEXT,
                    created_at  TEXT,
                    updated_at  TEXT,
                    archived    INTEGER DEFAULT 0,
                    metadata    TEXT DEFAULT '{}'
                );
                CREATE TABLE IF NOT EXISTS messages (
                    id              TEXT PRIMARY KEY,
                    conversation_id TEXT NOT NULL,
                    role            TEXT NOT NULL,
                    author          TEXT,
                    content         TEXT,
                    created_at      TEXT,
                    metadata        TEXT DEFAULT '{}'
                );
                CREATE INDEX IF NOT EXISTS idx_messages_conv ON messages (conversation_id, created_at);
                """
            )
            if not conn.execute("SELECT 1 FROM client_prefs LIMIT 1").fetchone():
                conn.execute(
                    "INSERT INTO client_prefs (key, value) VALUES (?, ?)",
                    ("language", '"zh-CN"'),
                )

    # ----- client preferences ------------------------------------------------

    def get_prefs(self, keys: list[str] | None = None) -> dict[str, Any]:
        with self._lock, self._connect() as conn:
            if keys:
                marks = ",".join("?" * len(keys))
                rows = conn.execute(
                    f"SELECT key, value FROM client_prefs WHERE key IN ({marks})", keys
                ).fetchall()
            else:
                rows = conn.execute("SELECT key, value FROM client_prefs").fetchall()
        out: dict[str, Any] = {}
        for row in rows:
            try:
                out[row["key"]] = json.loads(row["value"])
            except ValueError:
                out[row["key"]] = row["value"]
        return out

    def set_prefs(self, values: dict[str, Any]) -> None:
        with self._lock, self._connect() as conn:
            for key, value in values.items():
                conn.execute(
                    "INSERT INTO client_prefs (key, value) VALUES (?, ?) "
                    "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    (key, json.dumps(value, ensure_ascii=False)),
                )

    # ----- conversations ----------------------------------------------------

    def create_conversation(
        self, conv_id: str, title: str, project_id: str | None, agent_ids: list[str], now: str
    ) -> None:
        with self._lock, self._connect() as conn:
            conn.execute(
                "INSERT INTO conversations (id, title, project_id, agent_ids, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, ?, ?)",
                (conv_id, title, project_id, json.dumps(agent_ids), now, now),
            )

    def list_conversations(self, include_archived: bool = False) -> list[dict[str, Any]]:
        sql = "SELECT * FROM conversations"
        if not include_archived:
            sql += " WHERE archived = 0"
        sql += " ORDER BY updated_at DESC"
        with self._lock, self._connect() as conn:
            rows = conn.execute(sql).fetchall()
        return [self._conv_row(r) for r in rows]

    def get_conversation(self, conv_id: str) -> dict[str, Any] | None:
        with self._lock, self._connect() as conn:
            row = conn.execute("SELECT * FROM conversations WHERE id = ?", (conv_id,)).fetchone()
        return self._conv_row(row) if row else None

    def delete_conversation(self, conv_id: str) -> None:
        with self._lock, self._connect() as conn:
            conn.execute("DELETE FROM messages WHERE conversation_id = ?", (conv_id,))
            conn.execute("DELETE FROM conversations WHERE id = ?", (conv_id,))

    def set_archived(self, conv_id: str, archived: bool) -> None:
        with self._lock, self._connect() as conn:
            conn.execute(
                "UPDATE conversations SET archived = ? WHERE id = ?", (1 if archived else 0, conv_id)
            )

    def add_message(
        self,
        msg_id: str,
        conv_id: str,
        role: str,
        content: str,
        author: str | None,
        now: str,
        metadata: dict[str, Any] | None = None,
    ) -> None:
        with self._lock, self._connect() as conn:
            conn.execute(
                "INSERT OR REPLACE INTO messages "
                "(id, conversation_id, role, author, content, created_at, metadata) "
                "VALUES (?, ?, ?, ?, ?, ?, ?)",
                (msg_id, conv_id, role, author, content, now, json.dumps(metadata or {})),
            )
            conn.execute("UPDATE conversations SET updated_at = ? WHERE id = ?", (now, conv_id))

    def list_messages(self, conv_id: str) -> list[dict[str, Any]]:
        with self._lock, self._connect() as conn:
            rows = conn.execute(
                "SELECT * FROM messages WHERE conversation_id = ? ORDER BY created_at ASC",
                (conv_id,),
            ).fetchall()
        out = []
        for row in rows:
            item = dict(row)
            try:
                item["metadata"] = json.loads(item.get("metadata") or "{}")
            except ValueError:
                item["metadata"] = {}
            out.append(item)
        return out

    @staticmethod
    def _conv_row(row: sqlite3.Row) -> dict[str, Any]:
        item = dict(row)
        try:
            item["agent_ids"] = json.loads(item.get("agent_ids") or "[]")
        except ValueError:
            item["agent_ids"] = []
        try:
            item["metadata"] = json.loads(item.get("metadata") or "{}")
        except ValueError:
            item["metadata"] = {}
        return item


_store: Store | None = None


def store() -> Store:
    global _store
    if _store is None:
        _store = Store()
    return _store


# ----- local user ------------------------------------------------------------


def local_username() -> str:
    return os.environ.get("CREWCHAT_USERNAME") or "crewchat"


def local_password() -> str:
    return current_settings().local_password


def user_payload() -> dict[str, Any]:
    return {
        "id": "local",
        "username": local_username(),
        "display_name": local_username(),
        "role": "admin",
        "created_at": None,
    }


def overrides_file_exists() -> bool:
    from ..cli_overrides import cli_overrides_path

    return overrides_path(current_settings()).exists()


def _atomic_write(path: Path, text: str) -> None:
    handle, tmp = tempfile.mkstemp(dir=str(path.parent), suffix=".tmp")
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as fh:
            fh.write(text)
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise
