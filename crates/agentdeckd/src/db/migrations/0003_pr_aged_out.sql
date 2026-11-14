-- PR sync's age cutoff (spec §8): when a full sync moved an open PR's task to done because
-- GitHub hadn't updated the PR within `[pr_sync] max_age_days`. NULL for every other PR, and
-- again once GitHub shows the PR within the cutoff.

ALTER TABLE pull_requests ADD COLUMN aged_out_at TEXT;
