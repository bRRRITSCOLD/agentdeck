-- The v1 schema (spec §3). Ids are UUIDv7 strings; times are RFC 3339 UTC strings.

CREATE TABLE workspaces (
  id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE, name TEXT NOT NULL,
  dir TEXT NOT NULL,                 -- absolute path
  dir_owned INTEGER NOT NULL,        -- 1 = scratch dir agentdeck created, 0 = attached
  muted INTEGER NOT NULL DEFAULT 0,  -- notifications
  archived_at TEXT, created_at TEXT NOT NULL
);

CREATE TABLE tabs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces ON DELETE CASCADE,
  title TEXT NOT NULL, position INTEGER NOT NULL,
  layout TEXT NOT NULL               -- JSON split tree, or JSON null while the tab has no pane
);

CREATE TABLE tasks (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'todo', -- todo|doing|review|done
  kind TEXT NOT NULL DEFAULT 'plain',  -- plain|pr
  workspace_id TEXT REFERENCES workspaces ON DELETE SET NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, done_at TEXT
);

CREATE TABLE panes (
  id TEXT PRIMARY KEY, tab_id TEXT NOT NULL REFERENCES tabs ON DELETE CASCADE,
  kind TEXT NOT NULL,                -- 'agent' | 'shell'
  agent TEXT,                        -- 'claude' | 'codex' | 'other' (agent panes)
  argv TEXT NOT NULL,                -- JSON array, as launched (without resume flags)
  cwd TEXT NOT NULL,
  session_id TEXT,                   -- chosen by agentdeck at launch (claude) or discovered (codex)
  task_id TEXT REFERENCES tasks ON DELETE SET NULL,
  state TEXT NOT NULL DEFAULT 'unknown', -- working|blocked|done|idle|unknown|exited
  title TEXT NOT NULL DEFAULT '', exit_code INTEGER, created_at TEXT NOT NULL
);

CREATE TABLE pull_requests (
  task_id TEXT PRIMARY KEY REFERENCES tasks ON DELETE CASCADE,
  repo TEXT NOT NULL, number INTEGER NOT NULL, url TEXT NOT NULL,
  state TEXT NOT NULL,               -- open|merged|closed
  draft INTEGER NOT NULL, head_ref TEXT NOT NULL,
  checks TEXT,                       -- pending|success|failure|NULL
  review TEXT,                       -- approved|changes_requested|review_required|NULL
  updated_at TEXT NOT NULL, synced_at TEXT NOT NULL,
  UNIQUE (repo, number)
);

CREATE TABLE notes (
  id TEXT PRIMARY KEY, body TEXT NOT NULL,
  workspace_id TEXT REFERENCES workspaces ON DELETE CASCADE,
  task_id TEXT REFERENCES tasks ON DELETE CASCADE,
  created_at TEXT NOT NULL, read_at TEXT, read_by_pane TEXT,
  CHECK ((workspace_id IS NULL) <> (task_id IS NULL))
);

CREATE INDEX tabs_workspace ON tabs (workspace_id, position);
CREATE INDEX tasks_workspace ON tasks (workspace_id);
CREATE INDEX panes_tab ON panes (tab_id);
CREATE INDEX panes_task ON panes (task_id);
CREATE INDEX notes_workspace ON notes (workspace_id);
CREATE INDEX notes_task ON notes (task_id);
