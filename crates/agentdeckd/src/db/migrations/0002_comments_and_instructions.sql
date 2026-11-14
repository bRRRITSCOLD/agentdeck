-- Notes split into task comments and workspace instructions (spec §3, §9).
-- Task notes become the user's comments; workspace notes become instructions.

CREATE TABLE comments (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks ON DELETE CASCADE,
  body TEXT NOT NULL,
  author TEXT NOT NULL,              -- user|agent
  author_agent TEXT,                 -- claude|codex|NULL
  author_pane TEXT REFERENCES panes ON DELETE SET NULL,
  created_at TEXT NOT NULL,
  delivered_at TEXT, delivered_to_pane TEXT
);

CREATE TABLE instructions (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces ON DELETE CASCADE,
  body TEXT NOT NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);

INSERT INTO comments (id, task_id, body, author, created_at, delivered_at, delivered_to_pane)
  SELECT id, task_id, body, 'user', created_at, read_at, read_by_pane
  FROM notes WHERE task_id IS NOT NULL;

INSERT INTO instructions (id, workspace_id, body, created_at, updated_at)
  SELECT id, workspace_id, body, created_at, created_at
  FROM notes WHERE workspace_id IS NOT NULL;

DROP TABLE notes;

CREATE INDEX comments_task ON comments (task_id, created_at);
CREATE INDEX instructions_workspace ON instructions (workspace_id, created_at);
