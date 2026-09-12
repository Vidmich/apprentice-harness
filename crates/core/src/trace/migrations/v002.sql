-- Trace store schema v2 (task M01-02): the workspace registry.
-- Never edit after release: add v003.sql instead.

CREATE TABLE workspaces (
  id            TEXT PRIMARY KEY,
  root          TEXT NOT NULL UNIQUE,            -- canonical absolute path
  name          TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  last_used_at  TEXT NOT NULL,
  settings_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX workspaces_used ON workspaces(last_used_at);

-- `workspace_path` stays as the historical snapshot; `workspace_id` links
-- the session to the registry row while it exists.
ALTER TABLE sessions ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
CREATE INDEX sessions_workspace ON sessions(workspace_id);
