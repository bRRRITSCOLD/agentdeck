# agentdeck v1 spec

The decisions behind this spec, and their reasons, are in [the v1 map](https://github.com/bRRRITSCOLD/agentdeck/issues/1). This file is the build contract: what to build, not why. When code and spec disagree, change one of them in the same PR.

## 1. Programs and crates

One Cargo virtual workspace, pinned like rust-packages: GPUI pin, toolchain 1.99.1, edition 2024, resolver 3, inherited lints. Crates live under `crates/`.

| Crate | Kind | Depends on | Notes |
|---|---|---|---|
| `agentdeck-core` | lib | — | Domain types, ids, state rules engine, no IO, no GPUI, no tokio |
| `agentdeckd` | bin | core | tokio, dropshot, rusqlite, portable-pty, alacritty_terminal |
| `agentdeck-client` | lib | core | progenitor output from `openapi.json`, checked in, built by `cargo xtask client`; plus `daemon` (find or start the daemon, §2) and `ws` (the `/v1/ws` client, §5), shared by the CLI and the GUI |
| `agentdeck` | bin | core, client | CLI and `agentdeck mcp` (rmcp); no GPUI. A current-thread tokio runtime drives the async client and the MCP server |
| `agentdeck-gui` | bin | core, client, `brrritscold-core-gpui` | GPUI app. A small multi-thread tokio runtime, kept as a GPUI global, drives the client and the socket |
| `xtask` | bin | — | `openapi` (regenerate spec), `client` (regenerate client), `ci` |

Licence `MIT OR Apache-2.0`.

## 2. Daemon

- **Start:** the GUI or the CLI runs `agentdeckd` detached when it can't reach the daemon in `daemon.json`. An optional systemd user unit is `contrib/agentdeckd.service`. Only one daemon runs per user: it holds a lock on `$XDG_RUNTIME_DIR/agentdeck/lock`.
- **Endpoint:** `127.0.0.1:<random>`. On start it writes `$XDG_RUNTIME_DIR/agentdeck/daemon.json` (mode 0600): `{ "port", "token", "pid", "version" }`. Every request, including the WebSocket upgrade, carries `Authorization: Bearer <token>`.
- **Data:** `$XDG_DATA_HOME/agentdeck/agentdeck.db` (SQLite, WAL). Scratch workspaces live in `$XDG_DATA_HOME/agentdeck/workspaces/<slug>/`. Config is `$XDG_CONFIG_HOME/agentdeck/config.toml`.
- **DB access:** rusqlite on one owned connection behind a `spawn_blocking` actor. Migrations are embedded SQL run by `rusqlite_migration` at start.
- **Logging:** `tracing`, to `$XDG_STATE_HOME/agentdeck/agentdeckd.log` and stderr.

## 3. Data model

Ids are UUIDv7 strings. Times are RFC 3339 UTC strings.

```sql
CREATE TABLE workspaces (
  id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE, name TEXT NOT NULL,
  dir TEXT NOT NULL,                 -- absolute path
  dir_owned INTEGER NOT NULL,        -- 1 = scratch dir agentdeck created, 0 = attached
  muted INTEGER NOT NULL DEFAULT 0,  -- notifications
  archived_at TEXT, created_at TEXT NOT NULL
);
CREATE TABLE tabs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces ON DELETE CASCADE,
  title TEXT NOT NULL, position INTEGER NOT NULL,
  layout TEXT NOT NULL               -- JSON split tree: {"pane":id} | {"split":"h"|"v","ratio":f,"a":…,"b":…}; JSON null until the tab has a pane
);
CREATE TABLE panes (
  id TEXT PRIMARY KEY, tab_id TEXT NOT NULL REFERENCES tabs ON DELETE CASCADE,
  kind TEXT NOT NULL,                -- 'agent' | 'shell'
  agent TEXT,                        -- 'claude' | 'codex' | 'other' (agent panes)
  argv TEXT NOT NULL,                -- JSON array, as launched (without resume flags)
  cwd TEXT NOT NULL,
  session_id TEXT,                   -- chosen by agentdeck at launch (claude) or discovered (codex)
  task_id TEXT REFERENCES tasks ON DELETE SET NULL,
  state TEXT NOT NULL DEFAULT 'unknown', -- working|blocked|done|idle|unknown|exited
  title TEXT NOT NULL DEFAULT '', exit_code INTEGER, created_at TEXT NOT NULL
);
CREATE TABLE tasks (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'todo', -- todo|doing|review|done
  kind TEXT NOT NULL DEFAULT 'plain',  -- plain|pr
  workspace_id TEXT REFERENCES workspaces ON DELETE SET NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, done_at TEXT
);
CREATE TABLE pull_requests (
  task_id TEXT PRIMARY KEY REFERENCES tasks ON DELETE CASCADE,
  repo TEXT NOT NULL, number INTEGER NOT NULL, url TEXT NOT NULL,
  state TEXT NOT NULL,               -- open|merged|closed
  draft INTEGER NOT NULL, head_ref TEXT NOT NULL,
  checks TEXT,                       -- pending|success|failure|NULL
  review TEXT,                       -- approved|changes_requested|review_required|NULL
  updated_at TEXT NOT NULL, synced_at TEXT NOT NULL,
  UNIQUE (repo, number)
);
CREATE TABLE notes (
  id TEXT PRIMARY KEY, body TEXT NOT NULL,
  workspace_id TEXT REFERENCES workspaces ON DELETE CASCADE,
  task_id TEXT REFERENCES tasks ON DELETE CASCADE,
  created_at TEXT NOT NULL, read_at TEXT, read_by_pane TEXT,
  CHECK ((workspace_id IS NULL) <> (task_id IS NULL))
);
```

A pane links to at most one task (`panes.task_id`). A task shows its linked agent panes by querying in the other direction.

v1.1 adds `channels`, `channel_members` and `messages` in their own migration.

## 4. HTTP API (`/v1`, dropshot)

`openapi.json` is generated by `cargo xtask openapi` and committed. A unit test fails when it differs from what the code generates. Errors use dropshot's `HttpError`, and an expected domain failure is its own status with a `code` field (`error_code`): `unauthorized` 401, `not_found` 404, `dirty` 409, `exited` 409 (input to a pane whose process has exited), and 400 for `invalid_name`, `invalid_dir`, `invalid_title`, `invalid_layout`, `invalid_target`, `invalid_argv`, `unsupported_kind`, `unknown_workspace`, `unknown_tab`, `unknown_task`. `DELETE /v1/workspaces/{id}`'s error body adds `dirty`, the dirty report. Every endpoint below is in v1.

| Method and path | Purpose |
|---|---|
| `GET /v1/health` | version, uptime |
| `GET/POST /v1/workspaces`, `GET/PATCH /v1/workspaces/{id}` | list, create (`{name, dir?}`: no `dir` means a scratch dir), rename, mute |
| `POST /v1/workspaces/{id}/archive`, `.../unarchive` | archive keeps the data; refused (409 `panes_running`, "Close its 2 running panes first") while any of its panes hasn't exited |
| `GET /v1/workspaces/{id}/dirty` | each git repo under `dir` with its uncommitted files and unpushed branches |
| `DELETE /v1/workspaces/{id}?force=` | without `force`: `409 dirty` with the dirty report. Removes the dir only if `dir_owned` |
| `GET/POST /v1/workspaces/{id}/tabs`, `PATCH/DELETE /v1/tabs/{id}` | tabs and their layout tree, listed in `position` order |
| `POST /v1/panes` | spawn: `{tab_id, kind, agent?, argv?, cwd, task_id?}`; see §6. `cwd` must be an existing absolute dir (`invalid_dir`); a shell without `argv` runs `$SHELL`, else `/bin/sh`; a program that can't start is `invalid_argv`. An agent pane names its `agent` and a shell names none (`unsupported_kind`); `other` needs `argv`, `claude` and `codex` take none (`invalid_argv`). The PTY starts at 80×24. The client places the pane in the tab's `layout` |
| `GET /v1/panes/{id}`, `DELETE /v1/panes/{id}` | read (live title, state and exit code while the process runs in this daemon); kill (SIGHUP to its process group and the foreground one, then SIGKILL after 3 s) and delete. Deleting a tab or workspace kills its panes too |
| `POST /v1/panes/{id}/input` | `{text, when: "now"|"idle"}` (`now` by default): typed into the PTY as is (end it with `\r` for Enter); `idle` queues until the pane is idle, done or blocked, and is typed at once if it already is. 202 `{queued}`. Queued text is dropped if the process exits |
| `GET /v1/panes/{id}/tail?bytes=` | raw output tail (256 KiB ring) for `agentdeck tail`, as `application/octet-stream`; all of it without `bytes` |
| `GET/POST /v1/tasks`, `GET/PATCH/DELETE /v1/tasks/{id}` | list filters: `status`, `workspace`, `kind`, `inbox` (no workspace) |
| `POST /v1/prs/sync` | sync now; `GET /v1/prs/status` reports the `gh` login state and the last sync |
| `GET/POST /v1/notes`, `PATCH/DELETE /v1/notes/{id}` | filters: `workspace`, `task`, `unread` |
| `GET /v1/ws` | the WebSocket, §5. Not in `openapi.json`: clients use `agentdeck_core::ws` over their own WebSocket client |

## 5. WebSocket (`/v1/ws`)

One connection per client.
- **Binary frames** carry terminal bytes: `[u32 channel, big-endian][bytes]`. Client → daemon is PTY input; daemon → client is a snapshot or live output.
- **Text frames** are JSON tagged with `t`.

Client → daemon:
```json
{"t":"attach","pane":"<id>","channel":3}
{"t":"detach","channel":3}
{"t":"resize","channel":3,"cols":120,"rows":40}
{"t":"primary","channel":3}
```
Daemon → client:
```json
{"t":"attached","channel":3,"pane":"<id>","cols":100,"rows":30,"title":"✳ Claude Code","state":"idle"}
{"t":"resize","channel":3,"cols":120,"rows":40}
{"t":"title","channel":3,"title":"◐ Claude Code"}
{"t":"state","pane":"<id>","state":"working"}
{"t":"exit","pane":"<id>","code":0}
{"t":"changed","entity":"task","id":"<id>"}
{"t":"lagged","channel":3}
{"t":"error","channel":3,"code":"not_found","message":"…"}
```
The message types are `agentdeck_core::ws::{ClientMessage, ServerMessage}`.
- **Attach:** the daemon sends `attached`, then one binary frame holding the **snapshot**, then live bytes. The snapshot and the live subscription are taken under one lock, so no output is lost or doubled.
  - The snapshot is the daemon's `Term` re-encoded as escape sequences (SGR runs, cursor, modes). It includes scrollback and the primary screen under an alternate screen.
  - The client feeds it into a fresh `alacritty_terminal::Term` of the `attached` size. It starts with a full reset, so a reused `Term` works too.
  - It does not carry the scroll region, tab stops or the active character set, which `alacritty_terminal` keeps private.
  - Attaching a channel that is already attached replaces it.
- **Resize:** the **primary** client's size wins. The most recent client to send `primary` (the focused GUI window) is primary; until one does, or after it detaches, any client's resize applies. A resize from any other client is ignored and answered with `resize` carrying the size the pane keeps. Sending `primary` applies the size that client last asked for. Other attached clients get `resize` when the size changes; bytes after it were written at the new size.
- **Terminal queries** (DA, cursor position, …) are answered only by the daemon's `Term`. Clients drop the replies their own `Term` generates.
- **Backpressure:** a client more than 1 MiB behind on a channel gets `lagged`. It then re-attaches for a fresh snapshot.
- **Data events:** `changed` tells clients to refetch an entity over REST: `workspace`, `tab`, `pane`, `task` or `note`, after it is created, changed or deleted; `pr_sync` (id `status`) means `GET /v1/prs/status`, when a PR sync begins, reads a page or ends. `state` and `exit` are sent to every client, attached or not.
- **Errors:** a message the daemon can't act on gets `error` with a `code`: `not_found` (attach to a pane that isn't live), `not_attached` (a channel with nothing attached), `invalid_message`.

