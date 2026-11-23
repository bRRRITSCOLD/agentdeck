-- Finished pull requests (spec §3, §8): when PR sync saw a kept PR merged or closed, a close
-- from agentdeck closed it, or a full sync found it open but not updated within the age
-- cutoff. NULL while it is open and within the cutoff. PR sync deletes a PR 7 days after it
-- became Finished. It replaces `aged_out_at`, which only the cutoff set.
ALTER TABLE pull_requests ADD COLUMN finished_at TEXT;

-- A PR aged out before this migration became Finished then; one stored merged or closed
-- becomes Finished now, so it is kept for its whole window rather than deleted at once.
UPDATE pull_requests SET finished_at = aged_out_at WHERE aged_out_at IS NOT NULL;
UPDATE pull_requests SET finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  WHERE finished_at IS NULL AND state <> 'open';

ALTER TABLE pull_requests DROP COLUMN aged_out_at;
