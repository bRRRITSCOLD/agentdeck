-- PRs the GitHub sign-in can't see (spec §8): when a full sync last fetched an open PR by
-- number and GitHub didn't show it to the token (not found, or no access). The PR keeps its
-- state and its task keeps its status; NULL again once GitHub shows it.

ALTER TABLE pull_requests ADD COLUMN unreachable_at TEXT;

-- The GitHub login of the last good PR sync, so a signed-out Inbox can say whose PRs it shows.
-- At most one row.
CREATE TABLE pr_sync (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  synced_as TEXT NOT NULL
);
