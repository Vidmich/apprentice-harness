-- Trace store schema v3 (task M01-10): the materialised conversation of
-- every session, so resume reads messages, not events.
-- Never edit after release: add v004.sql instead.

-- One row per message as the mentor saw it: `content_json` is the exact
-- content-block array sent or received (tool results already cut to the
-- mentor's size; the raw outputs stay in the trace blobs). `seq` is the
-- message's position in the conversation, from 1.
CREATE TABLE session_messages (
  session_id   TEXT NOT NULL REFERENCES sessions(id),
  seq          INTEGER NOT NULL,
  role         TEXT NOT NULL,                  -- user|assistant|system
  content_json TEXT NOT NULL,
  agent_id     TEXT,
  step_id      TEXT,
  created_at   TEXT NOT NULL,
  PRIMARY KEY (session_id, seq)
);

-- The text blocks of every message, for `session.search`.
CREATE VIRTUAL TABLE session_fts USING fts5(
  session_id UNINDEXED,
  seq UNINDEXED,
  text
);

ALTER TABLE sessions ADD COLUMN message_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN last_agent_status TEXT;        -- ok|cancelled|error
ALTER TABLE sessions ADD COLUMN prompt_version TEXT;           -- set at the first run
ALTER TABLE sessions ADD COLUMN tools_hash TEXT;               -- set at the first run
ALTER TABLE sessions ADD COLUMN title_source TEXT;             -- user|prompt|generated

-- What a call was for: a step of the loop, or the session title.
ALTER TABLE mentor_calls ADD COLUMN kind TEXT NOT NULL DEFAULT 'step';