## 6. Launching agents

- **Environment:** built from an allowlist: `PATH HOME USER SHELL LANG LC_* TERM COLORTERM XDG_* SSH_AUTH_SOCK GH_* EDITOR CODEX_HOME`, plus `TERM=xterm-256color` and `COLORTERM=truecolor`. Nothing else from the daemon's own environment is passed on, and that rule is tested.
- **Argv by agent:** `claude` and `codex` are looked up on the pane's `PATH`, so tests put stubs first.
  - `claude`: `claude --session-id <uuid> [<launch prompt>]`, with a UUIDv4 agentdeck chooses.
  - `codex`: `codex [<launch prompt>]`. Codex has no flag to start on a chosen session id (checked against `openai/codex` on 2026-11-11: `codex resume <id>` and `codex fork <id>` only). It writes each session to `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl` (`CODEX_HOME` from the pane's environment, default `$HOME/.codex`). The daemon lists those files just before launch, then polls every 250 ms for up to 2 min, while the pane runs, for the newest new one whose id no other pane holds, and stores its id as `session_id` (a `changed` pane event follows).
  - `other` and `shell`: argv as given.
  - `panes.argv` holds the argv as first launched, launch prompt included.
- **Launch prompt** for `claude` and `codex` panes: the linked task's title and body, followed by every unread note on the task or on the pane's workspace, oldest first. With neither, there is no prompt argument. Those notes are then marked read by this pane. `other` panes get no prompt.

  ```
  Task: <title>

  <body>

  Notes left for you:
  - <note>
  - <note, continuation lines indented two spaces>
  ```
- **Trust dialog:** never answered automatically. The launch prompt goes in as an argument, and input agentdeck types on its own (note delivery, §9) waits until the pane is `idle` or `done`, never `blocked`, so it can't answer any dialog. The GUI focuses the new pane so you can answer it.
- **Resume after a restart:** at start, before it serves, the daemon re-spawns each pane whose state isn't `exited`. Exits aren't recorded while the daemon shuts down, so those are the panes that were alive. Their rows, and so the layout, are unchanged.
  - `claude` panes as `claude --resume <session_id>`.
  - `codex` panes as `codex resume <session_id>`; one whose session was never found comes back as a shell.
  - All other panes as `$SHELL` (else `/bin/sh`) in their `cwd`, with the `shell` rules.
  - A pane that can't be re-spawned (its `cwd` is gone, say) is marked exited.
  - A pane that had exited stays exited.

## 7. Agent state

`agentdeck-core::state` evaluates **rules**: data files, embedded defaults that a file in `$XDG_CONFIG_HOME/agentdeck/rules/<agent>.toml` can override. A file that doesn't parse is logged and the defaults apply. Shell panes read `rules/shell.toml`; none is shipped, so a shell is `working` while it prints and `unknown` otherwise. Inputs are the foreground process name, the OSC 0/2 title, the last N screen rows, OSC 9;4 and OSC 7501, and time since the last output.

```toml
agent = "claude"
process = ["claude"]

[[rule]]                       # first match wins
state = "blocked"
rows = "bottom:6"
contains = ["Enter to confirm", "Esc to cancel"]

[[rule]]
state = "working"
title_regex = '^[◐◓◑◒]'

[[rule]]
state = "idle"
title_regex = '^✳'

[settle]
min_ms = 400                   # a new state must hold this long before it is published
```

- **Rule keys:** a rule names a `state` (`working`, `blocked` or `idle`) and one or more conditions, all of which must hold:
  - `title_regex`, matched against the title;
  - `contains`, strings that must each appear in `rows`, which is `screen` (the default) or `bottom:N`, the last N non-blank rows of the live screen;
  - `progress`, OSC 9;4 states (`removed`, `normal`, `error`, `indeterminate`, `paused`);
  - `program_status`, OSC 7501 states (`idle`, `working`, `done`, `blocked`, `error`).
  The rules apply only while a `process` entry is in the foreground, or when the foreground process can't be read. `done`, `unknown` and `exited` come from the evaluator, never from a rule.
- **done vs idle:** `done` is published when a pane goes working → idle. It turns back to `idle` once the pane is focused, or after a new prompt.
- **Exit:** a dead process is `exited`, whatever the rules say.
- **No match:** the state is `unknown`, unless the pane has produced output in the last 2 s, in which case it is `working`.
- **v1 ships rules for `claude` and `codex`.** The spike captured `claude`'s title and dialog patterns; `codex`'s rules come from recorded sessions.
- **Tests:** every rule set has unit tests that feed **recorded terminal output** (`tests/fixtures/<agent>/*.bytes` plus the expected state) through a `Term`.

## 8. PR sync

- The token comes from `gh auth token` at start and again after a 401; while there is none, every sync asks `gh` again. No `gh`, a `gh` that isn't logged in, or a token GitHub still rejects after that retry means `GET /v1/prs/status` reports `logged_out`, and the GUI shows a banner.
- A sync every 120 s (the first at start), and on `POST /v1/prs/sync`, which clients call when a task is opened. One sync runs at a time. `AGENTDECK_GITHUB_API` replaces the GraphQL URL (`https://api.github.com/graphql`); tests point it at httpmock.
  - **Full:** the first sync, and the first after an hour since the last full one (`AGENTDECK_PR_FULL_SYNC_SECS`, `0` for every sync), in two phases. First the list `viewer.pullRequests(states: OPEN)`, 100 a page, most recently updated first, with light fields only (no review, no checks); each page is stored as it arrives, a stored PR keeping its review and checks. Then every field, fetched by number, for the PRs that are new, that GitHub updated since they were stored, or whose checks are pending.
  - **Incremental:** the rest search `is:pr author:@me updated:>=<T>`, where `T` is 10 minutes before the last good sync began, so a merged or closed PR comes back in its new state. A merged or closed PR not stored as open is skipped. Then open PRs whose checks are `pending`, which a finished check run doesn't bring into the search, are fetched by number, and inbox PRs are matched to workspaces again as stored. Nothing is found gone.
- `POST /v1/prs/sync` answers once its sync finishes, with the same body as `GET /v1/prs/status`: `{ login: "logged_in"|"logged_out", last_synced_at, last_error, progress }`. A failed sync is still 200, with `last_error` set: `GitHub answered <status>`, plus GitHub's `message` when the body is JSON. A request that fails with a 5xx or in transit is tried up to 3 times, 1 s then 2 s apart.
- Each result becomes a PR task, titled with the PR's title:
  - Its **workspace** is the oldest unarchived workspace whose dir holds a clone of the PR's repo with `head_ref` present as a local branch. A clone is a git repo up to 3 levels under the dir (as the dirty report searches) with any `github.com` remote (https, ssh or scp-like URL) naming the repo, compared case-insensitively. If no workspace matches, it goes to the inbox (`workspace_id` NULL). Only an inbox task is matched again on later syncs; a PR task never leaves its workspace.
  - **New PR:** a PR task created with status `review`.
  - **Merged or closed:** the task goes to `done`; reopened, back to `review`. While the PR's state holds, the task keeps whatever status it has. In a full sync, PRs that left the list are fetched by number, once; one GitHub no longer shows is stored as closed. Fetches by number go 25 to a request, 4 requests at a time.
- `checks` and `review` come from `statusCheckRollup` (`ERROR` counts as failure, `EXPECTED` as pending) and `reviewDecision`. Every task in the API carries its PR as `pr` (null for a plain task).
- A sync stores each page as it arrives, and each task it creates or changes gets a `changed` event (§5) then. While it runs, the status's `progress` is `{ prs_done, prs, stage }` (`prs` from the search's count, `0` until the first page answers; `stage` is `listing`, then `details` while reviews and checks are fetched by number), and every change to it sends a `pr_sync` event. Clients show it: "Syncing… 100 of 271 PRs", then "Syncing details…".

