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
| `agentdeck-gui` | bin | core, client, notify, `brrritscold-core-gpui` | GPUI app. A small multi-thread tokio runtime, kept as a GPUI global, drives the client and the socket |
| `agentdeck-notify` | lib | — | The GUI's desktop notifications: the freedesktop notifications D-Bus API over one zbus connection, on zbus's own runtime (no tokio); clicks reach GPUI's foreground (§11) |
| `xtask` | bin | — | `openapi` (regenerate spec), `client` (regenerate client), `ci` |

Licence `MIT OR Apache-2.0`.

## 2. Daemon

- **Start:** the GUI or the CLI runs `agentdeckd` detached when it can't reach the daemon in `daemon.json`. An optional systemd user unit is `contrib/agentdeckd.service`. Only one daemon runs per user: it holds a lock on `$XDG_RUNTIME_DIR/agentdeck/lock`.
- **Endpoint:** `127.0.0.1:<random>`. On start it writes `$XDG_RUNTIME_DIR/agentdeck/daemon.json` (mode 0600): `{ "port", "token", "pid", "version" }`. Every request, including the WebSocket upgrade, carries `Authorization: Bearer <token>`.
- **Data:** `$XDG_DATA_HOME/agentdeck/agentdeck.db` (SQLite, WAL). Scratch workspaces live in `$XDG_DATA_HOME/agentdeck/workspaces/<slug>/`. Config is `$XDG_CONFIG_HOME/agentdeck/config.toml`, which the daemon owns: it reads the file at start, and clients read and change the settings only through `GET` / `PATCH /v1/settings` (§4), so the GUI, the CLI and PR sync share one copy. The file need not exist.
  - **Settings:** `[appearance] theme` (`system` | `dark` | `light`, default `system`; the GUI's theme, §11), `[pr_sync] max_age_days` (a count of days, default `90`, `0` for no cutoff; §8) and `[terminal] file_opener` (a command, default `xdg-open {path}`; what a `secondary`-click on a file path in a pane runs, §11). Each is named by its dotted key, `appearance.theme`.
    - `file_opener` is split into words as a shell would split it (`shell-words`) and never run through a shell. One argument after the program must hold `{path}`, the file's absolute path; `{line}` and `{col}` are the line and column the path names, `1` when it names none. A value that is blank, doesn't split (an unclosed quote) or passes no `{path}` is refused (`invalid_setting`), and in the file gives the default.
  - **Reading:** a missing file or key gives the default; a value of the wrong kind gives that key's default; a key the daemon doesn't know is ignored; a file that doesn't parse is logged and every default applies, as a state rule file's is (§7).
  - **Writing:** a PATCH re-reads the file, changes only its keys with `toml_edit`, so every other key, table and comment stays, and writes a temp file beside it that it renames over it, creating the dir if need be. A file that doesn't parse is left as it is: the PATCH is `409 unparsable_config`. A change applies at once, with no restart.
  - **Environment overrides:** `AGENTDECK_PR_MAX_AGE_DAYS` overrides `pr_sync.max_age_days` (§8). The settings then report the variable's value and list the key in `overridden_by_env`. A PATCH of an overridden key still writes the file and answers 200, but the variable's value stays in use, and the key stays in `overridden_by_env`, until the daemon runs without it.
- **DB access:** rusqlite on one owned connection behind a `spawn_blocking` actor. Migrations are embedded SQL run by `rusqlite_migration` at start.
- **Logging:** `tracing`, to `$XDG_STATE_HOME/agentdeck/agentdeckd.log` and stderr.

## 3. Data model

Ids are UUIDv7 strings. Times are RFC 3339 UTC strings; the daemon stamps them to the microsecond (`2026-11-14T10:00:00.123456Z`), so they sort as text and an edit right after a create still stamps later. Rows written before that carry milliseconds, which still sort before every later stamp.

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
  aged_out_at TEXT,                  -- when PR sync aged it out (§8); NULL otherwise
  UNIQUE (repo, number)
);
CREATE TABLE comments (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks ON DELETE CASCADE,
  body TEXT NOT NULL,
  author TEXT NOT NULL,              -- user|agent
  author_agent TEXT,                 -- claude|codex|NULL
  author_pane TEXT REFERENCES panes ON DELETE SET NULL,
  created_at TEXT NOT NULL,
  delivered_at TEXT, delivered_to_pane TEXT,
  seen_at TEXT                       -- when the user saw an agent's comment; NULL = new
);
CREATE TABLE instructions (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces ON DELETE CASCADE,
  body TEXT NOT NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
```

A pane links to at most one task (`panes.task_id`). A task shows its linked agent panes by querying in the other direction.

Migration `0002` replaced v1's first `notes` table (a note on exactly one of a workspace and a task, with `read_at` and `read_by_pane`): task notes became the user's comments (`read_at` → `delivered_at`, `read_by_pane` → `delivered_to_pane`, seen), workspace notes became instructions, and `notes` was dropped. Migration `0003` added `pull_requests.aged_out_at`.

v1.1 adds `channels`, `channel_members` and `messages` in their own migration.

## 4. HTTP API (`/v1`, dropshot)

`openapi.json` is generated by `cargo xtask openapi` and committed. A unit test fails when it differs from what the code generates. Errors use dropshot's `HttpError`, and an expected domain failure is its own status with a `code` field (`error_code`): `unauthorized` 401, `not_found` 404, `dirty` 409, `exited` 409 (input to a pane whose process has exited), `unparsable_config` 409 (a settings PATCH while `config.toml` doesn't parse), and 400 for `invalid_setting` (a key that isn't a setting, or a value it can't take; the message starts with the dotted key), `invalid_name`, `invalid_dir`, `invalid_title`, `invalid_layout`, `invalid_target`, `invalid_argv`, `invalid_task` (a task in another workspace than the pane's, or done), `unsupported_kind`, `unknown_workspace`, `unknown_tab`, `unknown_task`, `unknown_pane`. `DELETE /v1/workspaces/{id}`'s error body adds `dirty`, the dirty report. Every endpoint below is in v1.

| Method and path | Purpose |
|---|---|
| `GET /v1/health` | version, uptime |
| `GET/POST /v1/workspaces`, `GET/PATCH /v1/workspaces/{id}` | list, create (`{name, dir?}`: no `dir` means a scratch dir), rename, mute |
| `POST /v1/workspaces/{id}/archive`, `.../unarchive` | archive keeps the data; refused (409 `panes_running`, "Close its 2 running panes first") while any of its panes hasn't exited |
| `GET /v1/workspaces/{id}/dirty` | each git repo under `dir` with its uncommitted files and unpushed branches, each `{name, commits}`: how many of its commits are on no remote branch (`git rev-list --count <branch> --not --remotes`), so a branch with no upstream counts what it would push |
| `DELETE /v1/workspaces/{id}?force=` | without `force`: `409 dirty` with the dirty report. Removes the dir only if `dir_owned` |
| `GET/POST /v1/workspaces/{id}/tabs`, `PATCH/DELETE /v1/tabs/{id}` | tabs and their layout tree, listed in `position` order |
| `POST /v1/panes` | spawn: `{tab_id, kind, agent?, argv?, cwd, task_id?}`; see §6. `cwd` must be an existing absolute dir (`invalid_dir`); a shell without `argv` runs `$SHELL`, else `/bin/sh`; a program that can't start is `invalid_argv`. An agent pane names its `agent` and a shell names none (`unsupported_kind`); `other` needs `argv`, `claude` and `codex` take none (`invalid_argv`). The PTY starts at 80×24. The client places the pane in the tab's `layout` |
| `GET /v1/panes/{id}`, `DELETE /v1/panes/{id}` | read (live title, state and exit code while the process runs in this daemon); kill (SIGHUP to its process group and the foreground one, then SIGKILL after 3 s) and delete. Deleting a tab or workspace kills its panes too |
| `PATCH /v1/panes/{id}` | link the pane to a task, or unlink it: `{task_id: id\|null}`, `task_id` required. `unknown_task` when there is no such task; `invalid_task` when it is in another workspace than the pane's (the Inbox included) or done. Sends `changed` for the pane, then for its old and new task when the link changed. A live agent pane linked to a new task gets the task's waiting comments (§9) |
| `POST /v1/panes/{id}/input` | `{text, when: "now"|"idle"}` (`now` by default): typed into the PTY as is (end it with `\r` for Enter); `idle` queues until the pane is idle, done or blocked, and is typed at once if it already is. 202 `{queued}`. Queued text is dropped if the process exits |
| `GET /v1/panes/{id}/tail?bytes=` | raw output tail (256 KiB ring) for `agentdeck tail`, as `application/octet-stream`; all of it without `bytes` |
| `GET/POST /v1/tasks`, `GET/PATCH/DELETE /v1/tasks/{id}` | list filters: `status`, `workspace`, `kind`, `inbox` (no workspace). Each task carries `unseen_agent_comments`, the agents' comments on it the user hasn't seen |
| `POST /v1/prs/sync` | sync now; `GET /v1/prs/status` reports the `gh` login state, the last sync and the age cutoff |
| `GET/POST /v1/tasks/{id}/comments`, `DELETE /v1/comments/{id}` | the task's thread, oldest first. POST takes `{body, pane?}`: with `pane` (`unknown_pane` if there is none) the author is that pane's agent, else the user (§9) |
| `POST /v1/tasks/{id}/comments/seen` | 204: stamps `seen_at` on every agent comment on the task the user hasn't seen; the GUI calls it when the user opens the task |
| `GET/POST /v1/workspaces/{id}/instructions`, `PATCH/DELETE /v1/instructions/{id}` | the workspace's instructions, oldest first; PATCH `{body}` edits one and stamps `updated_at` |
| `GET /v1/settings`, `PATCH /v1/settings` | every setting (§2) by section, `{appearance: {theme}, pr_sync: {max_age_days}, terminal: {file_opener}, overridden_by_env}`; PATCH takes a partial object of the same sections (`{"pr_sync": {"max_age_days": 30}}`), writes `config.toml` and answers with every setting |
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
  - It ends with the shell's last OSC 7 (`ESC ] 7 ; <uri> ST`), which a `Term` ignores, so a client reading OSC 7 for file paths (§11) has the directory from the attach on. Nothing else changes: an older client's `Term` drops it.
  - Attaching a channel that is already attached replaces it.
- **Resize:** the **primary** client's size wins. The most recent client to send `primary` (the focused GUI window) is primary; until one does, or after it detaches, any client's resize applies. A resize from any other client is ignored and answered with `resize` carrying the size the pane keeps. Sending `primary` applies the size that client last asked for. Other attached clients get `resize` when the size changes; bytes after it were written at the new size.
- **Terminal queries** (DA, cursor position, …) are answered only by the daemon's `Term`. Clients drop the replies their own `Term` generates.
- **Backpressure:** a client more than 1 MiB behind on a channel gets `lagged`. It then re-attaches for a fresh snapshot.
- **Data events:** `changed` tells clients to refetch an entity over REST: `workspace`, `tab`, `pane`, `task`, `comment` or `instruction`, after it is created, changed or deleted (a comment also when it is delivered or seen; its task too when an agent writes one or it is seen, since the task's `unseen_agent_comments` changed); `pr_sync` (id `status`) means `GET /v1/prs/status`, when a PR sync begins, reads a page or ends, or a PATCH changes the age cutoff; `settings` (id `settings`) means `GET /v1/settings`, after every PATCH. `state` and `exit` are sent to every client, attached or not. The daemon subscribes a connection to these events before it answers the upgrade, so every event published after the client sees the 101 reaches it.
- **Errors:** a message the daemon can't act on gets `error` with a `code`: `not_found` (attach to a pane that isn't live), `not_attached` (a channel with nothing attached), `invalid_message`.

## 6. Launching agents

- **Environment:** built from an allowlist: `PATH HOME USER SHELL LANG LC_* TERM COLORTERM XDG_* SSH_AUTH_SOCK GH_* EDITOR CODEX_HOME`, plus `TERM=xterm-256color` and `COLORTERM=truecolor`. Nothing else from the daemon's own environment is passed on, and that rule is tested.
- **Argv by agent:** `claude` and `codex` are looked up on the pane's `PATH`, so tests put stubs first.
- **A program that can't start** is refused with 400 `invalid_argv` and the pane row is dropped. When `argv[0]` is a bare name in none of the pane's `PATH` directories, the message is only what to do: "`codex` isn't on agentdeckd's PATH. Install it, or start agentdeckd from a shell where it's found."; the lookup's whole error, which lists every `PATH` directory, goes to `agentdeckd.log` at warn. Any other spawn error keeps its text. The GUI's toast and `agentdeck agent new` show an `invalid_argv` refusal as that message alone.
  - `claude`: `claude --session-id <uuid> [<launch prompt>]`, with a UUIDv4 agentdeck chooses.
  - `codex`: `codex [<launch prompt>]`. Codex has no flag to start on a chosen session id (checked against `openai/codex` on 2026-11-11: `codex resume <id>` and `codex fork <id>` only). It writes each session to `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl` (`CODEX_HOME` from the pane's environment, default `$HOME/.codex`). The daemon lists those files just before launch, then polls every 250 ms for up to 2 min, while the pane runs, for the newest new one whose id no other pane holds, and stores its id as `session_id` (a `changed` pane event follows).
  - `other` and `shell`: argv as given.
  - `panes.argv` holds the argv as first launched, launch prompt included.
- **Launch prompt** for `claude` and `codex` panes: the linked task's title and body, then every instruction for the pane's workspace, oldest first, then the task's user comments no agent has had yet, oldest first. Those comments are then marked delivered to this pane. Either block is left out when empty; with no task and no instructions, there is no prompt argument. `other` panes get no prompt.

  ```
  Task: <title>

  <body>

  Instructions for this workspace:
    - <instruction>
    - <instruction, continuation lines indented four spaces>
  Comments since your last session:
    - <comment>
  ```
- **Trust dialog:** never answered automatically. The launch prompt goes in as an argument, and input agentdeck types on its own (comment delivery, §9) waits until the pane is `idle` or `done`, never `blocked`, so it can't answer any dialog. The GUI focuses the new pane so you can answer it.
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
rows = "bottom:1"
contains = ["Esc to cancel"]

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
- **v1 ships rules for `claude` and `codex`,** both from recorded sessions.
- **`claude`** (from Claude Code v2.1.325, recorded at 120×40 in the default inline TUI and in the fullscreen one with a status line; foreground process `claude`, or `node` for some npm installs):
  - The OSC 0 title is a state glyph and the session's name: `✳ Claude Code` at a fresh prompt, then a name Claude Code makes up after the first turn (`✳ Ok`). The glyph is `✳` when idle, a half-circle spinner (`◐`, `◑`) while it thinks, streams or runs a tool, and `✳` again on a permission prompt. The rules read `working` from a spinner (Braille as well, which versions before v2.1.228 drew) and `idle` from `✳`, below the dialog rule.
  - Dialogs are read from the screen by the key hint on the bottom non-blank row (`bottom:1`), which model output can't reach because the footer holds it otherwise: `Enter to confirm · Esc to cancel` under the folder trust dialog (`Accessing workspace:` … `Yes, I trust this folder`) and other select dialogs, and `Esc to cancel · Tab to amend` under a tool permission prompt (`Do you want to proceed?` … `1. Yes`). In the fullscreen TUI the dialog replaces the composer and its hint is still the last non-blank row.
  - The inline footer adds `esc to interrupt` while a turn runs; a rule reads it from the bottom row, for when the title is missing. The fullscreen footer with a status line doesn't show it.
  - No OSC 9;4. OSC 7501 only as a startup probe, `ESC ] 7501 ; ? BEL`, which agentdeck doesn't answer; no state follows it. The rules keep OSC 7501 and 9;4 entries should Claude Code start sending them unprompted.
  - These correct the spike's notes in two places: the title doesn't keep `Claude Code` once a session has a name, and it shows `✳`, not a spinner, on a permission prompt, so only the screen tells blocked from idle. The trust dialog now preselects `No, exit`.
  - After `/exit` it clears the title and prints `claude --resume <session id>`; no rule matches, and the process exit publishes `exited`.
- **`codex`** (from codex-cli 0.169.0, recorded at 120×40; foreground process `codex`):
  - OSC 7501 carries every state: `state=idle` at a fresh prompt, `working` while it thinks, streams or runs a command, `done` when a turn finishes, `blocked` (`kind=permission`) on an approval prompt, and `clear` on exit. The rules map `blocked`, `working`, and `idle`/`done`/`error` straight across.
  - Two dialogs are read from the screen, ahead of OSC 7501, by the key hint on the bottom non-blank row (`bottom:1`), which model output can't reach because the footer holds it otherwise:
    - the approval prompt (`Would you like to run the following command?` … `Press enter to confirm or esc to cancel`), should OSC 7501 not flag it;
    - the folder trust dialog (`Trust this folder?` … `enter continue · esc back`), which comes before any OSC 7501.
  - The OSC 0 title is the folder name, with a Braille spinner in front while working and `[ ! ] Action Required |` in front when blocked. Users can choose what the title shows, so no rule reads it. Codex sends no OSC 9;4.
  - After `/exit` it sends OSC 7501 `clear` and prints `codex resume <session id>`; no rule matches, and the process exit publishes `exited`.
- **Tests:** every rule set has unit tests that feed **recorded terminal output** (`tests/fixtures/<agent>/*.bytes` plus the expected state) through a `Term`.
- **Recording fixtures:** `script/record-pty.py --out DIR --steps FILE -- <argv>` runs the CLI in a 120×40 PTY with `TERM=xterm-256color`, answers the terminal queries a TUI waits on, types what a steps file says (`wait <regex>`, `sleep`, `send`, `mark <name>`, `end`), and writes the raw bytes since each `mark` to `<name>.bytes`, with each mark's foreground process and OSC sequences in `marks.log`. Each fixture continues the one before it, so a test feeds a chain in order. To re-record `codex`, run the `*.steps` files beside its fixtures, each of which names its flags:
  - Run it in a throwaway git repo (`mktemp -d /tmp/agentdeck-codex-XXXXXX`: no `.` in the path, since `-c` splits its key on dots), never a real one, with trivial prompts; it uses the account's quota.
  - Pass everything as flags; don't edit `~/.codex/config.toml`. `-a on-request -s read-only` plus a prompt asking for a command with escalated permissions forces an approval prompt (0.169.0 dropped `untrusted`). `-c 'projects.<dir>.trust_level="trusted"'` skips the trust dialog; the quoted TOML key form doesn't. Record the trust dialog with no flags and end it without answering, since answering saves the choice to `config.toml`.
  - Sleep a second before sending Enter, or Codex takes the text and the Enter as one paste.
  - Wait on text the prompt itself doesn't contain.
  - To re-record `claude`, run its `*.steps` files the same way, each naming its flags, with an allowlisted environment (`env -i PATH=… HOME=… USER=… SHELL=… LANG=…`). Never pass `CLAUDECODE` or `CLAUDE_CODE_*` through from a Claude Code session.
    - `--setting-sources project,local --strict-mcp-config` keeps the user's settings, hooks, plugins, status line and MCP servers out of the recording; `--model haiku` keeps it cheap; `--permission-mode manual` makes a Bash command that writes (`touch a.txt`) ask, since auto mode was on by default and read-only commands like `echo` never ask; `--session-id <uuid>` launches it as agentdeck does.
    - Claude Code has no flag to skip the trust dialog, and answering `Yes, I trust this folder` saves it to `~/.claude.json`. Trust is inherited from a trusted parent folder, but inside a git repo only up to the repo's root, so record the trust dialog in a fresh `mktemp -d` git repo and end it without answering, and record the sessions in a `mktemp -d -p <a folder trusted once>` that isn't a git repo.
    - Wait on single words (`proceed`, `confirm`), since Claude Code moves the cursor between words, and on the title (`\x1b\]0;✳`, `\x1b\]0;(?!✳)`) for a turn's start and end.
  - Scrub before committing: the temp dir's random suffix (also where the renderer split it with a cursor move), session ids (to `00000000-0000-7000-8000-000000000001` for `codex`, `00000000-0000-4000-8000-000000000001` for `claude`), and any home path, user or account name. Keep the bytes otherwise as recorded; trimming is only cutting a chain's last fixture at a frame end (`ESC [ ? 2026 l`).

## 8. PR sync

- The token comes from `gh auth token` at start and again after a 401; while there is none, every sync asks `gh` again. No `gh`, a `gh` that isn't logged in, or a token GitHub still rejects after that retry means `GET /v1/prs/status` reports `logged_out`, and the GUI shows a banner.
- A sync every 120 s (the first at start), and on `POST /v1/prs/sync`, which clients call when a task is opened. One sync runs at a time. `AGENTDECK_GITHUB_API` replaces the GraphQL URL (`https://api.github.com/graphql`); tests point it at httpmock.
  - **Age cutoff:** only PRs GitHub updated in the last `max_age_days` days sync: the setting `pr_sync.max_age_days` (§2), default `90`, `0` for no cutoff. Each sync reads it as it begins, so a PATCH applies to the next sync. `AGENTDECK_PR_MAX_AGE_DAYS` overrides the file (tests use it); empty is the same as unset, and a value that isn't a count of days is logged and ignored. The cutoff is that many days before the sync began.
  - **Full:** the first sync, and the first after an hour since the last full one (`AGENTDECK_PR_FULL_SYNC_SECS`, `0` for every sync), in two phases. First the list `viewer.pullRequests(states: OPEN)`, 100 a page, most recently updated first, with light fields only (no review, no checks); each page is stored as it arrives, a stored PR keeping its review and checks. The list stops at the first PR updated before the cutoff: neither it nor anything after it is stored, and no later page is read. Then every field, fetched by number, for the PRs that are new, that GitHub updated since they were stored, or whose checks are pending.
  - **Incremental:** the rest search `is:pr author:@me updated:>=<T>`, where `T` is 10 minutes before the last good sync began, so a merged or closed PR comes back in its new state. A merged or closed PR not stored as open is skipped, and so is any PR updated before the cutoff. Then open PRs whose checks are `pending`, which a finished check run doesn't bring into the search, are fetched by number, and inbox PRs are matched to workspaces again as stored; aged-out PRs are left out of both. Nothing is found gone.
- `POST /v1/prs/sync` answers once its sync finishes, with the same body as `GET /v1/prs/status`: `{ login: "logged_in"|"logged_out", last_synced_at, last_error, progress, max_age_days }`. `max_age_days` is the age cutoff the next sync uses (`0` for none), so clients never read `config.toml` themselves. A failed sync is still 200, with `last_error` set: `GitHub answered <status>`, plus GitHub's `message` when the body is JSON. A request that fails with a 5xx or in transit is tried up to 3 times, 1 s then 2 s apart.
- Each result becomes a PR task, titled with the PR's title:
  - Its **workspace** is the oldest unarchived workspace whose dir holds a clone of the PR's repo with `head_ref` present as a local branch. A clone is a git repo up to 3 levels under the dir (as the dirty report searches) with any `github.com` remote (https, ssh or scp-like URL) naming the repo, compared case-insensitively. If no workspace matches, it goes to the inbox (`workspace_id` NULL). Only an inbox task is matched again on later syncs; a PR task never leaves its workspace.
  - **New PR:** a PR task created with status `review`.
  - **Merged or closed:** the task goes to `done`; reopened, back to `review`. While the PR's state holds, the task keeps whatever status it has. In a full sync, PRs that left the list are fetched by number, once; one GitHub no longer shows is stored as closed. Fetches by number go 25 to a request, 4 requests at a time.
  - **Aged out:** in a full sync, a PR stored as open that left the list and was last updated before the cutoff is not fetched. It is aged out instead (`aged_out_at` set), once: its task goes to `done` (a `changed` event, as any status change sends) and stays findable under Show done; a status the user picks after that sticks. Nothing changes on GitHub. When GitHub next shows it within the cutoff, it is no longer aged out and its task goes back to `review`, as a reopened PR's does.
- `checks` and `review` come from `statusCheckRollup` (`ERROR` counts as failure, `EXPECTED` as pending) and `reviewDecision`. Every task in the API carries its PR as `pr` (null for a plain task).
- A sync stores each page as it arrives, and each task it creates or changes gets a `changed` event (§5) then. While it runs, the status's `progress` is `{ prs_done, prs, stage }` (`prs` from the search's count, `0` until the first page answers; `stage` is `listing`, then `details` while reviews and checks are fetched by number), and every change to it sends a `pr_sync` event. Clients show it: "Syncing… 100 of 271 PRs", then "Syncing details…".

## 9. Comments and instructions

**Task comments** are a thread on each task, every comment with its author.
- **Your comments** (no `pane` on create) go to the task's agent:
  - When a live agent pane is linked to the task, the comment is typed with Enter (`<body>\r`) into the newest such pane once it is `idle` or `done`. Unlike `input` `when: "idle"`, it never goes into a `blocked` pane, so it can't answer a dialog. It is marked delivered (time and pane) when typed, and stays undelivered if the process exits first.
  - Otherwise it goes into the next launch prompt for the task (§6).
  - Linking a live agent pane to the task (`PATCH /v1/panes/{id}`) hands it the task's comments still undelivered, oldest first, in the same way: each typed once the pane is `idle` or `done`, and marked delivered then. A shell pane linked to a task gets none.
  - A comment reaches one agent at most once. While it waits for a pane's prompt it is held for that pane alone. Linking another agent pane to the task moves it to that pane only if it is the newer of the two (or the holder is no longer linked), so it still goes to the newest agent pane; the pane that lost it never types it. A launch prompt (§6) takes it from a pane holding it. One already typed or prompted is handed to no pane.
  - Yours are seen when written.
- **Agent comments** are written through the CLI or MCP from inside a pane (`pane` on create, from `$AGENTDECK_PANE`): progress, "blocked on X", "PR opened", a summary when done. They are never delivered. Each is new to the user (`seen_at` NULL) until `POST /v1/tasks/{id}/comments/seen`, which the GUI calls when the user opens the task; a task's `unseen_agent_comments` counts them.

**Workspace instructions** are standing context for every agent in a workspace ("Push only after 20 green runs").
- Every launch prompt in the workspace carries all of them (§6). They are never consumed: there is no read or unread, and they stay until deleted. They can be edited.
- **Send to agent:** the GUI types one into a chosen pane at once (typed with Enter, `when: "now"`), as `agentdeck agent send --now` does. The instruction is unchanged.

## 10. CLI and MCP

`agentdeck` (clap) reads `daemon.json` and starts the daemon when it isn't running: no file, a pid that isn't running, or a port that refuses the connection. It starts `$AGENTDECK_DAEMON` if set, else the `agentdeckd` beside its own executable, else `agentdeckd` from `PATH`, in its own process group with no terminal, and waits up to 10 s for a `daemon.json` that answers `/v1/health`. Output is human-readable, or with `--json` the API's own objects (`ws rm` prints `{id, deleted}`, `tail` prints `{pane, output}`). A failure prints `agentdeck: <message> (<error_code>)` to stderr and exits 1; `agent new`'s `invalid_argv` prints the message alone (§6).

```
agentdeck ws ls | new <name> [--dir PATH] | archive <ws> | mute <ws> | unmute <ws> | rm <ws> [--force]
agentdeck task ls [--status S] [--ws WS] [--inbox] | add <title> [--ws WS] [--body B] | set <id> --status S | show <id>
agentdeck comment ls --task T | add <body> --task T
agentdeck instruction ls [--ws WS] | add <body> [--ws WS] | edit <id> <body> | rm <id>
agentdeck agent new --ws WS [--agent claude|codex] [--cwd DIR] [--task T] | send <pane> <text> [--now] | link <pane> (--task T | --none)
agentdeck pr sync | status
agentdeck config ls | get <key> | set <key> <value>
agentdeck tail <pane> [--bytes N]
agentdeck mcp            # MCP server over stdio
```

- A `WS` or `<ws>` is a workspace's id, slug or name, tried in that order; a name two workspaces share is an error that lists their slugs.
- `ws new --dir` takes a path relative to the current directory too; the CLI sends it absolute.
- `ws rm` without `--force` on a dirty dir prints the dirty report and exits 1.
- `agent new` launches the agent with `POST /v1/panes` (no `argv`: the daemon builds it and the launch prompt) in `--cwd`, made absolute, or the workspace's dir. The pane goes in the workspace's first tab with no panes, else in a new tab titled after the agent, and becomes that tab's layout. A tab made for a launch that fails is deleted.
- `agent send` types the text followed by `\r` (Enter), with `when: "idle"`, or `"now"` with `--now`.
- `agent link` links a running pane to a task with `PATCH /v1/panes/{id}`, or unlinks it with `--none`; it takes exactly one of the two. It prints `linked <pane> to <task>` or `unlinked <pane>`, and the pane with `--json`. MCP has no tool for it in v1.
- `comment add` run with `$AGENTDECK_PANE` set is authored by that pane's agent; otherwise it is yours. `comment ls` prints who wrote each comment (`you` or the agent) and whether yours reached an agent (`delivered` or `waiting`).
- `config` names a setting by its dotted key (§2). `ls` prints each key and value, and `set` the key it set, adding "(an environment variable overrides config.toml)" to an overridden one; with `--json`, both print the settings object. `get` prints the value alone, or `{key, value}` with `--json`; a key that isn't a setting is an error. `set` sends a value that reads as a number, `true` or `false` as that, anything else as text, and the daemon checks it (`invalid_setting`).
- `instruction` without `--ws` works on `$AGENTDECK_WORKSPACE`, and is an error outside a workspace. `instruction rm` prints `{id, deleted}` with `--json`.

**MCP tools** (rmcp): `tasks_list`, `task_get`, `task_create`, `task_update`, `task_comments { task }`, `task_comment { task, body }`, `instructions_list { workspace? }`, `workspaces_list`, `prs_status`.
- `AGENTDECK_PANE` and `AGENTDECK_WORKSPACE` are set in every pane's environment. They are added on top of the allowlist.
- Tools default to the caller's workspace and pane:
  - `tasks_list` lists the caller's workspace unless given a `workspace`; it takes `inbox: true` for the inbox and `all_workspaces: true` for everything. Outside a workspace it lists everything.
  - `task_create` goes to the caller's workspace unless given one.
  - `task_comment` is authored by the caller's pane, and so never delivered to an agent.
  - `instructions_list` lists the caller's workspace unless given a `workspace`; outside a workspace, with none given, it is an error.
- Each tool answers with the API's JSON object as text. A failure (a bad argument, or the daemon's error with its `error_code`) is a tool result with `isError: true`, not a protocol error.
- Each call reaches the daemon the way the CLI does, so a daemon restarted under a long-lived `agentdeck mcp` is found again.

## 11. GUI

Built on `brrritscold-core-gpui` (`init`, `Root`, theme preference, toasts). Screens follow the Penpot page design. The design is made before GUI work starts; until then, this list is the outline.

**Dark mode is first class.** Every screen is designed, built and checked in Dark first, then Light, to the same standard. Every GUI capture set includes both modes. The terminal palette follows the active mode. The theme preference defaults to `System` (follows the OS appearance); Settings and the command palette offer System / Dark / Light, saved in `config.toml` as `appearance.theme` (§2). The GUI reads it with `GET /v1/settings` as it connects and applies it, and reads it again on each `changed settings`.

- **Title bar:** the window asks for the compositor's frame. Where the compositor draws none (GNOME's Mutter on Wayland), the app draws a 32px title bar across the top: drag to move, double-click to maximize, right-click for the window menu, minimize / maximize / close at the right. 4px resize strips run along the window's inside edges, except on a tiled side. Everything else sits under it.
- **Window:** it opens at 1440×900, the design's size, centred on the primary display and shrunk to fit its work area. Its size, position and maximized state are kept across restarts in `$XDG_STATE_HOME/agentdeck/window.json` (GUI-only, apart from `config.toml`), written once a resize or move settles and on quit. A missing or unreadable file, or saved bounds whose centre is on no connected display, opens at the default instead.
- **Sidebar:** workspaces, each with a rolled-up state (any blocked → blocked, else any working → working, else any done → done) and its open-PR count. Inbox and Tasks entries sit at the top, under the brand. The workspaces sit under a **Workspaces** header whose `+` (ghost, icon-xs, as on the right panel's Tasks & PRs header) opens the new workspace dialog. Below the workspaces, an **Archived** group header (chevron, "Archived", a count) lists the archived workspaces, oldest first: rows with `muted-foreground` labels and no counts. It is folded at first, a click toggles it, it stays open while it holds the selected workspace, and it is hidden while nothing is archived. The footer says when PRs last synced, and ends, pushed right, in a ghost icon-xs `Settings2` button that opens Settings; no row is highlighted while Settings shows.
- **Workspace view:** a top bar with the workspace's name and dir, New agent, a `…` workspace menu and the right panel toggle; under it, tabs, with split panes inside each. Each pane has a header showing its title, state and linked task, and a `…` menu: Split right, Split down, then for an agent pane (not a shell) **Link to task…**, then, after a separator, Close pane. A `+` after the tabs opens a shell tab in the workspace dir; a workspace with no tabs when shown gets one.
  - **Link to task…** (Penpot "Screens" → "Link to task") opens a dialog titled "Link <Agent> to a task", with "The agent in the <tab> tab of <workspace>." Its one Field, "Task", is a Select of None and the workspace's tasks that aren't done, preset to the pane's link, described "Comments on it that no agent has had yet are typed in once <Agent> is idle. None unlinks it." The footer is Cancel and **Link**, which links the pane with `PATCH /v1/panes/{id}` (None unlinks it) and closes. A failure keeps it open and toasts "Couldn't link <Agent>" with the daemon's reason.
  - **Mute notifications**, first in the `…` menu (Penpot "Screens" → "Workspace menu"), mutes the workspace's notifications with `PATCH /v1/workspaces/{id}` `{muted: true}`; once muted it reads **Unmute notifications**. A failure toasts "Couldn't mute <name>" (or unmute) with why. A muted workspace's sidebar row shows a 12px `BellOff` in `muted-foreground` before its counts; an archived row shows none.
  - **Archive workspace**, in the `…` menu, archives at once, with no dialog, since it can be undone. The daemon refuses while a pane runs, and a toast "Can't archive <name>" gives its reason ("Close its 2 running panes first"). Otherwise the toast "Archived <name>" says "Its folder, tasks and instructions are kept." and offers **Undo**, which unarchives. The workspace stays selected, now archived.
  - **Delete workspace…**, after a separator and in `destructive`, opens the delete dialog.
  - **An archived workspace** shows an outline **Unarchive** in place of New agent, and its menu has only Delete workspace…. In place of its tabs and panes it shows "<name> is archived" over "Unarchive it to open its tabs and start agents here again." It attaches no terminals and opens no shell tab.
- **Right panel**, which can be toggled: first the workspace's **Instructions**, newest first, under a header whose `+` focuses the field "Add an instruction for agents here…" below them (Enter adds it); then, below a separator, its tasks and PRs, as a list grouped by status under a header whose `+` opens the new task dialog, ending in "Show N done".
  - Each instruction is a card (Penpot `agentdeck/InstructionItem`): its text, then a meta row, "added 2 h ago" (xs, muted), ending with a ghost `…` menu. Instructions have no read state, so there is no unread badge or count anywhere.
  - **Send to agent:** the menu starts with the label "Send to agent", then an item per live (not exited) agent pane in the workspace, "<Agent> · <linked task title, else tab title>"; with none, one disabled "No agent is running here". Picking one types the instruction into that pane at once as `agentdeck agent send --now` does (§9); a failure toasts why.
  - **Edit**, after a separator, turns the card into a textarea holding the text, over **Cancel** and **Save**. Enter inserts a newline; Ctrl+Enter or Save saves it with `PATCH /v1/instructions/{id}`, Escape or Cancel drops the edit. Save is disabled while the text is blank. A failed save keeps the editor open and toasts why.
  - **Delete**, after a separator and in `destructive`, deletes the instruction at once with `DELETE /v1/instructions/{id}`, with no confirmation; a failure toasts why.
- **Settings** (Penpot "Screens" → Settings): a main-window view like the Tasks view, headed "Settings" over "Saved in ~/.config/agentdeck/config.toml. Changes apply at once." Its sections are 480px wide, 32px apart under 8px, each a heading (base, semibold) over its fields, 16px apart. It shows the settings from `GET /v1/settings` and reads them again on each `changed settings`, so another client's change shows, and its theme applies.
  - **Appearance:** Theme, a Select of System / Dark / Light, "System follows your desktop's light or dark setting." Picking one applies it at once and saves it with `PATCH /v1/settings`; a failed save puts the saved theme back and toasts why.
  - **GitHub:** "Sync PRs updated in the last (days)", an input holding `pr_sync.max_age_days`, "0 syncs every open PR. Older PRs already here stay until you mark them done." Enter, or leaving the field, saves it; text that isn't a whole number of days, 0 or more, shows the field's error and isn't saved, as does the daemon's `invalid_setting`. Any other failure toasts why.
  - **Terminal:** "Open file paths with", an input holding `terminal.file_opener`, "Ctrl+click a file path in a pane to run this. {path} is the file and {line} its line, when the path has one (src/main.rs:42). For example: code -g {path}:{line}". It saves as the days field does: Enter or leaving the field. A blank command ("Enter a command, such as xdg-open {path}."), an unclosed quote ("Close the quote in the command.") or one with no `{path}` ("Pass {path} to the command, as in code -g {path}:{line}.") shows the field's error and isn't saved, as does the daemon's `invalid_setting`. Any other failure toasts why.
- **Tasks view:** a grouped list of every task, with a filter, a Show done switch and **New task**. Done tasks are hidden by default, and each row has a status picker.
- **Table width:** the Tasks view and the Inbox fit their columns to the width instead of scrolling sideways (Penpot "Screens" → "Tasks · 1200", "Inbox · 1200"). The title column takes what's left, down to 200px, its text wrapping; below that a column drops, header and rows together. Tasks drops **Agent**, then **Checks**; Review and Workspace always stay. The Inbox drops **Checks**, then **Review**. At a 1200px window Tasks shows neither Agent nor Checks and the Inbox shows every column. The Tasks header's filter is 180px wide, so the filter, Show done and New task fit at 1200. Only below the narrowest layout does the table scroll sideways.
- **Task sheet:** clicking a task's title in the Tasks view, the Inbox or the right panel opens a `Sheet` from the right (core-gpui's default right sheet width).
  - The header is the task's title over "<Agent> is working on it." while a live agent pane is linked to it, else "No agent on it."
  - The body (Penpot `agentdeck/TaskSheetBody`, `px-4`, `space.4` apart): the status picker and "in <workspace>" (or "in the Inbox"); the description (sm, muted), if any; a PR task's PR line (repository and number, draft, checks, review, as its row shows them); a "Comments" heading (sm medium) and the thread, oldest first. Each comment is its author ("You", or the agent's name after its glyph, "✳ Claude Code"), " · 2 h ago" (xs, muted), then the text (sm); yours add "Delivered to <Agent>" or "Waiting for the agent" (xs, muted).
  - **Start agent:** while no live agent pane is linked to the task ("No agent on it."), the status row ends, pushed right, in an outline sm **Start agent** (Penpot "Screens" → "Task sheet · no agent"). It closes the sheet and opens New agent on the task's workspace, which need not be the one shown, with the task picked; once the agent starts, the window shows that workspace and focuses the new pane. For an Inbox task it is disabled, with the tooltip "Move this task to a workspace to start an agent on it."; in an archived workspace, "Unarchive <name> to start an agent on it." It is hidden while an agent works on the task.
  - The footer (Penpot `agentdeck/CommentComposer`) is a Textarea "Comment for the agent on this task…" over "Ctrl+Enter to send" (xs, muted) and a sm **Comment** button at the end of the row, disabled while the text is blank. Enter inserts a newline; Ctrl+Enter or Comment sends it as your comment (§9) and empties the composer. A failure keeps the text and toasts why.
  - Opening the sheet reads the thread and marks the task's agent comments seen (`POST /v1/tasks/{id}/comments/seen`). While it is open, every `changed comment` reads the thread again, and an agent comment that arrives is marked seen too.
- **New from the agent:** a task row in the Tasks view, the Inbox and the right panel shows a 14px lucide `MessageSquareDot` in `foreground` while the task's `unseen_agent_comments` is above 0: at the end of the title cell in the Tasks view and the Inbox, just before the agent's state mark in the right panel. It is a speech bubble, not a dot, so it can't be read as a state mark in either mode.
- **Inbox:** tasks with no workspace, mostly PRs.
  - **Description:** two lines under the heading, both sm `muted-foreground`: "Tasks with no workspace. A PR lands here when no workspace has a clone of its repo.", then the age cutoff from `GET /v1/prs/status` (§8): "PRs not updated in the last N days are left out; change that in Settings." ("1 day" for 1), or "Every open PR syncs." for `0`. Until the status is read only the first line shows. It shows while gh is logged out too. Its "Settings" is a link (`primary`, underlined on hover, as core-gpui's link button) that opens Settings.
  - **gh logged out:** while `GET /v1/prs/status` says `logged_out`, a default alert with the `Info` icon sits between the Inbox's header and its columns: "GitHub isn't connected" / "PR sync uses the gh login. Run gh auth login in a terminal, then press Sync now." It goes once the login is back.
  - The header's sync label and the sidebar's footer read "gh logged out" then, over a failed sync or an earlier one; a running sync still shows its progress, so pressing Sync now after logging in answers at once.
- **Scrollbars:** the Tasks view, the Inbox, the right panel and the sidebar's workspace list show a vertical scrollbar while their content is taller than they are, and none when it fits: a 10px track over the list's right edge holding an 8px, fully rounded thumb in `border` (Penpot "Screens" → Inbox, as shadcn's `ScrollArea` thumb). It shows at rest, not only while scrolling; dragging the thumb or clicking the track scrolls. There is no horizontal scrollbar: below their narrowest layout, the Tasks view and the Inbox scroll sideways by a sideways wheel only. Their scrollbar sits outside that sideways scroll, over the right edge of the part of the list in view, so a window narrower than the table still shows it.
- **Dialogs:** new workspace, new agent (agent, cwd, task), new task, and delete workspace:
  - Enter in a dialog's single-line field does what its primary button does, and only that: when the form can't go ahead, or the daemon refuses it, the dialog stays open.
  - **New workspace** has a Name and an optional Existing folder. Enter in either creates. A blank name, a relative folder, or the daemon's `invalid_name` / `invalid_dir` shows under the field it names; any other failure toasts. Once made it closes.
  - **New task** is titled "New task", with "Add a task to a workspace, or to the Inbox.": a Title ("What needs doing?", required), a Description (optional) and a Workspace select of the unarchived workspaces plus "Inbox (no workspace)". The Tasks view's **New task** opens it on the Inbox, the right panel's Tasks & PRs **+** on that workspace (the Inbox for an archived one). **Create task** is disabled while the title is blank; Enter in the title does the same. It makes a `todo` task with `POST /v1/tasks` and closes; the task comes back through `changed`. A failure keeps it open and toasts why.
  - **Delete workspace** is titled "Delete <name>?", with "This can't be undone. Archive it instead to keep its folder."
  - It reads the dirty report on opening, showing "Checking for unpushed work…" meanwhile. When repos are dirty, a destructive alert "N repos have work that isn't pushed" / "Deleting removes the folder and everything in it." sits over a card per repo: its path under the workspace, "N uncommitted files in <the folder they share>" and "<branch>: N commits not pushed" ("1 commit") for each unpushed branch.
  - Then "Its N tasks move to the Inbox. Its N instructions are deleted." and "Type <name> to confirm" over an input.
  - The footer is **Archive instead** (absent once archived), which closes it and archives as the menu does, **Cancel** and the destructive **Delete workspace**. Delete is disabled until the input is the name exactly. It deletes with `force`, since the report has been shown; Enter does the same, and Escape cancels.
  - A failed delete keeps the dialog open and toasts why. Once deleted, the selection falls back to the first workspace left, else the Inbox.
- **Terminal:**
  - `alacritty_terminal` per attached pane, drawn by our own renderer.
  - **Zed's GPL-3.0 terminal crates are never copied.**
  - The font is Inter for the UI. The terminal gets a monospace font with a symbols fallback for agent glyphs (`⎿`, `✻`, `◐`).
  - The mouse, after xterm: a program that asks for mouse reports (1000 / 1002 / 1003, SGR 1006) gets clicks, drags and the wheel; shift takes them back for selecting and the scrollback. Otherwise, on the alternate screen, the wheel sends an arrow key per line (1007).
  - **Links:** `secondary` (ctrl; cmd on macOS) and a left click opens the link under the pointer with the OS's handler; a plain click selects as before. A link is an OSC 8 hyperlink (`gh`, `ls --hyperlink`, Claude Code), else an `http://` / `https://` URL in the text, found across soft wraps, without the trailing `.`, `,`, `:`, `;`, `!`, `?`, quotes and unopened `)` / `]` that end a sentence around it.
    - It opens even while the program has the mouse (vim, tmux, Claude Code), and the program hears nothing of that click. A `secondary`-click off a link is reported as before.
    - Only `http`, `https` and `mailto` open with the OS's handler. A `file://` link opens through `terminal.file_opener` (below) when its host is empty, `localhost` or this machine's hostname and it names an existing regular file, with no confirmation, since the command is one the user set. Any other scheme, or another host's file, is refused silently (a debug log line), so a program can't make a click run a custom URL handler.
    - While `secondary` is held over a link, its cells are underlined and the pointer is a pointing hand, until the key is let go or the pointer leaves.
    - **File paths:** a run of text with no spaces, quotes or brackets that holds a `/` or a `.` (`src/main.rs`, `./a/b.txt`, `/abs/path`, `~/x`, `Cargo.toml`), with an optional `:line[:col]`, found across soft wraps and without the sentence punctuation around it, as a URL is. Anything after the column (`src/main.rs:42:fn main`) isn't part of it. A relative path resolves against the directory the shell last reported with OSC 7 (`file://host/path`, for this host), else the pane's launch dir; `~/` against `$HOME`. Only an existing regular file is a link: no underline and no hand otherwise. A URL or OSC 8 link over the same text wins.
      - The GUI reads the OSC 7 directory from the pane's output, the attach snapshot included: the daemon keeps each pane's last OSC 7 URI, as sent, and ends every snapshot with it (§5), so a reattach (a GUI restart, a `lagged` re-attach, switching back to a workspace) resolves against the shell's directory at once.
      - A click runs `terminal.file_opener` (§2) with `{path}` the absolute path and `{line}` / `{col}` from the text, `1` when it has none. The command is split into words before the placeholders are filled, and started straight from those words, never through a shell, detached in its own process group, from the GUI's process, on the user's desktop. A command that doesn't start toasts "Couldn't open <file name>" with why.
  - Input methods and dead keys: a composition shows underlined at the cursor, and only committed text reaches the pane. The candidate window opens at the cursor.
- **Colours** use semantic tokens only.
  - Agent state: working `info`, blocked `destructive`, done `success`, idle/unknown/exited-0 `muted-foreground`, exited ≠ 0 `destructive`.
  - Blocked and a failed exit share red, as in herdr: the label (`blocked` vs `exited 1`) and the StateDot shape (a filled dot vs a red `OctagonX`) tell them apart, so colour is never the only signal.
  - Checks: passing `success`, pending `info`, failing `destructive`.
  - Review: approved `success`, changes requested `warning`, required `muted-foreground`.
  - Task statuses aren't coloured.
  - The terminal palette follows the theme. Terminal text is JetBrains Mono (`font-family.mono`).
- **Notifications:** the GUI shows OS notifications through the freedesktop notifications D-Bus API, named "agentdeck", with no icon, no hints and the server's own timeout; only while it runs, and the daemon sends none. Nothing about them is configurable in v1: no global mute, quiet hours or sounds.
  - **One connection:** `agentdeck-notify` holds one session-bus connection for the whole app and one task on GPUI's foreground that sends each `Notify` and reads the server's `ActionInvoked` and `NotificationClosed`, by the id the server gave each notification. A notification left open costs an entry in that id map, never a thread or a connection; it leaves the map once clicked or closed. (GPUI's own `show_system_notification` starts a thread and a connection per notification and holds both until the server closes it, #133.) Without a session bus the GUI logs why once and shows none.
  - **Panes:** an agent pane (not a shell) turning `blocked` or `done` while the user can't see it: its tab isn't the one shown, its workspace isn't, or the window isn't focused. Title "<Agent> is blocked" / "<Agent> is done"; body "<linked task title, else tab title> · <workspace>". Once per state change, not again while the state holds.
  - **PRs:** a PR task's checks turning `failure`, or its review turning `approved` or `changes_requested`. Title "Checks failed" / "Approved" / "Changes requested"; body "<repo> #<n> · <title>". Changes are held while a PR sync runs and shown when it ends; more than 3 PRs changed in one sync make one "N PRs changed" whose body lists their "<repo> #<n>".
  - Only changes between two reads count: what the GUI first reads after connecting (a pane already blocked, a check already failing) notifies nothing.
  - **Muted:** a muted workspace's panes and PR tasks never notify; an Inbox PR has no workspace, so it always can.
  - **Click:** clicking a notification's body focuses the window and shows what it is about: the pane, in its workspace and tab; the task sheet on a PR task; the Tasks view for "N PRs changed". Notifications offer no buttons.
- **Keymap:** `keymap.rs`, normal app shortcuts, `secondary-` bindings loaded with `KeyBinding::load`.
  - A binding wins over the terminal, so every shortcut is one a shell has no use for, after GNOME Terminal. Plain ctrl-letter, alt-letter, the arrows and the paging keys always reach the pane.
  - Quit `secondary-shift-q`; next / previous tab `ctrl-pagedown` / `ctrl-pageup` (or `ctrl-tab` / `ctrl-shift-tab`); tab 1–9 `alt-1`…`alt-9`; next / previous pane `secondary-shift-down` / `secondary-shift-up`, in layout order; new shell tab `secondary-shift-t`; split the focused pane with a shell right / down `secondary-shift-e` / `secondary-shift-o` (after Terminator); close it `secondary-shift-w`, asking first while it runs. Closing a tab's last pane closes the tab.

## 12. Testing

| Tier | Where | What |
|---|---|---|
| unit | `agentdeck-core`, GUI views (`#[gpui::test]`), daemon pure parts | rules with recorded output, layout tree, snapshot encode/decode round-trip, env allowlist, `openapi.json` staleness |
| integration | `agentdeckd/tests/integration` | temp `$XDG_*` dirs, a real daemon on a random port, a real client; PTYs run `sh`; GitHub is httpmock; `gh` is a stub script on `PATH` |
| integration | `agentdeck/tests/integration` | the real `agentdeck` binary in temp `$XDG_*` dirs, starting the real daemon itself; `agentdeck mcp` driven over stdio with hand-written JSON-RPC; panes run `sh`, never a real agent CLI |
| integration | `agentdeck-notify/tests/integration` | the desktop notifier against a fake notification server on a private `dbus-daemon` session bus, the app side (GPUI's headless platform) in a child process: what it sends, one connection and no thread per notification, a click routed by id, a closed notification forgotten. The GUI's unit tests stand in a fake `Notifier` for it |

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
