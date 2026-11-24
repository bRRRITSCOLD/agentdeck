-- Why each kept pull request is yours (spec §3, §8): PR sync keeps the PRs you authored, those
-- where your review is requested and those assigned to you, and an open one that is yours none
-- of these ways is Finished. Every PR kept before this was found as one you authored.

ALTER TABLE pull_requests ADD COLUMN authored INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pull_requests ADD COLUMN review_requested INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pull_requests ADD COLUMN assigned INTEGER NOT NULL DEFAULT 0;
UPDATE pull_requests SET authored = 1;