## 9. Notes

- **On a task**, when a live agent pane is linked to the task: delivered automatically to the newest such pane, typed with Enter (`<body>\r`) once the pane is `idle` or `done`. Unlike `input` `when: "idle"`, it never goes into a `blocked` pane, so it can't answer a dialog. It is marked read when typed, and stays unread if the process exits first.
- **On a workspace**, or on a task with no live pane: delivered in the next launch prompt there (§6).
- **Any note** can be sent to a chosen pane at once with the GUI's "Send to agent" action or `agentdeck agent send`.
- **Read:** delivery marks the note read and records the pane. A workspace note is not typed into live panes. Agents can also pull notes through the CLI or MCP.

## 10. CLI and MCP

`agentdeck` (clap) reads `daemon.json` and starts the daemon when it isn't running: no file, a pid that isn't running, or a port that refuses the connection. It starts `$AGENTDECK_DAEMON` if set, else the `agentdeckd` beside its own executable, else `agentdeckd` from `PATH`, in its own process group with no terminal, and waits up to 10 s for a `daemon.json` that answers `/v1/health`. Output is human-readable, or with `--json` the API's own objects (`ws rm` prints `{id, deleted}`, `tail` prints `{pane, output}`). A failure prints `agentdeck: <message> (<error_code>)` to stderr and exits 1.

