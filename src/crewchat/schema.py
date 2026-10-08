from __future__ import annotations

import sqlite3
from pathlib import Path

from .config import get_settings

SCHEMA = Path(__file__).parent / "schema.sql"


def ensure_schema() -> None:
    """跑一次 CREATE TABLE IF NOT EXISTS；幂等，可安全重复调用。"""
    settings = get_settings()
    settings.ensure_dirs()
    conn = sqlite3.connect(settings.db_path)
    try:
        conn.executescript(SCHEMA.read_text(encoding="utf-8"))
        conn.commit()
    finally:
        conn.close()