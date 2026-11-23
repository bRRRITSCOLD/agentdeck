# PROTOTYPE: navigating Pull requests, Workspaces and Repositories

Throwaway. This branch is never merged. It records the prototype for
[Navigating Pull requests, Workspaces and Repositories](https://github.com/bRRRITSCOLD/agentdeck/issues/349)
on the map [rethink agentdeck's domain](https://github.com/bRRRITSCOLD/agentdeck/issues/341).
The decisions live in that ticket's resolution comment. Only those decisions go into the spec.

## The question

Where do you see a Workspace's PRs and the Repositories it links, given:

- a global Pull requests view grouped by Repository;
- PRs in Repositories that no Workspace links;
- the empty states: no Workspaces, a Workspace with no Repositories, and logged out?

## What was built

These are Penpot boards in the agentdeck file, on the page **PROTOTYPE · 349 navigation**, exported in Dark.

| Row | Boards |
|---|---|
| Shared | `shared-1-pull-requests`, `shared-2-row-menu`, `shared-3-select`, `shared-4-logged-out`, `shared-5-from-a-notification`, `shared-6-no-workspaces` |
| A · Right panel | `a-1-workspace`, `a-2-workspace-no-repositories`: a Repositories panel beside the panes |
| B · Sidebar tree | `b-1-workspace`, `b-2-repository`, `b-3-repositories`: Repositories nest under Workspaces, and each Repository has a page |
| C · Workspace tabs | `c-1-workspace-panes`, `c-2-workspace-pull-requests`, `c-3-edit-workspace`, `c-4-workspace-no-repositories`: Panes and Pull requests sides; Edit workspace lists the Repositories |

## Verdict

**C**, with one change from the boards. Unlinked PRs don't share one flat "Not in a workspace" group. Each sits in its own Repository group, whose header reads "Not in a workspace". The resolution comment on the ticket has every decision.