```
agentdeck ws ls | new <name> [--dir PATH] | archive <ws> | rm <ws> [--force]
agentdeck task ls [--status S] [--ws WS] [--inbox] | add <title> [--ws WS] [--body B] | set <id> --status S | show <id>
agentdeck note ls [--ws WS|--task T] [--unread] | add <body> (--ws WS | --task T) | read <id>
agentdeck agent new --ws WS [--agent claude|codex] [--cwd DIR] [--task T] | send <pane> <text> [--now]
agentdeck pr sync | status
agentdeck tail <pane> [--bytes N]
agentdeck mcp            # MCP server over stdio
```

- A `WS` or `<ws>` is a workspace's id, slug or name, tried in that order; a name two workspaces share is an error that lists their slugs.
- `ws new --dir` takes a path relative to the current directory too; the CLI sends it absolute.
- `ws rm` without `--force` on a dirty dir prints the dirty report and exits 1.
- `agent new` launches the agent with `POST /v1/panes` (no `argv`: the daemon builds it and the launch prompt) in `--cwd`, made absolute, or the workspace's dir. The pane goes in the workspace's first tab with no panes, else in a new tab titled after the agent, and becomes that tab's layout. A tab made for a launch that fails is deleted.
- `agent send` types the text followed by `\r` (Enter), with `when: "idle"`, or `"now"` with `--now`.
- `note read` prints the note and marks it read, recording `$AGENTDECK_PANE` as the reader when it is set.

