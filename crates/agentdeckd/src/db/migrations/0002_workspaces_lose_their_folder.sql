-- A Workspace has no folder (spec §3): no scratch folder, no attached folder. Scratch folders
-- agentdeck made stay on disk, since they may hold work.

ALTER TABLE workspaces DROP COLUMN dir;
ALTER TABLE workspaces DROP COLUMN dir_owned;
