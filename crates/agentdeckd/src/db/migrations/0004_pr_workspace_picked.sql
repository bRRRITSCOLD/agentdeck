-- A PR task's workspace the user picked (spec §8): when they last set it with
-- `PATCH /v1/tasks/{id}`, the Inbox included. NULL while PR sync's clone match decides it;
-- once set, sync never moves the task again.

ALTER TABLE pull_requests ADD COLUMN workspace_picked_at TEXT;
