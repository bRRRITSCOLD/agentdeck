-- Links (spec §3): the tie you make between a Workspace and a Repository, one per pair. A
-- Link goes with its Workspace; a Repository is kept while a Link or a kept pull request
-- names it, so the daemon deletes it with its last Link when no PR belongs to it.
CREATE TABLE links (
  workspace_id TEXT NOT NULL REFERENCES workspaces ON DELETE CASCADE,
  repository_id INTEGER NOT NULL REFERENCES repositories,
  PRIMARY KEY (workspace_id, repository_id)
);
CREATE INDEX links_by_repository ON links (repository_id);

-- When GitHub stopped showing the Repository to the sign-in; NULL while it can see it.
ALTER TABLE repositories ADD COLUMN unreachable_at TEXT;
