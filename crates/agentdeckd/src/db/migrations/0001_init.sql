-- The schema (spec §3), squashed into one migration at the domain reset: a database from
-- before it, one with a `tasks` table, is moved aside at start and this one starts empty
-- (spec §2). Ids are UUIDv7 strings but for a Repository's, GitHub's numeric id; times are
-- RFC 3339 UTC strings to the microsecond.

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

CREATE TABLE panes (
  id TEXT PRIMARY KEY, tab_id TEXT NOT NULL REFERENCES tabs ON DELETE CASCADE,
  kind TEXT NOT NULL,                -- 'agent' | 'shell'
  agent TEXT,                        -- 'claude' | 'codex' | 'other' (agent panes)
  hosted_agent TEXT,                 -- 'claude' | 'codex' while a shell pane hosts that agent
  argv TEXT NOT NULL,                -- JSON array, as launched (without resume flags)
  cwd TEXT NOT NULL,                 -- the folder it launched in
  folder TEXT,                       -- its last known current folder; NULL: `cwd`
  session_id TEXT,                   -- chosen by agentdeck at launch (claude) or discovered (codex)
  state TEXT NOT NULL DEFAULT 'unknown', -- working|blocked|done|idle|unknown|exited
  title TEXT NOT NULL DEFAULT '', exit_code INTEGER, created_at TEXT NOT NULL
);

-- A GitHub repository a Workspace links or a kept pull request belongs to, by GitHub's id,
-- which a rename or a transfer keeps. Deleted once neither references it.
CREATE TABLE repositories (
  id INTEGER PRIMARY KEY,            -- GitHub's id (`fullDatabaseId`, REST's `id`)
  full_name TEXT NOT NULL,           -- owner/name, as GitHub calls it now
  url TEXT NOT NULL,
  private INTEGER NOT NULL, archived INTEGER NOT NULL,
  unreachable_at TEXT                -- when GitHub stopped showing it to the sign-in
);

-- A Workspace's tie to a Repository, made by the user; it goes with its Workspace.
CREATE TABLE links (
  workspace_id TEXT NOT NULL REFERENCES workspaces ON DELETE CASCADE,
  repository_id INTEGER NOT NULL REFERENCES repositories,
  PRIMARY KEY (workspace_id, repository_id)
);

-- A pull request PR sync keeps, named by its Repository and number: no unique name, so a
-- renamed Repository's old name taken by another never collides.
CREATE TABLE pull_requests (
  repository_id INTEGER NOT NULL REFERENCES repositories,
  number INTEGER NOT NULL,
  title TEXT NOT NULL, url TEXT NOT NULL,
  state TEXT NOT NULL,               -- open|merged|closed
  draft INTEGER NOT NULL,
  head_ref TEXT NOT NULL, base_ref TEXT NOT NULL,
  author TEXT,                       -- the login that opened it; NULL once that account is gone
  checks TEXT,                       -- pending|success|failure|NULL
  review TEXT,                       -- approved|changes_requested|review_required|NULL
  updated_at TEXT NOT NULL,          -- GitHub's
  synced_at TEXT NOT NULL,
  finished_at TEXT,                  -- when it became Finished; NULL while open within the cutoff
  unreachable_at TEXT,               -- when a full sync found the sign-in can't see it
  PRIMARY KEY (repository_id, number)
);

-- The GitHub login of the last good PR sync. At most one row.
CREATE TABLE pr_sync (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  synced_as TEXT NOT NULL
);

CREATE INDEX tabs_workspace ON tabs (workspace_id, position);
CREATE INDEX panes_tab ON panes (tab_id);
CREATE INDEX links_by_repository ON links (repository_id);
