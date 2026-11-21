# What depends on tasks, the Inbox, comments and Instructions

Research for [agentdeck#346](https://github.com/bRRRITSCOLD/agentdeck/issues/346), under the map [agentdeck#341](https://github.com/bRRRITSCOLD/agentdeck/issues/341). It feeds [agentdeck#347](https://github.com/bRRRITSCOLD/agentdeck/issues/347) (MCP server and CLI) and [agentdeck#348](https://github.com/bRRRITSCOLD/agentdeck/issues/348) (what each task-built feature becomes).

- **Source:** `origin/main` at `62abe20` (2026-11-21): the code under `crates/`, `openapi.json`, `README.md` and `docs/spec.md`. Spec sections are cited as §N.
- **Scope:** everything that reads, writes or is shaped by tasks, the Inbox, task comments, workspace Instructions, or PR→workspace matching.
- **What this is:** an inventory of what exists, grouped by feature. It records facts only and makes no recommendation about what any feature should become.
- **Paths:** relative to the repo root. Test counts are `fn should…` counts per file.

## Contents

0. [Foundations: data, types, API, events](#0-foundations-data-types-api-events)
1. [PRs stored as PR tasks](#1-prs-stored-as-pr-tasks)
2. [Status sync: PR state moves the task's status](#2-status-sync-pr-state-moves-the-tasks-status)
3. [PR→workspace matching, and the user's pick](#3-prworkspace-matching-and-the-users-pick)
4. [Unreachable PRs](#4-unreachable-prs)
5. [Aged-out PRs and the age cutoff](#5-aged-out-prs-and-the-age-cutoff)
6. [Bulk close (Closing PRs)](#6-bulk-close-closing-prs)
7. [Opening PRs on GitHub](#7-opening-prs-on-github)
8. [Sync now, sync labels and the logged-out banner](#8-sync-now-sync-labels-and-the-logged-out-banner)
9. [The Inbox view](#9-the-inbox-view)
10. [The Tasks view](#10-the-tasks-view)
11. [The right panel](#11-the-right-panel)
12. [The task sheet](#12-the-task-sheet)
13. [New task dialog](#13-new-task-dialog)
14. [Send to agent](#14-send-to-agent)
15. [Pane ↔ task links: Link to task, Unlink task, header, "(done)"](#15-pane--task-links)
16. [Launch prompt, task brief, New agent and Start agent](#16-launch-prompt-task-brief-new-agent-and-start-agent)
17. [Comment delivery to agents](#17-comment-delivery-to-agents)
18. [Agent comments and the unseen mark](#18-agent-comments-and-the-unseen-mark)
19. [Workspace Instructions](#19-workspace-instructions)
20. [MCP tools](#20-mcp-tools)
21. [CLI commands](#21-cli-commands)
22. [Settings](#22-settings)
23. [Notifications](#23-notifications)
24. [Sidebar: Inbox and Tasks entries, counts, footer](#24-sidebar)
25. [Workspace lifecycle: archive, delete](#25-workspace-lifecycle)
26. [Layout and chrome coupled to the task lists](#26-layout-and-chrome-coupled-to-the-task-lists)
27. [Penpot boards the spec names](#27-penpot-boards-the-spec-names)
28. [README and other docs](#28-readme-and-other-docs)

---

## 0. Foundations: data, types, API, events

Everything below rests on these.

### Tables and migrations (§3)

All migrations are in `crates/agentdeckd/src/db/migrations/` and are listed in `crates/agentdeckd/src/db/mod.rs`.

| Table / column | What it holds | Migration |
|---|---|---|
| `tasks` | `id, title, body, status (todo\|doing\|review\|done), kind (plain\|pr), workspace_id` (NULL = Inbox, `ON DELETE SET NULL`), `created_at, updated_at, done_at`; index `tasks_workspace` | `0001_init.sql` |
| `panes.task_id` | the pane's link, `REFERENCES tasks ON DELETE SET NULL`; index `panes_task` | `0001_init.sql` |
| `pull_requests` | **primary key `task_id REFERENCES tasks ON DELETE CASCADE`**: a PR row has no id of its own, so each PR exists only as a PR task. `repo, number (UNIQUE together), url, state, draft, head_ref, checks, review, updated_at, synced_at` | `0001_init.sql` |
| `notes` | v1's first table, a note on a workspace or a task | created `0001`, dropped `0002` |
| `comments` | `task_id … ON DELETE CASCADE, body, author (user\|agent), author_agent, author_pane, created_at, delivered_at, delivered_to_pane, seen_at`; index `comments_task` | `0002_comments_and_instructions.sql` (copies task notes in) |
| `instructions` | `workspace_id … ON DELETE CASCADE, body, created_at, updated_at`; index `instructions_workspace` | `0002_comments_and_instructions.sql` (copies workspace notes in) |
| `pull_requests.aged_out_at` | the age cutoff's mark (§5 here) | `0003_pr_aged_out.sql` |
| `pull_requests.workspace_picked_at` | the user's workspace pick for a PR task (§3 here) | `0004_pr_workspace_picked.sql` |
| `pull_requests.unreachable_at`, table `pr_sync (synced_as)` | unreachable PRs (§4 here); the login of the last good sync | `0006_pr_unreachable.sql` |

`0005_pane_hosted_agent.sql` doesn't touch tasks. Test: `crates/agentdeckd/tests/integration/migrations.rs` `mod migration_0002` (2 tests: task notes become comments, workspace notes become instructions).

### Core types (`agentdeck-core`)

- `crates/agentdeck-core/src/task.rs`: `TaskStatus`, `TaskKind`, `TaskPlace` (Inbox vs workspace, shared with the client).
- `crates/agentdeck-core/src/pr.rs`: `PrState`, `ChecksState`, `ReviewState` and `pr_task_status` (§2 here).
- `crates/agentdeck-core/src/comment.rs`: `CommentAuthor`.
- `crates/agentdeck-core/src/id.rs`: `TaskId` (its doc says it is "also a pull request's key"), `CommentId`, `InstructionId`.
- `crates/agentdeck-core/src/ws.rs`: `Entity::{Task, Comment, Instruction}` for `changed` events.
- `crates/agentdeck-core/src/schema.rs`: a test of `TaskStatus`'s schema.
- `crates/agentdeck-core/src/lib.rs` re-exports all of these.

### Daemon model and DB modules

- `crates/agentdeckd/src/model.rs`:
  - `Task`, which carries `pr: Option<PullRequest>` and `unseen_agent_comments`;
  - `PullRequest`, which has no id and carries `unreachable`;
  - `NewTask`, `TaskPatch`, `TaskFilter`;
  - `Comment`, `NewComment`, `Instruction`, `NewInstruction`, `InstructionPatch`;
  - `PrClose { task_ids }`, `PrCloseResult { task_id, … }`;
  - `Pane.task_id`, `NewPane.task_id`, `PanePatch { task_id }`.
- `crates/agentdeckd/src/db/tasks.rs`: `list` with filters, `get`, `create`, `create_kind`, `update`, `edit` (stamps `workspace_picked_at`, unlinks panes in other workspaces), `delete`.
- `crates/agentdeckd/src/db/pull_requests.rs`: all PR storage (§1–§6 here).
- `crates/agentdeckd/src/db/comments.rs`: `list`, `undelivered`, `create`, `mark_delivered`, `mark_seen`, `delete`.
- `crates/agentdeckd/src/db/instructions.rs`: CRUD.
- `crates/agentdeckd/src/db/panes.rs`: `agents_on_task`, `set_task`.

### HTTP routes (§4)

Registered in `crates/agentdeckd/src/api/mod.rs`, with handlers in `crates/agentdeckd/src/api/{tasks,comments,instructions,prs,panes}.rs`.

| Route | operationId |
|---|---|
| `GET/POST /v1/tasks`, `GET/PATCH/DELETE /v1/tasks/{id}` | `task_list`, `task_create`, `task_get`, `task_update`, `task_delete` |
| `GET/POST /v1/tasks/{id}/comments`, `POST /v1/tasks/{id}/comments/seen`, `DELETE /v1/comments/{id}` | `comment_list`, `comment_create`, `comment_seen`, `comment_delete` |
| `GET/POST /v1/workspaces/{id}/instructions`, `PATCH/DELETE /v1/instructions/{id}` | `instruction_list`, `instruction_create`, `instruction_update`, `instruction_delete` |
| `POST /v1/prs/close` (body `task_ids`) | `prs_close` |
| `POST /v1/panes` (`task_id?`), `PATCH /v1/panes/{id}` (`task_id`) | `pane_create`, `pane_update` |

- **Error codes tied to tasks:** `invalid_task`, `unknown_task`, `invalid_title` (a PR task's title) and `unknown_workspace` (task PATCH).
- **Routes with no caller in the CLI or GUI:** `DELETE /v1/tasks/{id}` and `DELETE /v1/comments/{id}`. Only daemon integration tests call them (`tests/integration/tasks.rs:614`, `comments.rs:364`).
- **`openapi.json`:** the schemas `Task`, `TaskId`, `TaskKind`, `TaskPatch`, `TaskPlace`, `TaskStatus`, `NewTask`, `PullRequest`, `PrClose*`, `Comment*`, `NewComment`, `Instruction*`, `NewInstruction`. `API_VERSION` is 15 (`crates/agentdeck-core/src/daemon.rs`).
- **Client:**
  - `crates/agentdeck-client/src/generated.rs` is generated;
  - `crates/xtask/src/client.rs` `CORE_TYPES` lists `TaskId`, `CommentId`, `InstructionId`, `CommentAuthor`, `TaskStatus`, `TaskKind` and the PR enums;
  - `crates/agentdeck-client/src/instructions.rs` holds `send_instruction`;
  - `crates/agentdeck-client/src/panes.rs` holds `link_task` and `launch_agent(…, task)`.

### WebSocket events (§5)

- The daemon sends `changed` with `entity` `task`, `comment` or `instruction`, from `crates/agentdeckd/src/events.rs` and from each handler above.
- The GUI re-reads on `Changed::Task` and the rest (`crates/agentdeck-gui/src/workspace.rs:136`, `sidebar.rs`).
- `crates/agentdeckd/tests/integration/ws.rs` `mod events` uses task creation as its `changed` fixture.

### Daemon tests for the CRUD itself

In `crates/agentdeckd/tests/integration/`:

- `tasks.rs`: 21 tests.
- `comments.rs`: 16.
- `instructions.rs`: 9.
- `instructions_client.rs`: 3.

---

## 1. PRs stored as PR tasks

**What:** PR sync makes each synced PR a task of `kind = pr`. The task's title is the PR's title. The `pull_requests` row hangs off the task, keyed by `task_id`. Every API object that carries a PR is a `Task` with `pr` set. PR close takes task ids. There is no PR entity apart from its task.

- **Spec:** §3 (`pull_requests` keyed by `task_id`), §8 ("Each result becomes a PR task"), §4 (`PATCH /v1/tasks/{id}`: "another title for a PR task" is `invalid_title`).
- **Daemon:**
  - `crates/agentdeckd/src/prsync/mod.rs` (`store`, `read_full`, `read_since`, `closed`) sends a `changed` task event per task created or changed.
  - `crates/agentdeckd/src/db/pull_requests.rs`: `store_one` creates the task with `tasks::create_kind(…, TaskKind::Pr)` and writes the PR row by `task_id`. `task_for` looks a PR up by repo and number. `write_row` writes it.
  - `crates/agentdeckd/src/db/tasks.rs`: `pr_from_row` joins `pull_requests` into every task read.
- **GitHub client:** `crates/agentdeckd/src/prsync/github.rs` never mentions tasks (GraphQL, list, search, fetch by number, close).
- **GUI:** every PR surface works from `Task.pr`:
  - `crates/agentdeck-gui/src/task_list.rs` (row PR fields), `task_row.rs` (the kind icon shows the PR's state), `task_sheet.rs` (PR line);
  - `overview.rs` (counts), `notifications.rs` (PR alerts keyed `task:<id>`), `open_pr.rs`.
- **CLI:** `crates/agentdeck/src/render.rs`: `pr_label` and `task` render a PR inside a task. `close_results` maps `TaskId → PullRequest`.
- **Tests:** `crates/agentdeckd/tests/integration/prs.rs`, 73 tests in all. The ones that are about PR tasks as such:
  - `post_v1_prs_sync::should_put_a_new_pr_in_the_inbox_in_review_200`;
  - `…should_tell_websocket_clients_which_tasks_changed_200`;
  - `…should_update_checks_and_review_on_the_same_task_200`;
  - `edited_by_the_user::should_refuse_another_title_for_a_pr_task_400`;
  - `…should_take_a_pr_tasks_own_title_and_a_description_200`.

## 2. Status sync: PR state moves the task's status

**What:**

- A new PR's task is created in `review`, or in `done` if the PR is already merged or closed.
- A merge or close moves it to `done`, and a reopen moves it back to `review`.
- While the PR's state holds, the task keeps whatever status the user picked.
- A PR closed from agentdeck moves its task to `done` at once.

- **Spec:** §8 ("New PR", "Merged or closed", Closing PRs: "its task goes to `done` at once").
- **Code:**
  - `crates/agentdeck-core/src/pr.rs` `pr_task_status(before, now)` (with unit tests);
  - `crates/agentdeckd/src/db/pull_requests.rs` `store_one` (status only when it changes; an aged-out PR is treated as finished) and `close` (moves the task to done);
  - `crates/agentdeckd/src/prsync/mod.rs` `closed`.
- **Tests:** in `crates/agentdeckd/tests/integration/prs.rs`:
  - `should_keep_the_status_the_user_picked_while_the_pr_stays_open_200`;
  - `should_move_a_pr_that_merged_out_of_the_search_to_done_200`;
  - `incremental::should_move_a_pr_merged_since_the_last_sync_to_done_without_a_fetch_200`;
  - `asked_full::should_reopen_a_pr_stored_closed_that_no_incremental_search_shows_200`;
  - `post_v1_prs_close::should_close_each_pr_and_move_its_task_to_done_200`.
- **GUI:** the "Closing N of M…" count reads tasks turning `closed` (§6 here). The Inbox hides done tasks, so a merged or closed PR leaves the Inbox (`crates/agentdeck-gui/src/inbox.rs`).

## 3. PR→workspace matching, and the user's pick

**What:**

- Sync places a new PR task in the oldest unarchived workspace whose dir holds a clone of the PR's repo with `head_ref` as a local branch. Failing that, it uses the oldest workspace holding any clone of the repo, and failing that, the Inbox.
- Only Inbox PR tasks are matched again on later syncs. The incremental sync re-matches stored Inbox PRs without asking GitHub.
- A workspace the user sets with `PATCH /v1/tasks/{id}` (the Inbox included) stamps `workspace_picked_at`, and sync never moves that task again.

- **Spec:** §8 (Its **workspace**, **The user's pick sticks**, Incremental), §4 (`PATCH /v1/tasks/{id}`).
- **Daemon:**
  - `crates/agentdeckd/src/prsync/matching.rs` (`Clones::scan`, `workspace_for`, `remote_urls`, `has_branch`, `github_repo`; unit tests in-file);
  - `crates/agentdeckd/src/git.rs` `find_repos`, shared with the dirty report;
  - `crates/agentdeckd/src/prsync/mod.rs`: `workspace_dirs`, the `Clones::scan` per sync, `store` (matching on `spawn_blocking`), `read_since` re-matching `inbox_open`, and the `Matching` error;
  - `crates/agentdeckd/src/db/pull_requests.rs`: `inbox_open`, `sync_places`, `is_picked`;
  - `crates/agentdeckd/src/db/tasks.rs` `edit` stamps `workspace_picked_at`;
  - migration `0004_pr_workspace_picked.sql`.
- **GUI:**
  - The task sheet's Workspace select (`crates/agentdeck-gui/src/task_sheet.rs`) is how a user picks.
  - `new_task.rs` mentions `workspace_picked`.
  - The Inbox's description line, "A PR lands here when no workspace has a clone of its repo.", is in `crates/agentdeck-gui/src/inbox.rs`.
- **CLI/MCP:** `agentdeck task edit --ws/--inbox` and MCP `task_update { workspace | inbox }` also pick.
- **Tests:** in `crates/agentdeckd/tests/integration/prs.rs`:
  - `post_v1_prs_sync`: `should_put_a_pr_in_the_workspace_whose_clone_has_its_branch_200`, `…whose_branch_no_clone_has_in_the_oldest_workspace_with_its_repo_200`, `should_leave_a_pr_in_the_inbox_when_no_clone_is_of_its_repo_200`, `should_move_an_inbox_pr_into_a_workspace_once_a_clone_appears_200`;
  - the same last test again under `incremental`;
  - `mod edited_by_the_user` (3 tests that keep a pick: Inbox in a full sync, Inbox in an incremental sync, a workspace).
- **Test support:** `crates/agentdeckd/tests/integration/support/git.rs` (clones for matching).

## 4. Unreachable PRs

**What:**

- A PR fetched by number that GitHub doesn't show to the token is marked `unreachable_at` instead of closed. It keeps its state and its task's status, and each full sync fetches it again.
- It is left out of incremental unsettled fetches, Inbox re-matching, closing and PR counts.
- Clients draw it dimmed with a tooltip.
- When GitHub shows it again, its task moves as a new PR's would.
- A sign-in change triggers a full sync, which is when PRs a new token can't see become unreachable.

- **Spec:** §8 (**Unreachable**, Full: another token), §11 Colours → **Unreachable PR**, Sidebar (counts), Closing PRs (never selectable).
- **Daemon:**
  - `crates/agentdeckd/src/prsync/mod.rs` (module doc, `read_full`, `store(prs, unreachable, …)`);
  - `crates/agentdeckd/src/db/pull_requests.rs` (`mark_unreachable`, `closable`, `stored_where` filters);
  - `crates/agentdeckd/src/db/tasks.rs` (the `unreachable` field);
  - `crates/agentdeckd/src/model.rs` `PullRequest.unreachable`;
  - migration `0006_pr_unreachable.sql`.
- **GUI:**
  - `crates/agentdeck-gui/src/task_row.rs` (opacity 0.5, the `UNREACHABLE` tooltip);
  - `task_list.rs` (`Row.unreachable`);
  - `overview.rs` (counts leave it out);
  - `close_prs.rs` `selectable`;
  - `inbox.rs` `mod unreachable` tests.
- **CLI:** `crates/agentdeck/src/render.rs:110` prints `, unreachable` (test `should_say_the_sign_in_cannot_see_an_unreachable_pr`).
- **Tests:** in `crates/agentdeckd/tests/integration/prs.rs`:
  - `should_mark_a_pr_github_no_longer_shows_unreachable_and_keep_its_task_200`;
  - `mod sign_in_changes`: `should_mark_the_prs_of_a_repo_the_new_token_cannot_see_unreachable_200`, `should_bring_back_a_pr_the_token_sees_again_in_review_200`, `should_not_close_an_unreachable_pr_200`;
  - `post_v1_prs_close::should_fail_a_pr_github_does_not_show_with_githubs_message_200`.
- **Penpot:** "Screens · Main" → "Inbox · unreachable PR".

## 5. Aged-out PRs and the age cutoff

**What:**

- In a full sync, a stored open PR that left GitHub's list and was last updated before the `pr_sync.max_age_days` cutoff isn't fetched. It is aged out (`aged_out_at`), and its task moves to `done` once.
- If GitHub shows it again within the cutoff, its task goes back to `review`.
- Aged-out PRs can still be closed.

- **Spec:** §8 (**Age cutoff**, **Aged out**), §2 (setting `pr_sync.max_age_days`, `AGENTDECK_PR_MAX_AGE_DAYS`), §11 Settings → GitHub → **Age cutoff**, Inbox description.
- **Daemon:**
  - `crates/agentdeckd/src/db/pull_requests.rs` (`age_out`, `is_aged_out`);
  - `crates/agentdeckd/src/prsync/mod.rs` (`older_than`, `widens`, the age-out call);
  - `crates/agentdeckd/src/config.rs` (the setting);
  - migration `0003_pr_aged_out.sql`.
- **GUI:** `crates/agentdeck-gui/src/settings.rs` `DAYS_DESCRIPTION`: "Older PRs already here move to done within the hour."
- **Tests:** in `crates/agentdeckd/tests/integration/prs.rs`:
  - `mod max_age` (6 tests, including `should_move_a_stored_pr_older_than_the_cutoff_to_done_200` and `should_move_an_aged_out_task_to_done_only_once_200`);
  - `post_v1_prs_close::should_close_an_aged_out_pr_200`;
  - `crates/agentdeckd/tests/integration/settings.rs` `mod pr_sync_cutoff`.

## 6. Bulk close (Closing PRs)

**What:**

- The **Select** toggle, checkboxes, group-header and column-header select-all, the selection bar ("N selected · Close PRs · Cancel"), the confirm dialog with its per-repo list, "Closing N of M…", the result toasts and the failed-row marks with tooltips.
- A single **Close PR** in a row's `…` menu.
- All of it works in the Inbox, the Tasks view and the right panel.
- Rows are task rows, and selection and results are by `TaskId`.
- Selectable rows are PR tasks stored `open`, aged-out ones included (they show under Show done), and never unreachable ones.

- **Spec:** §11 **Closing PRs** (and Labels, Dialogs, Table width), §8 **Closing PRs**, §4 `POST /v1/prs/close`, §10 `pr close`.
- **GUI:**
  - `crates/agentdeck-gui/src/close_prs.rs` (`ClosePrs`, `select_toggle`, `row_parts`, `selectable`; 21 tests);
  - its hosts `inbox.rs`, `tasks.rs`, `right_panel.rs`;
  - `task_row.rs` (`SelectCell`, `group_header`);
  - `api.rs` `close_prs(tasks: Vec<TaskId>)`.
- **Daemon:**
  - `crates/agentdeckd/src/api/prs.rs` `prs_close`;
  - `crates/agentdeckd/src/prsync/mod.rs` `close` and `closed`;
  - `crates/agentdeckd/src/db/pull_requests.rs` `closable` and `close`;
  - `model.rs` `PrClose`, `PrCloseResult(s)`.
- **CLI:**
  - `crates/agentdeck/src/commands.rs` `close_prs` and `task_ids` (resolves short task ids);
  - `crates/agentdeck/src/render.rs` `close_prompt` and `close_results`.
- **Tests:**
  - `crates/agentdeckd/tests/integration/prs.rs` `mod post_v1_prs_close` (13 tests);
  - `crates/agentdeck/tests/integration/pr.rs` `mod pr_close` (6), plus `support/github.rs`.
- **Penpot:**
  - "Inbox · closing", "Inbox · partial failure";
  - `agentdeck/ClosePrsList`, `agentdeck/CheckboxIndeterminate`.

## 7. Opening PRs on GitHub

**What:**

- In a PR task row's `…` menu, **Open on GitHub**.
- On the task sheet, the PR line's "<repo> #<n>" link.
- Both open `pr.url` with the OS's handler, but only for `https://github.com` URLs.

- **Spec:** §11 **Opening PRs**.
- **Code:**
  - `crates/agentdeck-gui/src/open_pr.rs`;
  - `task_row.rs` / `close_prs.rs` `row_parts` (the menu);
  - `task_sheet.rs` (the link).
- **Penpot:** "Task sheet · PR link", "· hover"; "Inbox · row menu · open PR", "Tasks · row menu · merged PR".

## 8. Sync now, sync labels and the logged-out banner

**What:**

- **Sync now** is a button in the Inbox header only. It sends `POST /v1/prs/sync?full=true`.
- The Inbox header has a "Synced …" label, and the sidebar footer has a "PRs synced …" label. Both read "Not signed in" while logged out.
- The logged-out banner sits between the Inbox's header and its columns. It reads "Not signed in to GitHub, so PRs don't sync." or "Synced as <login>. Sign in to refresh." (from `synced_as`), and its **Open Settings** link opens Settings at GitHub.
- The **older daemon** banner over the whole window reuses this banner's styling.
- A sync asked for in the Inbox emits `Synced`, and the shell makes the sidebar re-read.

- **Spec:** §11 **Inbox** → **Logged out**, Sidebar (footer), **Older daemon** ("the Inbox's logged-out banner … with … Restart"), §8 (Asked for, `synced_as`), §4 `POST /v1/prs/sync`, `GET /v1/prs/status`.
- **GUI:**
  - `crates/agentdeck-gui/src/inbox.rs`: `LOGGED_OUT`, the `synced_as` text (line 69), `sync_now` (line 156), the header (line 258), `Synced`, `OpenSettings`.
  - `crates/agentdeck-gui/src/shell.rs`:
    - subscribes to `Synced` and `OpenSettings` (around lines 451 and 505);
    - `older_daemon_banner` (line 986), whose doc comment says it is the Inbox's gh banner.
  - `crates/agentdeck-gui/src/overview.rs` (`PrSync` label, `short_label`) and `sidebar.rs` (footer).
  - `api.rs` `sync_prs(full)`.
- **Daemon:** `crates/agentdeckd/src/api/prs.rs`, `prsync/mod.rs`, and `db/pull_requests.rs` (`synced_as`, `set_synced_as`).
- **CLI/MCP:** `agentdeck pr sync | status` (`commands.rs` `pr`, `render.rs` `pr_status`) and MCP `prs_status`.
- **Tests:**
  - `crates/agentdeckd/tests/integration/prs.rs` `mod get_v1_prs_status`, `sign_in_changes::should_keep_every_pr_and_the_login_they_synced_as_once_signed_out_200`;
  - `crates/agentdeck/tests/integration/pr.rs` `mod pr_status`, `mod pr_sync`;
  - GUI: `inbox.rs` (69 tests, which include the banner and Sync now), `shell.rs` `should_show_the_banner_while_the_daemon_predates_the_app`.
- **Penpot:** "Inbox · logged out · synced before"; "Screens · Daemon" → "Older daemon".

## 9. The Inbox view

**What:** a top-level view of tasks with no workspace, which are mostly PRs. Done tasks are left out.

- **Contents:** a status picker per row, two description lines (where PRs land, and the age cutoff with a Settings link), Select, the sync label, Sync now and the logged-out banner.
- **What it hosts:** Bulk close, Opening PRs, the unseen mark and the row menu (Edit task, Send to agent, Open on GitHub, Copy task id, Close PR).
- **Opens:** the task sheet.
- **The sidebar's fallback selection:**
  - no workspaces selects the Inbox (`sidebar.rs:158`);
  - deleting the last workspace selects the Inbox (`shell.rs` test `should_select_the_inbox_once_the_last_workspace_is_deleted`).
- **Placement:** tasks reach it when they are created with no workspace, when PR sync can't match them, through `PATCH workspace_id: null`, or when their workspace is deleted (FK `ON DELETE SET NULL`).

- **Spec:** §11 **Inbox**, Table width ("Inbox · 1200"), Scrollbars, Sidebar; §4 (`inbox` filter on `GET /v1/tasks`).
- **GUI:**
  - `crates/agentdeck-gui/src/inbox.rs` (1807 lines, 69 tests);
  - `task_row.rs` (`Table::Inbox` columns: Checks drops first, then Review);
  - `sidebar.rs` (`Selection::Inbox`, `Lead::Icon(IconName::Inbox)`);
  - `shell.rs` (owns the `InboxView` and routes its events).
- **Daemon:** `TaskFilter.inbox` (`crates/agentdeckd/src/model.rs`, `db/tasks.rs` `list`).
- **Penpot:** "Screens · Main" → Inbox, "Inbox · 1200", "Inbox · row menu · open PR", plus those in §4, §6 and §8 here.

## 10. The Tasks view

**What:** a top-level view of every task, grouped by status, with foldable groups.

- **Header:** a filter (on title, PR or workspace), Select, the Show done switch and **New task** (which opens the dialog on the Inbox).
- **Columns:** each row has a status picker. The columns are Agent (the state of the panes linked to the task), Checks, Review and Workspace.
- **Click:** opens the task sheet.

- **Spec:** §11 **Tasks view**, Table width (Tasks drops Agent, then Checks), Window (the minimum width is derived from it, §26 here).
- **GUI:**
  - `crates/agentdeck-gui/src/tasks.rs` (1094 lines, 32 tests);
  - `task_list.rs` (`Row`, `rows`; 12 tests);
  - `task_row.rs` (`Table::Tasks`, `Columns`; 15 tests);
  - `sidebar.rs` (`Selection::Tasks`);
  - `shell.rs`;
  - `api.rs` `set_task_status`.
- **Penpot:** "Tasks · 1200", "Tasks · PR states", "Tasks · row menu · edit task", "Tasks · row menu · send to agent", "Tasks · row menu · merged PR".

## 11. The right panel

**What:** the workspace's side panel, a single virtualised list.

- **Instructions** come first, newest first. They include:
  - an "Add an instruction for agents here…" field and the header's `+`;
  - an `InstructionItem` card per instruction, with a 4-line clamp and Show more / Show less;
  - a `…` menu with **Send to agent**, **Edit** and **Delete**.
- **Tasks & PRs** come below a separator: grouped by status, "Show N done", Select with the selection bar pinned to the bottom, and `+`, which opens New task on the workspace.

- **Spec:** §11 **Right panel** (and Scrollbars, Colours: cached view, virtualised list).
- **GUI:**
  - `crates/agentdeck-gui/src/right_panel.rs` (2368 lines, 58 tests);
  - `instruction_list.rs` (4 tests);
  - `keymap.rs` `INSTRUCTION_EDITOR` (Ctrl+Enter saves, Escape cancels);
  - `top_bar.rs` / `keymap.rs` `ToggleRightPanel`.
- **Penpot:**
  - "Right panel · long instruction", "· expanded";
  - `agentdeck/InstructionItem`;
  - "Send to agent · hosted agent";
  - "Main window" `RightPanel`, "Main window · scrolled".

## 12. The task sheet

**What:** a right-side `Sheet` on one task, opened from any task row or from a PR notification.

- **Header:** the title, over "<Agent> is working on it." or "No agent on it.".
- **`…` menu:** Send to agent, then Copy task id.
- **Body:**
  - the status picker;
  - the Workspace select ("Inbox (no workspace)" plus the unarchived workspaces);
  - **Start agent** (§16 here);
  - the description, which a PR task keeps as a local note;
  - a PR task's PR line and link;
  - the "Comments" thread.
- **Footer:** the comment composer.
- **Editing in place:** the title (not for a PR task, which shows "The title comes from GitHub."), the description, and the workspace. Each saves with `PATCH /v1/tasks/{id}`.
- **Seen:** opening the sheet marks agent comments seen.

- **Spec:** §11 **Task sheet**, **New from the agent**, Opening PRs; §9; §4.
- **GUI:**
  - `crates/agentdeck-gui/src/task_sheet.rs` (2450 lines, 67 tests; `EditTask`, `OpenTask`, `StartAgentOn`, `agent_name`, which `workspace.rs` and `notifications.rs` also use);
  - `keymap.rs` `TASK_EDITOR`, `COMMENT_COMPOSER`;
  - `api.rs` `edit_task`, `comments`, `new_comment`, `mark_comments_seen`;
  - `shell.rs` `open_task`, `edit_task`.
- **Penpot:**
  - "Screens · Task sheet" (the whole page): "Task sheet · edit title", "· title error", "· edit description", "· workspace select", "· PR task", "· no agent", "· send to agent", "· PR link", "· PR link · hover";
  - `agentdeck/TaskSheetBody`, `agentdeck/CommentComposer`.

## 13. New task dialog

**What:** a Title, a Description, a Workspace select (the unarchived workspaces plus "Inbox (no workspace)") and **Send to**:

- None;
- New agent, while a workspace is picked;
- after a separator, the live agent panes Send to agent offers.

It makes a `todo` task with `POST /v1/tasks`, then opens New agent on it or links the picked pane. The Tasks view opens it on the Inbox, and the right panel's `+` opens it on the workspace.

- **Spec:** §11 Dialogs → **New task**.
- **GUI:**
  - `crates/agentdeck-gui/src/new_task.rs` (810 lines, 20 tests; `NewTask`, `NewTaskDialog`);
  - `api.rs` `new_task`;
  - `shell.rs` `new_task`.
- **Penpot:** "Screens · Dialogs" → "New task · send to".

## 14. Send to agent

There are three distinct "send" features.

### a. A task to an agent pane

**What:**

- **Where it appears:** a submenu in every non-done task row's `…` menu and at the top of the task sheet's menu.
- **What it lists:** the live agent panes the task can go to, as "<Agent> · <workspace> / <tab>". For a task in a workspace these are that workspace's panes; for an Inbox task, every workspace's.
- **What picking does:** an Inbox task is first moved to the pane's workspace with `PATCH /v1/tasks/{id}`. Then the pane is linked with `PATCH /v1/panes/{id}`, and the daemon types the task brief and the waiting comments into the pane (§15–§17 here).

- **Spec:** §11 Opening PRs → **Send to agent**, §9 ("Sending a task to an agent").
- **GUI:**
  - `crates/agentdeck-gui/src/send_task.rs` (351 lines, 9 tests);
  - row menus via `close_prs.rs` `row_parts` and `task_row.rs`;
  - `task_sheet.rs`.
- **Penpot:** "Tasks · row menu · send to agent", "Task sheet · send to agent".

### b. New task's **Send to**

See §13 here. It reuses `send_task`.

### c. An instruction to an agent pane

**What:**

- **Where it appears:** an instruction card's `…` menu.
- **What it lists:** every live agent pane in the workspace, as "<Agent> · <linked task title, else tab title>".
- **What picking does:** it submits the instruction text at once (`input` with `submit: true`, `when: now`), as `agentdeck agent send --now` does. The pane label reads the pane's linked task.

- **Spec:** §9 (Workspace instructions → Send to agent), §11 Right panel → **Send to agent**.
- **Code:**
  - `crates/agentdeck-client/src/instructions.rs` `send_instruction`;
  - `crates/agentdeck-gui/src/instruction_list.rs`, `right_panel.rs`;
  - `api.rs` `send_instruction`.
- **Penpot:** "Screens · Dialogs" → "Send to agent · hosted agent".

## 15. Pane ↔ task links

**What:** each pane links to at most one task in its own workspace (`panes.task_id`).

- **Pane header:** shows the linked task's title (the `Task` part, a 14px `ListTodo`).
- **Pane `…` menu:**
  - **Link to task**, shown while an agent runs or a shell hosts one;
  - **Unlink task**, shown on any linked pane.
- **The Link to task dialog:**
  - a Task select of None plus the workspace's tasks that aren't done;
  - a pane linked to a done task gets it preset as "<title> (done)", offered only while it stays picked;
  - linking a done task is refused (`invalid_task`).
- **Moving a task** to another workspace or to the Inbox unlinks panes in other workspaces.
- **Deleting a task** unlinks its panes.
- **A link survives:** a task going done, an agent exiting, a respawn (Restart agent, Open shell) and a resume.
- **Environment:** `AGENTDECK_TASK` is set in a pane linked when it is spawned. The CLI and MCP read the pane's current link instead.
- **The Tasks view's Agent column** and the task sheet's "<Agent> is working on it." read links in the other direction.

- **Spec:**
  - §3 ("A pane links to at most one task");
  - §4 (`PATCH /v1/panes/{id}`, `POST /v1/panes` `task_id`, respawn and stop keep "its task link");
  - §6 (Environment, `AGENTDECK_TASK`);
  - §9;
  - §10 (`agent link`, `agent ls` TASK column, `agent new --task`);
  - §11 Workspace view (**Link to task**, **Unlink task**, Agent exited, Hosted agent).
- **Daemon:**
  - `crates/agentdeckd/src/api/panes.rs`: `pane_update` sends `changed` for the pane and its old and new task, then `deliver_on_link`. `pane_create` passes `task_id` into the launch.
  - `crates/agentdeckd/src/db/panes.rs`: `set_task` checks the workspace and refuses a done task; `agents_on_task`.
  - `crates/agentdeckd/src/db/tasks.rs` `edit`: unlinks on a move.
  - `crates/agentdeckd/src/api/tasks.rs`: unlink on move (line 98), `terminals.unlink_task` on delete (line 115).
  - `crates/agentdeckd/src/terminal/mod.rs`: `link`, `unlink_task`.
  - `crates/agentdeckd/src/terminal/pane.rs`: `Launch.task`, `AGENTDECK_TASK` via `agents::TASK_VAR`, `link`, `linked`, and the queue filtered by task.
  - `crates/agentdeckd/src/agents/resume.rs` and `agents/restart.rs`: carry `pane.task_id`.
  - `crates/agentdeckd/src/agents/registration.rs:433`: Codex `env_vars` lists `AGENTDECK_TASK`.
- **GUI:**
  - `crates/agentdeck-gui/src/link_task.rs` (642 lines, 20 tests; "(done)" at lines 90, 166, 175);
  - `pane.rs` (`PaneRequest::LinkTask` / `UnlinkTask`, `menu_items`; 21 tests);
  - `pane_header.rs` (`task`);
  - `workspace.rs` (`LinkPane`, `unlink_task`; reads the linked tasks' titles);
  - `shell.rs` `open_link_task`;
  - `overview.rs` `PaneState.task`;
  - `api.rs` `link_task`.
- **Client:** `crates/agentdeck-client/src/panes.rs` `link_task`.
- **CLI:**
  - `agentdeck agent link <pane> (--task T | --none)` (`crates/agentdeck/src/cli.rs` `AgentCommand::Link`, `commands.rs` `agent`);
  - the `agent ls` TASK column and `task_title` (`commands.rs` `agent_ls`, `render.rs` `panes`).
- **Tests:**
  - `crates/agentdeckd/tests/integration/panes.rs` `mod patch_v1_panes_id` (8 tests, including `should_reject_a_done_task_400` and `should_reject_a_task_in_another_workspace_or_the_inbox_400`);
  - `agents.rs` `mod patch_v1_panes_id`, `mod task_delivery`;
  - `hosted.rs` `mod unlink`;
  - `panes_client.rs` `mod link_task`;
  - `agents_mcp.rs` (`ENV_VARS`);
  - `crates/agentdeck/tests/integration/agent.rs` `mod agent_link` (3), `agent_ls`;
  - GUI: `workspace.rs` tests (link/unlink asks, `should_show_the_linked_tasks_title`), `shell.rs` `should_open_the_link_dialog_on_the_pane_the_workspace_asks_for`.
- **Penpot:** "Screens · Dialogs" → "Link to task"; "Screens · Main" → "Pane · menu", "Pane · agent exited", "Pane · hosted agent".

## 16. Launch prompt, task brief, New agent and Start agent

**What:**

- **The launch prompt:** `claude` and `codex` panes get one, built from:
  - the linked task's brief (`Task: <title>`, `Task id: <id>`, and a report-back line naming the MCP tools `task_comment` / `task_update` and `agentdeck comment add --task`);
  - the task's body;
  - "Instructions for this workspace:" with every instruction;
  - "Comments since your last session:" with the task's undelivered user comments, which are then marked delivered.
- **When it is built:** for a new pane, a respawn's fresh start, and a resume that fails at once. Without a task or instructions there is no prompt argument.
- **The task brief alone** is typed into an agent linked later, or into a shell pane's agent when it starts hosting.
- **New agent dialog:** has an optional **task** select (tasks that aren't done, or a done one once picked). Its description says what the prompt will carry, including how many instructions.
- **Start agent:** on the task sheet; it opens New agent on the task's workspace with the task picked. It is disabled for an Inbox task ("Move this task to a workspace to start an agent on it.").

- **Spec:** §6 (**Launch prompt**, Resume, Trust dialog), §9 (The task on linking), §11 Task sheet → **Start agent**, Dialogs (new agent: agent, cwd, task), §10 (`agent new --task`).
- **Daemon:**
  - `crates/agentdeckd/src/agents/mod.rs` (`task_brief`, `launch_prompt`, `TASK_VAR`);
  - `crates/agentdeckd/src/api/panes.rs` `pane_create` (lines 116–170);
  - `crates/agentdeckd/src/agents/restart.rs` (lines 117–179);
  - `agents/delivery.rs` `for_launch_prompt`, `launch_failed`.
- **GUI:**
  - `crates/agentdeck-gui/src/new_agent.rs` (647 lines, 12 tests, including `should_count_the_workspaces_instructions_for_the_prompt` and `should_say_the_task_and_how_many_instructions`);
  - `task_sheet.rs` `StartAgentOn`;
  - `shell.rs` `open_new_agent(workspace, Some(task))`;
  - `api.rs` `launch_agent`.
- **CLI:**
  - `crates/agentdeck/src/launch.rs` (`task`; `unknown_task` → "see `agentdeck task ls`");
  - `cli.rs` `AgentCommand::New { task }`.
- **Tests:**
  - in `crates/agentdeckd/tests/integration/agents.rs`:
    - `post_v1_panes::claude::should_carry_the_task_instructions_and_new_comments_in_the_launch_prompt_201`;
    - `…should_leave_out_delivered_and_agent_comments_but_keep_instructions_201`;
    - `…should_start_without_a_prompt_with_no_task_or_instructions_201`;
    - `…should_reject_an_unknown_task_400`;
    - `resume_after_restart::should_give_a_resumed_agent_its_task`;
    - `…should_start_a_claude_that_cannot_find_its_session_afresh_on_its_task`;
    - `post_v1_panes_id_respawn::resume_that_fails_at_once::should_start_{claude,codex}_afresh_on_its_task_200`;
  - `crates/agentdeck/tests/integration/agent.rs` `agent_new::should_prompt_with_the_task_instructions_and_comments_and_mark_them_delivered`, `should_link_a_task_named_by_its_short_id`, `should_name_an_unknown_task`;
  - GUI `shell.rs` tests on Start agent.
- **Penpot:** "Task sheet · no agent".

## 17. Comment delivery to agents

**What:**

- **Where a user's comment goes:** into the newest live agent pane linked to the task, typed once that pane is `idle` or `done` and never while it is `blocked`. With no such pane, it waits for the next launch prompt.
- **Linking or hosting:** a pane that is linked to the task, or that starts hosting an agent while linked, gets the task brief first, then the undelivered comments.
- **Exactly once:** a comment reaches one agent at most once, held for a single pane (`Claims`).
- **A held comment moves on** when its pane exits, its hosted agent leaves, or it is relinked or unlinked.
- **Delivered:** delivery stamps `delivered_at` and `delivered_to_pane`.

- **Spec:** §9 (**Task comments**, The task on linking), §7 (Hosted agents: a waiting comment is dropped on a hosting change), §4 (`input` submit semantics).
- **Daemon:**
  - `crates/agentdeckd/src/agents/delivery.rs` (522 lines: `Claims`, `deliver_to_task_pane`, `deliver_on_link`, `deliver_waiting`, `deliver_on_hosting`, `pass_on`, `mark_delivered`);
  - `crates/agentdeckd/src/api/comments.rs` `comment_create` (calls `deliver_to_task_pane`);
  - `crates/agentdeckd/src/terminal/pane.rs` (the `When::Prompt` queue, typed only while linked to the task);
  - `crates/agentdeckd/src/terminal/mod.rs:239` (hosting change → delivery);
  - `crates/agentdeckd/src/api/mod.rs` (`claims` in the server context).
- **Tests:**
  - `crates/agentdeckd/tests/integration/agents.rs` `mod comment_delivery` (12) and `mod task_delivery` (6);
  - `hosted.rs` `mod comment_delivery` (9);
  - `support/agents.rs` (stub `claude`/`codex` that read input as TUIs do), `support/terminal.rs`.

## 18. Agent comments and the unseen mark

**What:**

- **Agent comments:** written from inside a pane (`pane` on create, from `$AGENTDECK_PANE`) through the CLI or MCP, and authored as that pane's agent or the agent it hosts. They are never delivered.
- **Unseen:** each is unseen until the user opens the task sheet (`POST /v1/tasks/{id}/comments/seen`). `Task.unseen_agent_comments` counts them.
- **The mark:** task rows in the Tasks view, the Inbox and the right panel show a 14px `MessageSquareDot` while that count is above 0.
- **Composer and thread:** the task sheet's thread and composer show "Delivered to <Agent>" / "Waiting for the agent" on the user's comments.
- **Stale `$AGENTDECK_PANE` errors:** `stale_pane` explains codex's background server.

- **Spec:** §9 (**Agent comments**), §11 **New from the agent**, Task sheet; §10 (`comment add`, stale `$AGENTDECK_PANE`).
- **Daemon:**
  - `crates/agentdeckd/src/api/comments.rs` (`comment_seen`);
  - `db/comments.rs` (`mark_seen`);
  - `db/tasks.rs` (the unseen count);
  - `model.rs` `Comment`, `NewComment { pane }`.
- **GUI:**
  - `crates/agentdeck-gui/src/task_list.rs` `Row.unseen`;
  - `task_row.rs`;
  - `task_sheet.rs`;
  - `inbox.rs` tests (`unseen …`).
- **CLI/MCP:** `crates/agentdeck/src/commands.rs` (`comment`, `task_or_callers`, `caller_pane`, `stale_pane`), `render.rs` (`comments`, `comment_created`), and `mcp.rs` `task_comment`, `task_comments`.
- **Tests:**
  - `crates/agentdeckd/tests/integration/comments.rs` (16);
  - `hosted.rs` `mod authorship`;
  - `agents.rs` `should_let_the_agent_comment_on_its_task_through_its_mcp_server_201`;
  - `crates/agentdeck/tests/integration/comment.rs` (16).

## 19. Workspace Instructions

**What:**

- **Content and lifecycle:** standing text for every agent in a workspace. They are never consumed, can be edited, and are deleted with their workspace.
- **Where they show up:**
  - every launch prompt in the workspace (§16 here);
  - the right panel's list and editor (§11 here);
  - Send to agent (§14c here);
  - the delete dialog's count and the archive toast (§25 here);
  - the CLI's `instruction` commands, and the MCP `instructions_list` tool.

- **Spec:** §9 (**Workspace instructions**), §3, §4, §6, §10, §11 (Right panel, Delete workspace).
- **Daemon:** `crates/agentdeckd/src/api/instructions.rs`, `db/instructions.rs`, `model.rs` (`Instruction`, `NewInstruction`, `InstructionPatch`), and `agents/mod.rs` `launch_prompt`.
- **GUI:**
  - `crates/agentdeck-gui/src/right_panel.rs`, `instruction_list.rs`;
  - `api.rs` (`new_instruction`, `edit_instruction`, `delete_instruction`, `send_instruction`);
  - `overview.rs` (`Snapshot.instructions`);
  - `new_agent.rs` (the instruction count);
  - `delete_workspace.rs`;
  - `keymap.rs` `INSTRUCTION_EDITOR`.
- **CLI:**
  - `agentdeck instruction ls [--ws] | add <body> [--ws] | edit <id> <body> | rm <id>` (`crates/agentdeck/src/cli.rs` `InstructionCommand`);
  - `commands.rs` (`instruction`, `instruction_workspace`, `caller_workspace`);
  - `render.rs` (`instructions`, `instruction_saved`).
- **Tests:**
  - `crates/agentdeckd/tests/integration/instructions.rs` (9), `instructions_client.rs` (3);
  - `crates/agentdeck/tests/integration/instruction.rs` (8);
  - `mcp.rs` `should_list_the_callers_workspace_instructions`.
- **Penpot:** `agentdeck/InstructionItem`, "Right panel · long instruction", "· expanded", "Send to agent · hosted agent".

## 20. MCP tools

**What:** `agentdeck mcp` is an rmcp stdio server. `claude` and `codex` panes get it from their launch flags, and agents the user starts in a shell pane get it from `agentdeck mcp install` or Settings → Agents.

- **Its instructions string:** "agentdeck's tasks, task comments, workspace instructions, workspaces and PR status. Tools default to the workspace and pane you run in, and to the task that pane is linked to."
- **What defaults depend on:** the caller's pane and workspace (`$AGENTDECK_PANE`, `$AGENTDECK_WORKSPACE`), and the task the pane is linked to now.

| Tool | Depends on |
|---|---|
| `tasks_list { workspace?, inbox?, all_workspaces? }` | tasks, Inbox |
| `task_get { id? }` | tasks (with `pr`), the pane's link |
| `task_create` | tasks |
| `task_update { id?, title?, body?, status?, workspace? \| inbox? }` | tasks, Inbox, the user's pick (§3 here) |
| `task_comments { task? }` | comments, the pane's link |
| `task_comment { task?, body }` | comments, the pane's link; fails outside a pane |
| `instructions_list { workspace? }` | instructions |
| `workspaces_list` | workspaces only |
| `prs_status` | PR sync status only (`GET /v1/prs/status`) |

No tool closes PRs.

- **Spec:** §10 **MCP tools** and the bullets under it; §6 (MCP server registration, Codex `env_vars` incl. `AGENTDECK_TASK`).
- **Code:**
  - `crates/agentdeck/src/mcp.rs` (555 lines; `serve`, `scope`, the tools at lines 247–455, `with_instructions` at line 466);
  - `commands.rs` helpers shared with the CLI (`task_or_callers`, `caller_pane`, `caller_workspace`, `stale_pane`);
  - `crates/agentdeckd/src/agents/mod.rs` (`mcp_flags`) and `agents/registration.rs` (user-config install; `GET/PUT /v1/agents/mcp`);
  - GUI `crates/agentdeck-gui/src/agents_section.rs` (Settings → Agents).
- **Tests:**
  - `crates/agentdeck/tests/integration/mcp.rs` (17: `in_a_workspace`, `in_a_linked_pane`, `tool_errors`), `support/mcp.rs`, `mcp_install.rs`;
  - `crates/agentdeckd/tests/integration/agents_mcp.rs` (10);
  - `agents.rs` `should_let_the_agent_comment_on_its_task_through_its_mcp_server_201`.

## 21. CLI commands

Defined in `crates/agentdeck/src/cli.rs`, run in `commands.rs`, printed by `render.rs`, with short ids resolved in `resolve.rs`.

| Command | Depends on | Code |
|---|---|---|
| `task ls [--status S] [--ws WS] [--inbox]`, `add <title> [--ws] [--body]`, `set <id> --status S`, `edit <id> [--title] [--body] [--ws \| --inbox]`, `show <id>` | tasks, Inbox, PR tasks (`show` prints `pr:`), the user's pick | `TaskCommand`, `commands.rs` `task`, `render.rs` `tasks`, `task`, `pr_label` |
| `comment ls [--task T]`, `add <body> [--task T]` | comments; defaults to the caller pane's linked task; authored as the pane's agent | `CommentCommand`, `commands.rs` `comment`, `task_or_callers` |
| `instruction ls [--ws] \| add \| edit \| rm` | instructions; defaults to `$AGENTDECK_WORKSPACE` | `InstructionCommand`, `commands.rs` `instruction` |
| `agent ls [--ws]` | prints a TASK column (task title, else id); `--json` adds `task_title`; reads `GET /v1/tasks` | `commands.rs` `agent_ls`, `render.rs` `panes` |
| `agent new … [--task T]` | the launch prompt (§16 here) | `launch.rs`, `commands.rs` `agent` |
| `agent link <pane> (--task T \| --none)` | pane links | `commands.rs` `agent` |
| `agent send <pane> <text> [--now]` | none directly; `--now` is what instruction Send to agent mirrors | `commands.rs` `agent` |
| `pr sync \| status` | PR sync (text says "Sync PR tasks with GitHub now") | `commands.rs` `pr`, `render.rs` `pr_status` |
| `pr close <id>… [--yes]` | **task ids** (whole or short) of PR tasks | `commands.rs` `close_prs`, `task_ids`; `render.rs` `close_prompt`, `close_results` |
| `mcp`, `mcp install [--yes]` | the MCP server (§20 here) | `mcp.rs`, `commands.rs` `mcp_install` |

- **Short ids:** short task ids resolve against `GET /v1/tasks` (`commands.rs` `task_id`, `optional_task`; `resolve.rs` `by_short_id("task", …)`). `ambiguous_id` lists each matching task with its title.
- **Unaffected commands:** `ws …`, `config …`, `daemon restart` and `tail` don't depend on tasks.
- **Spec:** §10 (the command list and every bullet).
- **Tests:** in `crates/agentdeck/tests/integration/`:
  - `task.rs` (17), `comment.rs` (16), `instruction.rs` (8);
  - `agent.rs` (`agent_ls`, `agent_link`, `agent_new` task tests);
  - `pr.rs` (10), `mcp.rs` (17);
  - unit tests in `resolve.rs`, `render.rs`, `launch.rs`.

## 22. Settings

**What:** no setting is about tasks, comments or instructions. These ones touch PRs and the task model:

- **`pr_sync.max_age_days`** (§2), with the `AGENTDECK_PR_MAX_AGE_DAYS` override. Its description says older PRs "move to done" (§5 here), and widening it starts a full sync.
- **Settings → GitHub:**
  - the account, Sign in, Paste token and the client ID;
  - the sign-in dialog's text "let agentdeck sync and close your PRs";
  - the scope note "PR sync works; closing PRs needs repo".
- **Settings → Agents:** "Add agentdeck to Claude Code and Codex" installs the task/comment/instructions MCP server into the agents' own config.
- **Reached from the Inbox:** Settings is opened from the Inbox's description link and the logged-out banner (`OpenSettings`, `settings.scroll_to_github`).

- **Spec:** §2 (Settings, Environment overrides), §8, §11 **Settings** (GitHub, Age cutoff, Agents).
- **Code:**
  - `crates/agentdeckd/src/config.rs`, `api/settings.rs`;
  - `crates/agentdeck-gui/src/settings.rs`, `github/mod.rs`, `github/sign_in.rs`, `github/paste_token.rs`, `agents_section.rs`;
  - the CLI `config …` (`commands.rs` `config`).
- **Tests:** `crates/agentdeckd/tests/integration/settings.rs` `mod pr_sync_cutoff`; GUI `settings.rs`, `github/mod.rs`, `agents_section.rs`.

## 23. Notifications

**What:**

- **Panes:** "<Agent> is blocked / done", with the body "<linked task title, else tab title> · <workspace>".
- **PRs:**
  - **Triggers:** checks turning `failure`, or a review turning `approved` or `changes_requested`. Each alert is tagged `task:<id>`.
  - **Batching:** changes are held while a sync runs. More than 3 make one "N PRs changed" alert, which opens the **Tasks view**.
  - **Click:** opens the **task sheet** on the PR task.
  - **Withdrawn** once that task sheet or the Tasks view shows.
- **Muting:** goes by the task's workspace. An Inbox PR has no workspace, so it always notifies.

- **Spec:** §11 **Notifications** (One per tag `task:<id>`, Withdrawn, PRs, Muted, Click), §5 (Resync compares reads).
- **GUI:**
  - `crates/agentdeck-gui/src/notifications.rs` (700 lines, 22 tests; `Target::{Pane, Task(TaskId), Tasks}`, `muted(snapshot, Option<WorkspaceId>)`, `pane_alert` reading the task title, `PrChange.task`);
  - `shell.rs`:
    - `notify`, `withdraw_seen` (a `Target::Task` is in view while the task sheet shows it, `Target::Tasks` while the Tasks view does);
    - the click opens a task sheet or the Tasks view (lines 747–800);
    - tests `should_withdraw_a_prs_notification_once_its_task_sheet_shows`, `should_open_the_task_sheet_for_a_pr`;
  - `sidebar.rs` (alerts on resync, test `should_alert_on_resync_for_a_pr_whose_checks_failed_while_events_were_missed`).
- **`crates/agentdeck-notify`:** task-agnostic (tags are opaque strings).

## 24. Sidebar

**What:**

- **Entries:** **Inbox** and **Tasks** entries sit at the top, under the brand. The Inbox shows its open-PR count.
- **Workspace rows:** each shows its open-PR count, which is the PR tasks not done and not unreachable in that workspace.
- **Footer:** says when PRs last synced, or "Not signed in".
- **Selection:** falls back to the Inbox when there are no workspaces.

- **Spec:** §11 **Sidebar**, Colours (PR state: the sidebar keeps `GitPullRequest`), Unreachable PR.
- **GUI:**
  - `crates/agentdeck-gui/src/overview.rs` (`Counts { prs }`, `Overview.inbox`, `overview()`, `PrSync`; 24 tests including `should_count_pr_tasks_that_are_not_done`, `should_count_the_inbox_from_tasks_with_no_workspace`, `should_leave_out_prs_the_sign_in_cannot_see`);
  - `sidebar.rs` (`Selection::{Inbox, Tasks}`, rows at lines 516–529);
  - `count.rs` (the `agentdeck / Count` part).
- **Snapshot:** `crates/agentdeck-gui/src/overview.rs` `Snapshot { workspaces, panes, tasks, instructions, pr_sync }`, read by `api.rs` `snapshot()`. Every task list, the right panel, the dialogs and notifications read from it.

## 25. Workspace lifecycle

**What:**

- **Archive:** the toast reads "Its folder, tasks and instructions are kept." (`crates/agentdeck-gui/src/shell.rs:894`). The daemon doc says the same (`crates/agentdeckd/src/api/workspaces.rs:125`).
- **Delete:**
  - The dialog says "Its N tasks move to the Inbox. Its N instructions are deleted." (`crates/agentdeck-gui/src/delete_workspace.rs`, `tasks` and `instructions` counts from the snapshot; 26 tests).
  - The daemon deletes the row. Tasks move to the Inbox via `tasks.workspace_id … ON DELETE SET NULL`, and instructions cascade (`crates/agentdeckd/src/db/workspaces.rs:155`, `api/workspaces.rs:235`).
- **Archived workspaces** are left out of New task's and the task sheet's Workspace selects, and PATCHing a task into one is `unknown_workspace`.
- **PR matching** skips archived workspaces (`prsync/mod.rs` `workspace_dirs`).

- **Spec:** §11 Workspace view (**Archive workspace**), Dialogs (**Delete workspace**), §4 (`DELETE /v1/workspaces/{id}`, archive), §8 (matching).
- **Tests:** `crates/agentdeckd/tests/integration/workspaces.rs` `delete_v1_workspaces_id::should_move_its_tasks_to_the_inbox_and_drop_its_instructions_204`.
- **Penpot:** "Delete workspace · panes running".

## 26. Layout and chrome coupled to the task lists

- **Window minimum width:** 1200 is derived from the Tasks view's narrowest table in Select mode (888 + sidebar + padding = 1197). Spec §11 **Window**; `crates/agentdeck-gui/src/window_state.rs` `MIN_SIZE` and its test `should_fit_the_narrowest_tasks_table_beside_the_sidebar`.
- **Table widths:** the Tasks and Inbox column-dropping rules. Spec §11 **Table width**; `crates/agentdeck-gui/src/task_row.rs` (`Table`, `Columns`).
- **Scrollbars:** the placement rules name the Tasks view, the Inbox and the right panel. Spec §11 **Scrollbars**; `crates/agentdeck-gui/src/scrollbar.rs`.
- **Cached views and virtual lists:** the right panel and the Tasks/Inbox lists. Spec §11 Colours (the cached-view bullets); `crates/agentdeck-gui/src/rereads.rs` names a PR sync's burst of changes.
- **Keymap contexts:** `TASK_EDITOR`, `COMMENT_COMPOSER` and `INSTRUCTION_EDITOR` (`crates/agentdeck-gui/src/keymap.rs`). The dialog key context covers "New agent, Link to task, …".
- **Labels rule:** the example placeholder is "Filter tasks…" (spec §11 **Labels**).
- **Older daemon banner:** styled as the Inbox's logged-out banner (§8 here).

## 27. Penpot boards the spec names

Boards and parts in `docs/spec.md` that show tasks, the Inbox, comments, Instructions or PR-as-task UI:

- **"Screens · Main":**
  - Inbox, "Inbox · 1200", "Inbox · closing", "Inbox · partial failure", "Inbox · row menu · open PR", "Inbox · unreachable PR", "Inbox · logged out · synced before";
  - "Tasks · 1200", "Tasks · PR states", "Tasks · row menu · edit task", "Tasks · row menu · send to agent", "Tasks · row menu · merged PR";
  - "Right panel · long instruction", "Right panel · long instruction · expanded", "Main window · scrolled" (right panel);
  - "Pane · menu" (Link to task / Unlink task), "Pane · agent exited" and "Pane · hosted agent" (the linked task shown).
- **"Screens · Task sheet":** the whole page, which is "Task sheet · edit title", "· title error", "· edit description", "· workspace select", "· PR task", "· no agent", "· send to agent", "· PR link", "· PR link · hover".
- **"Screens · Dialogs":** "Link to task", "New task · send to", "Send to agent · hosted agent", "Delete workspace · panes running" (the tasks and instructions counts).
- **"Screens · Daemon":** "Older daemon" (built on the Inbox banner).
- **Parts:** `agentdeck/TaskSheetBody` (and `→ PR → Link`), `agentdeck/CommentComposer`, `agentdeck/InstructionItem`, `agentdeck/ClosePrsList`, `agentdeck/CheckboxIndeterminate`, `TaskRow`, `GroupHeader`, `PaneHeader` → `Task`, `agentdeck/icon/ExternalLink`.
- **Milestones:** §13 names the v1 pages "Screens · Main", "· Settings", "· Task sheet", "· Dialogs" and "· Daemon".

## 28. README and other docs

- **`README.md`:**
  - Line 3 describes agentdeck as adding "tasks that survive across sessions, tracking for your GitHub PRs, comment threads on tasks between you and their agents, standing instructions for every agent in a workspace".
  - Line 35 shows `agentdeck task ls --ws <ws>`.
- **`docs/spec.md`:** §3, §4, §5, §6, §8, §9, §10 and §11 carry everything above. §9 ("Comments and instructions") is wholly about the removed features.
- **`crates/agentdeck-core/rules/*.toml`, `packaging/`, `script/`:** no task references.