**MCP tools** (rmcp): `tasks_list`, `task_get`, `task_create`, `task_update`, `notes_list`, `note_create`, `note_mark_read`, `workspaces_list`, `prs_status`.
- `AGENTDECK_PANE` and `AGENTDECK_WORKSPACE` are set in every pane's environment. They are added on top of the allowlist.
- Tools default to the caller's workspace and pane:
  - `tasks_list` and `notes_list` list the caller's workspace unless given a `workspace` (or, for notes, a `task`); `tasks_list` takes `inbox: true` for the inbox, and both take `all_workspaces: true` for everything. Outside a workspace they list everything.
  - `task_create` and `note_create` (without a `task`) go to the caller's workspace unless given one; a note with neither is an error.
  - `note_mark_read` records the caller's pane as the reader.
- Each tool answers with the API's JSON object as text. A failure (a bad argument, or the daemon's error with its `error_code`) is a tool result with `isError: true`, not a protocol error.
- Each call reaches the daemon the way the CLI does, so a daemon restarted under a long-lived `agentdeck mcp` is found again.

## 11. GUI

Built on `brrritscold-core-gpui` (`init`, `Root`, theme preference, toasts). Screens follow the Penpot page design. The design is made before GUI work starts; until then, this list is the outline.

**Dark mode is first class.** Every screen is designed, built and checked in Dark first, then Light, to the same standard. Every GUI capture set includes both modes. The terminal palette follows the active mode. The theme preference defaults to `System` (follows the OS appearance); Settings and the command palette offer System / Dark / Light, saved in `config.toml`.

