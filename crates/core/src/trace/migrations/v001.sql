-- Trace store schema v1. Applied inside a transaction by `trace::schema`.
-- Never edit after release: add v002.sql instead.

CREATE TABLE schema_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE sessions (
  id             TEXT PRIMARY KEY,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  title          TEXT,
  workspace_path TEXT,
  config_json    TEXT NOT NULL,                 -- resolved config snapshot at creation
  status         TEXT NOT NULL DEFAULT 'open'   -- open|archived
);
CREATE INDEX sessions_updated ON sessions(updated_at);

CREATE TABLE agents (
  id              TEXT PRIMARY KEY,
  session_id      TEXT NOT NULL REFERENCES sessions(id),
  parent_agent_id TEXT REFERENCES agents(id),
  kind            TEXT NOT NULL,                -- main|sub|eval
  created_at      TEXT NOT NULL,
  ended_at        TEXT,
  status          TEXT NOT NULL,                -- running|ok|cancelled|error
  task_text       TEXT,
  options_json    TEXT NOT NULL
);
CREATE INDEX agents_session ON agents(session_id, created_at);

-- One step = one mentor call + its tool executions.
CREATE TABLE steps (
  id         TEXT PRIMARY KEY,
  agent_id   TEXT NOT NULL REFERENCES agents(id),
  seq        INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  ended_at   TEXT,
  status     TEXT NOT NULL,                     -- running|ok|cancelled|error
  UNIQUE(agent_id, seq)
);

CREATE TABLE blobs (
  id         TEXT PRIMARY KEY,                  -- sha256 hex of the content
  size       INTEGER NOT NULL,
  media_type TEXT NOT NULL,
  created_at TEXT NOT NULL,
  refcount   INTEGER NOT NULL DEFAULT 1,
  pruned_at  TEXT                             -- set when retention removed the file (row kept)
);

CREATE TABLE events (
  id           TEXT PRIMARY KEY,
  session_id   TEXT NOT NULL REFERENCES sessions(id),
  agent_id     TEXT,
  step_id      TEXT,
  seq          INTEGER NOT NULL,                -- monotonic per session, no gaps
  ts           TEXT NOT NULL,
  kind         TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  blob_id      TEXT REFERENCES blobs(id),
  UNIQUE(session_id, seq)
);
CREATE INDEX events_agent ON events(agent_id, seq);
CREATE INDEX events_kind  ON events(kind, ts);
CREATE INDEX events_blob  ON events(blob_id);

-- Denormalised per mentor call so stats and replay selection need no JSON parsing.
CREATE TABLE mentor_calls (
  id                    TEXT PRIMARY KEY,
  session_id            TEXT NOT NULL,
  agent_id              TEXT NOT NULL,
  step_id               TEXT NOT NULL,
  request_event_id      TEXT NOT NULL REFERENCES events(id),
  response_event_id     TEXT REFERENCES events(id),
  model                 TEXT NOT NULL,
  effort                TEXT,
  started_at            TEXT NOT NULL,
  ended_at              TEXT,
  status                TEXT NOT NULL,          -- running|ok|error|cancelled
  stop_reason           TEXT,
  http_status           INTEGER,
  input_tokens          INTEGER,
  output_tokens         INTEGER,
  cache_read_tokens     INTEGER,
  cache_creation_tokens INTEGER,
  cost_micros           INTEGER,                -- USD * 1e6, computed by token accounting
  first_byte_ms         INTEGER,
  total_ms              INTEGER,
  request_bytes         INTEGER,
  apprentice_applied    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX mentor_calls_time    ON mentor_calls(started_at);
CREATE INDEX mentor_calls_session ON mentor_calls(session_id, started_at);
