-- Repositories (spec §3): each pull request's GitHub repository, by GitHub's numeric id, so a
-- rename or a transfer keeps it the same Repository. Its other columns are what GitHub calls
-- and says of it now; PR sync writes them from each PR it reads.
CREATE TABLE repositories (
  id INTEGER PRIMARY KEY,            -- GitHub's repository id (`fullDatabaseId`)
  full_name TEXT NOT NULL,           -- owner/name, as GitHub calls it now
  url TEXT NOT NULL,
  private INTEGER NOT NULL,
  archived INTEGER NOT NULL
);

-- Each PR's Repository, the branch it merges into and who opened it. NULL on a row stored
-- before this migration until PR sync reads its PR again; `author` stays NULL for a PR whose
-- author's account is gone.
ALTER TABLE pull_requests ADD COLUMN repository_id INTEGER REFERENCES repositories;
ALTER TABLE pull_requests ADD COLUMN base_ref TEXT;
ALTER TABLE pull_requests ADD COLUMN author TEXT;
