-- The agent a shell pane hosts (spec §3, §7): 'claude' or 'codex' while one the user started
-- by hand is the pane's foreground process. NULL otherwise, and for every pane once the daemon
-- starts, since a shell pane comes back as its shell alone.

ALTER TABLE panes ADD COLUMN hosted_agent TEXT;
