# agentdeck

A desktop deck for running coding agents side by side, organised around the code they work on: the repositories you care about and the pull requests open against them.

## Language

### Code

**Repository**:
A GitHub repository that a Workspace links or that one of your Pull requests belongs to. GitHub identifies it, so a rename or transfer leaves it the same Repository; its owner and name are only what it is called now.
_Avoid_: Repo (in UI copy), project

**Unreachable**:
A Repository or Pull request your GitHub sign-in can no longer see. It is kept and marked, not removed.
_Avoid_: Deleted, gone, missing

**Not in a workspace**:
A Repository that no Workspace links, counting only Workspaces that aren't archived.
_Avoid_: Unlinked, orphan, unassigned

**Pull request**:
A GitHub pull request you authored, were asked by name to review, or are assigned. It belongs to exactly one Repository, and GitHub is the only source of its state.
_Avoid_: PR task, task

**Finished**:
A Pull request kept for a short while after it was merged or closed, stopped being yours, or went untouched past the age cutoff.
_Avoid_: Done, stale, archived

### Organising

**Workspace**:
A long-lived area of work, such as one product: the Repositories linked to it, which may be none, and its Panes. It has no folder of its own.
_Avoid_: Project, folder, effort

**Pane**:
A terminal inside a Workspace, running an agent or a shell, in whatever folder it was started or has moved to.
_Avoid_: Tab, session

**Link**:
The tie, made by you, between a Workspace and a Repository. One Repository can be linked to many Workspaces; its Pull requests show in each.
_Avoid_: Attach, assign, match

**Archived**:
A Workspace put away until you bring it back. It keeps its Links and Panes, but counts as linking nothing, so its Repositories read Not in a workspace and their Pull requests alert as if linked nowhere.
_Avoid_: Closed, hidden, deleted

**Muted**:
A Workspace that doesn't alert you. Its Panes stay quiet, and a Pull request stays quiet only when every Workspace linking its Repository is Muted; Archived Workspaces don't count. Only Workspaces are Muted, never a Repository or a single Pull request.
_Avoid_: Silenced, snoozed, hidden

**Suggestion**:
A Repository that one of a Workspace's Panes works in and that the Workspace doesn't link, offered for linking. Nothing is stored except that you said Not now.
_Avoid_: Detected repo, auto-link, match

### Views

**Pull requests view**:
The one place that lists all your Pull requests, each under its Repository.
_Avoid_: Inbox, all PRs, PR list

**A Workspace's Pull requests**:
The Pull requests in the Repositories a Workspace links: the Workspace's other side, beside its Panes.
_Avoid_: PR tab, Tasks & PRs
