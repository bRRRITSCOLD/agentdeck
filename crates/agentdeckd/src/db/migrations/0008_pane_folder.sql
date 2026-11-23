-- A Pane's last known folder (spec §3, §6): where its shell last said it was with OSC 7, else
-- its process's working directory, as the daemon last read either. NULL until the daemon has
-- read one that differs from where the Pane started, when the launch folder `cwd` stands in.
-- Kept after the Pane exits; a Pane resumed after a restart starts there.

ALTER TABLE panes ADD COLUMN folder TEXT;
