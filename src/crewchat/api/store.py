"""Persistence for the UI contract: client preferences and the single local user.

The fork expects aioncore's shapes, so this module keeps them verbatim rather
than inventing our own: `/api/settings/client?keys=` returns a flat map of the
keys the renderer asks for, and writes are a partial update of the same map.
"""

from __future__ import annotations

import json
import os
import sqlite3
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from ..core.config import get_settings
from ..core.cli_overrides import current_settings


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
                CREATE TABLE IF NOT EXISTS usage_events (
                    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_id        TEXT,
                    turn_id                TEXT,
                    agent_id               TEXT,
                    body                   TEXT NOT NULL,
                    model                  TEXT,
                    input_tokens           INTEGER NOT NULL DEFAULT 0,
                    output_tokens          INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens      INTEGER NOT NULL DEFAULT 0,
                    cache_creation_tokens  INTEGER NOT NULL DEFAULT 0,
                    reasoning_tokens       INTEGER NOT NULL DEFAULT 0,
                    cost_usd               REAL,
                    duration_ms            INTEGER,
                    created_at             TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_usage_created ON usage_events (created_at);
                CREATE INDEX IF NOT EXISTS idx_usage_body ON usage_events (body);
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

    # ----- usage -------------------------------------------------------------

    #: 聚合列片段：总量、按身体、按模型、按天共用，保证口径一致。
    _USAGE_AGG = (
        "COUNT(*) AS requests, "
        "COALESCE(SUM(input_tokens), 0) AS input_tokens, "
        "COALESCE(SUM(output_tokens), 0) AS output_tokens, "
        "COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens, "
        "COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens, "
        "COALESCE(SUM(reasoning_tokens), 0) AS reasoning_tokens, "
        "COALESCE(SUM(cost_usd), 0) AS cost_usd"
    )

    def record_usage(
        self,
        *,
        conversation_id: str,
        turn_id: str,
        agent_id: str,
        body: str,
        model: str,
        usage: dict[str, Any],
        cost_usd: float | None,
        duration_ms: int | None,
    ) -> None:
        """记一条**经我们 app 发起**的 CLI 身体运行消耗。不读网关日志。"""
        with self._lock, self._connect() as conn:
            conn.execute(
                "INSERT INTO usage_events "
                "(conversation_id, turn_id, agent_id, body, model, input_tokens, "
                " output_tokens, cache_read_tokens, cache_creation_tokens, reasoning_tokens, "
                " cost_usd, duration_ms, created_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    conversation_id,
                    turn_id,
                    agent_id,
                    body,
                    model,
                    int(usage.get("input_tokens") or 0),
                    int(usage.get("output_tokens") or 0),
                    int(usage.get("cache_read_tokens") or 0),
                    int(usage.get("cache_creation_tokens") or 0),
                    int(usage.get("reasoning_tokens") or 0),
                    cost_usd,
                    duration_ms,
                    datetime.now(timezone.utc).isoformat(),
                ),
            )

    def usage_summary(self, days: int = 30) -> dict[str, Any]:
        since = (datetime.now(timezone.utc) - timedelta(days=days)).isoformat()
        agg = self._USAGE_AGG
        with self._lock, self._connect() as conn:
            totals = conn.execute(
                f"SELECT {agg} FROM usage_events WHERE created_at >= ?", (since,)
            ).fetchone()
            by_body = conn.execute(
                f"SELECT body, {agg} FROM usage_events WHERE created_at >= ? "
                "GROUP BY body ORDER BY requests DESC",
                (since,),
            ).fetchall()
            by_model = conn.execute(
                f"SELECT model, body, {agg} FROM usage_events WHERE created_at >= ? "
                "GROUP BY model, body ORDER BY requests DESC",
                (since,),
            ).fetchall()
            by_day = conn.execute(
                f"SELECT substr(created_at, 1, 10) AS date, {agg} "
                "FROM usage_events WHERE created_at >= ? GROUP BY date ORDER BY date DESC",
                (since,),
            ).fetchall()
        return {
            "days": days,
            "totals": dict(totals) if totals else {},
            "by_body": [dict(row) for row in by_body],
            "by_model": [dict(row) for row in by_model],
            "by_day": [dict(row) for row in by_day],
        }

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
