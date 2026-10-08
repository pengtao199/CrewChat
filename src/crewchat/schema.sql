-- CrewChat 本地库：Chainlit SQLAlchemyDataLayer 不自带建表语句，
-- 以下 schema 从该 layer 自己的 SQL（sql_alchemy.py）逐条提取，字段一一对应。
-- SQLite 不支持部分 Postgres 语法（如 NULLS LAST / ON CONFLICT 除外），已做等价改写。

CREATE TABLE IF NOT EXISTS "users" (
    "id"         TEXT PRIMARY KEY,
    "identifier" TEXT NOT NULL UNIQUE,
    "createdAt"  TEXT,
    "metadata"   TEXT
);

CREATE TABLE IF NOT EXISTS "threads" (
    "id"            TEXT PRIMARY KEY,
    "createdAt"     TEXT,
    "name"          TEXT,
    "userId"        TEXT,
    "userIdentifier" TEXT,
    "tags"          TEXT,
    "metadata"      TEXT
);

CREATE TABLE IF NOT EXISTS "steps" (
    "id"            TEXT PRIMARY KEY,
    "name"          TEXT,
    "type"          TEXT,
    "threadId"      TEXT,
    "parentId"      TEXT,
    "streaming"     INTEGER,
    "waitForAnswer" INTEGER,
    "isError"       INTEGER,
    "metadata"      TEXT,
    "tags"          TEXT,
    "input"         TEXT,
    "output"        TEXT,
    "createdAt"     TEXT,
    "start"         TEXT,
    "end"           TEXT,
    "generation"    TEXT,
    "showInput"     TEXT,
    "language"      TEXT,
    "icon"          TEXT,
    "command"       TEXT,
    "modes"         TEXT,
    "defaultOpen"   INTEGER,
    "autoCollapse"  INTEGER,
    "feedback"      TEXT
);

CREATE TABLE IF NOT EXISTS "elements" (
    "id"          TEXT PRIMARY KEY,
    "threadId"    TEXT,
    "type"        TEXT,
    "chainlitKey" TEXT,
    "path"        TEXT,
    "url"         TEXT,
    "objectKey"   TEXT,
    "name"        TEXT,
    "display"     TEXT,
    "size"        TEXT,
    "language"    TEXT,
    "page"        INTEGER,
    "props"       TEXT,
    "autoPlay"    INTEGER,
    "playerConfig" TEXT,
    "forId"       TEXT,
    "mime"        TEXT
);

CREATE TABLE IF NOT EXISTS "feedbacks" (
    "id"       TEXT PRIMARY KEY,
    "forId"    TEXT,
    "threadId" TEXT,
    "value"    INTEGER,
    "comment"  TEXT
);

CREATE INDEX IF NOT EXISTS "idx_threads_userId"      ON "threads" ("userId");
CREATE INDEX IF NOT EXISTS "idx_steps_threadId"      ON "steps" ("threadId");
CREATE INDEX IF NOT EXISTS "idx_elements_threadId"   ON "elements" ("threadId");
CREATE INDEX IF NOT EXISTS "idx_feedbacks_forId"     ON "feedbacks" ("forId");
CREATE INDEX IF NOT EXISTS "idx_steps_createdAt"     ON "steps" ("createdAt");