- **Title bar:** the window asks for the compositor's frame. Where the compositor draws none (GNOME's Mutter on Wayland), the app draws a 32px title bar across the top: drag to move, double-click to maximize, right-click for the window menu, minimize / maximize / close at the right. 4px resize strips run along the window's inside edges, except on a tiled side. Everything else sits under it.
- **Sidebar:** workspaces, each with a rolled-up state (any blocked → blocked, else any working → working, else any done → done) and its unread-note and PR counts. Inbox and Tasks entries sit at the top. Below the workspaces, an **Archived** group header (chevron, "Archived", a count) lists the archived workspaces, oldest first: rows with `muted-foreground` labels and no counts. It is folded at first, a click toggles it, it stays open while it holds the selected workspace, and it is hidden while nothing is archived.
- **Workspace view:** a top bar with the workspace's name and dir, New agent, a `…` workspace menu and the right panel toggle; under it, tabs, with split panes inside each. Each pane has a header showing its title, state and linked task, and a menu to split or close it. A `+` after the tabs opens a shell tab in the workspace dir; a workspace with no tabs when shown gets one.
  - **Archive workspace**, in the `…` menu, archives at once, with no dialog, since it can be undone. The daemon refuses while a pane runs, and a toast "Can't archive <name>" gives its reason ("Close its 2 running panes first"). Otherwise the toast "Archived <name>" says "Its folder, tasks and notes are kept." and offers **Undo**, which unarchives. The workspace stays selected, now archived.
  - **Delete workspace…**, after a separator and in `destructive`, opens the delete dialog.
  - **An archived workspace** shows an outline **Unarchive** in place of New agent, and its menu has only Delete workspace…. In place of its tabs and panes it shows "<name> is archived" over "Unarchive it to open its tabs and start agents here again." It attaches no terminals and opens no shell tab.
