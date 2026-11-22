# agentdeck

A desktop deck for running coding agents side by side, organised around the code they work on: the repositories you care about and the pull requests open against them.

## Language

### Code

**Repository**:
A GitHub repository, identified by its owner and name.
_Avoid_: Repo (in UI copy), project

**Pull request**:
A GitHub pull request you authored, were asked to review, or are assigned. It belongs to exactly one Repository, and GitHub is the only source of its state.
_Avoid_: PR task, task

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