- **Right panel**, which can be toggled: the workspace's tasks and PRs, as a list grouped by status under a header whose `+` opens the new task dialog, plus its notes.
- **Tasks view:** a grouped list of every task, with a filter, a Show done switch and **New task**. Done tasks are hidden by default, and each row has a status picker.
- **Inbox:** tasks with no workspace, mostly PRs.
  - **gh logged out:** while `GET /v1/prs/status` says `logged_out`, a default alert with the `Info` icon sits between the Inbox's header and its columns: "GitHub isn't connected" / "PR sync uses the gh login. Run gh auth login in a terminal, then press Sync now." It goes once the login is back.
  - The header's sync label and the sidebar's footer read "gh logged out" then, over a failed sync or an earlier one; a running sync still shows its progress, so pressing Sync now after logging in answers at once.
- **Dialogs:** new workspace, new agent (agent, cwd, task), new task, and delete workspace:
  - **New task** is titled "New task", with "Add a task to a workspace, or to the Inbox.": a Title ("What needs doing?", required), a Description (optional) and a Workspace select of the unarchived workspaces plus "Inbox (no workspace)". The Tasks view's **New task** opens it on the Inbox, the right panel's Tasks & PRs **+** on that workspace (the Inbox for an archived one). **Create task** is disabled while the title is blank; Enter in the title does the same. It makes a `todo` task with `POST /v1/tasks` and closes; the task comes back through `changed`. A failure keeps it open and toasts why.
  - **Delete workspace** is titled "Delete <name>?", with "This can't be undone. Archive it instead to keep its folder."
  - It reads the dirty report on opening, showing "Checking for unpushed work…" meanwhile. When repos are dirty, a destructive alert "N repos have work that isn't pushed" / "Deleting removes the folder and everything in it." sits over a card per repo: its path under the workspace, "N uncommitted files in <the folder they share>" and "<branch>: not pushed" for each unpushed branch.
  - Then "Its N tasks move to the Inbox. Its N notes are deleted." and "Type <name> to confirm" over an input.
  - The footer is **Archive instead** (absent once archived), which closes it and archives as the menu does, **Cancel** and the destructive **Delete workspace**. Delete is disabled until the input is the name exactly. It deletes with `force`, since the report has been shown; Enter does the same, and Escape cancels.
  - A failed delete keeps the dialog open and toasts why. Once deleted, the selection falls back to the first workspace left, else the Inbox.
- **Terminal:**
  - `alacritty_terminal` per attached pane, drawn by our own renderer.
  - **Zed's GPL-3.0 terminal crates are never copied.**
  - The font is Inter for the UI. The terminal gets a monospace font with a symbols fallback for agent glyphs (`⎿`, `✻`, `◐`).
  - The mouse, after xterm: a program that asks for mouse reports (1000 / 1002 / 1003, SGR 1006) gets clicks, drags and the wheel; shift takes them back for selecting and the scrollback. Otherwise, on the alternate screen, the wheel sends an arrow key per line (1007).
  - Input methods and dead keys: a composition shows underlined at the cursor, and only committed text reaches the pane. The candidate window opens at the cursor.
- **Colours** use semantic tokens only.
  - Agent state: working `info`, blocked `destructive`, done `success`, idle/unknown/exited-0 `muted-foreground`, exited ≠ 0 `destructive`.
  - Blocked and a failed exit share red, as in herdr: the label (`blocked` vs `exited 1`) and the StateDot shape (a filled dot vs a red `OctagonX`) tell them apart, so colour is never the only signal.
  - Checks: passing `success`, pending `info`, failing `destructive`.
  - Review: approved `success`, changes requested `warning`, required `muted-foreground`.
  - Task statuses aren't coloured.
  - The terminal palette follows the theme. Terminal text is JetBrains Mono (`font-family.mono`).
- **Notifications:** an OS notification when a pane turns `blocked` or `done` while its tab isn't visible, and when a PR task's checks fail or a review arrives. They can be muted per workspace.
- **Keymap:** `keymap.rs`, normal app shortcuts, `secondary-` bindings loaded with `KeyBinding::load`.
  - A binding wins over the terminal, so every shortcut is one a shell has no use for, after GNOME Terminal. Plain ctrl-letter, alt-letter, the arrows and the paging keys always reach the pane.
  - Quit `secondary-shift-q`; next / previous tab `ctrl-pagedown` / `ctrl-pageup` (or `ctrl-tab` / `ctrl-shift-tab`); tab 1–9 `alt-1`…`alt-9`; next / previous pane `secondary-shift-down` / `secondary-shift-up`, in layout order; new shell tab `secondary-shift-t`; split the focused pane with a shell right / down `secondary-shift-e` / `secondary-shift-o` (after Terminator); close it `secondary-shift-w`, asking first while it runs. Closing a tab's last pane closes the tab.

## 12. Testing

| Tier | Where | What |
|---|---|---|
| unit | `agentdeck-core`, GUI views (`#[gpui::test]`), daemon pure parts | rules with recorded output, layout tree, snapshot encode/decode round-trip, env allowlist, `openapi.json` staleness |
| integration | `agentdeckd/tests/integration` | temp `$XDG_*` dirs, a real daemon on a random port, a real client; PTYs run `sh`; GitHub is httpmock; `gh` is a stub script on `PATH` |
| integration | `agentdeck/tests/integration` | the real `agentdeck` binary in temp `$XDG_*` dirs, starting the real daemon itself; `agentdeck mcp` driven over stdio with hand-written JSON-RPC; panes run `sh`, never a real agent CLI |

There is no e2e tier. GPUI's headless context doesn't advance real sockets, so anything touching sockets is an integration test. CI is Linux only: fmt, clippy, unit, integration and doc, with warnings denied.

## 13. Milestones

v1: §1–§12, the tabs and split-pane layout (Penpot page "Screens").

v1.1: the **canvas and channels together** (Penpot page "Canvas", #17).
- Each workspace's home view is a canvas of agent nodes that zoom in stages.
- Channel hubs, with member and listener edges.
- Orchestrator nodes.
- Focus into v1's split view.
- The `channels`, `channel_members`, `messages` and `canvas_nodes` tables.
- `agentdeck send <channel>`, MCP `channel_post` / `channel_read`, and delivery through `input` `when: "idle"`.

v1's split view stays: v1.1's Focus mode opens it.
