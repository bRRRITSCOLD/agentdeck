//! Human-readable output. `--json` prints the API's own objects instead.

use agentdeck_client::settings;
use std::collections::{BTreeMap, HashMap};

use agentdeck_client::types::{
    Comment, DirtyReport, Instruction, McpAgent, McpRegistration, McpState, Pane, PrCloseResult,
    PrSyncStage, PrSyncStatus, PullRequest, Settings, Task, Workspace,
};
use agentdeck_core::{CommentAuthor, PaneKind, TaskId, WorkspaceId};
use serde::Serialize;
use serde_json::Value;

/// Left-aligned columns two spaces apart, under a header row; `empty` when there are no rows.
fn table<const N: usize>(headers: [&str; N], rows: &[[String; N]], empty: &str) -> String {
    if rows.is_empty() {
        return format!("{empty}\n");
    }
    let mut widths = headers.map(|header| header.chars().count());
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: [&str; N]| {
        let mut line = String::new();
        for (index, cell) in cells.iter().enumerate() {
            if index + 1 == N {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  ", width = widths[index]));
            }
        }
        line.truncate(line.trim_end().len());
        line.push('\n');
        line
    };
    let mut out = line(headers);
    for row in rows {
        out.push_str(&line(row.each_ref().map(String::as_str)));
    }
    out
}

/// The first line of `text`, marked with `…` when there is more.
fn first_line(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

/// `agentdeck ws ls`.
pub fn workspaces(workspaces: &[Workspace]) -> String {
    let rows: Vec<[String; 4]> = workspaces
        .iter()
        .map(|ws| {
            let name = match ws.archived_at {
                Some(_) => format!("{} (archived)", ws.name),
                None => ws.name.clone(),
            };
            [ws.id.to_string(), ws.slug.clone(), name, ws.dir.clone()]
        })
        .collect();
    table(["ID", "SLUG", "NAME", "DIR"], &rows, "no workspaces")
}

/// One workspace, after `ws new`.
pub fn workspace(ws: &Workspace) -> String {
    format!("{}  {}  {}\n", ws.id, ws.slug, ws.dir)
}

/// The PR behind a task as `owner/repo#number`, or nothing.
fn pr_label(task: &Task) -> Option<String> {
    task.pr
        .as_ref()
        .map(|pr| format!("{}#{}", pr.repo, pr.number))
}

/// `agentdeck task ls`.
pub fn tasks(tasks: &[Task]) -> String {
    let rows: Vec<[String; 3]> = tasks
        .iter()
        .map(|task| {
            let title = match pr_label(task) {
                Some(pr) => format!("{pr} {}", task.title),
                None => task.title.clone(),
            };
            [task.id.to_string(), task.status.to_string(), title]
        })
        .collect();
    table(["ID", "STATUS", "TITLE"], &rows, "no tasks")
}

/// `agentdeck task show`, and a task after `task add` or `task set`.
pub fn task(task: &Task) -> String {
    let mut out = format!(
        "{}\n  id:        {}\n  status:    {}\n  workspace: {}\n",
        task.title,
        task.id,
        task.status,
        task.workspace_id
            .map_or_else(|| "inbox".to_owned(), |id| id.to_string()),
    );
    if let Some(pr) = &task.pr {
        // A PR the GitHub sign-in can't see is shown as it was last synced (spec §8).
        let reach = if pr.unreachable { ", unreachable" } else { "" };
        out.push_str(&format!(
            "  pr:        {} ({}{reach}, checks {}, review {})\n",
            pr.url,
            pr.state,
            pr.checks.map_or("none", |checks| checks.as_str()),
            pr.review.map_or("none", |review| review.as_str()),
        ));
    }
    if !task.body.is_empty() {
        out.push('\n');
        out.push_str(&task.body);
        out.push('\n');
    }
    out
}

/// A pane after `agent new`: its id, agent and dir, and its session once it has one. The
/// argv, launch prompt included, is in `--json`.
pub fn pane(pane: &Pane) -> String {
    let agent = pane.agent.map_or("shell", |agent| agent.as_str());
    let mut out = format!("{}\n  agent:   {agent}\n  cwd:     {}\n", pane.id, pane.cwd);
    if let Some(session) = &pane.session_id {
        out.push_str(&format!("  session: {session}\n"));
    }
    out
}

/// One pane as `agent ls` lists it: the pane, and the workspace, tab and task it is in. With
/// `--json` it is the pane's own object plus `workspace_id`, `workspace` (the slug), `tab`
/// (its title) and `task_title`.
#[derive(Debug, Serialize)]
pub struct ListedPane {
    /// The pane, as the API gives it.
    #[serde(flatten)]
    pub pane: Pane,
    /// The workspace holding its tab.
    pub workspace_id: WorkspaceId,
    /// That workspace's slug.
    pub workspace: String,
    /// Its tab's title.
    pub tab: String,
    /// Its linked task's title, when it has one the daemon still lists.
    pub task_title: Option<String>,
}

/// What runs in `pane`: its agent; for a shell pane, `hosting <agent>` while it hosts one,
/// else `shell`.
fn pane_agent(pane: &Pane) -> String {
    match (pane.kind, pane.agent, pane.hosted_agent) {
        (PaneKind::Agent, Some(agent), _) => agent.to_string(),
        (PaneKind::Shell, _, Some(agent)) => format!("hosting {agent}"),
        (PaneKind::Agent, None, _) => "agent".to_owned(),
        (PaneKind::Shell, _, None) => "shell".to_owned(),
    }
}

/// `agentdeck agent ls`: a pane a line, its linked task by title, or by id when the title
/// isn't known.
pub fn panes(panes: &[ListedPane]) -> String {
    let rows: Vec<[String; 6]> = panes
        .iter()
        .map(|listed| {
            let pane = &listed.pane;
            let task = match (&listed.task_title, pane.task_id) {
                (Some(title), _) => title.clone(),
                (None, Some(id)) => id.to_string(),
                (None, None) => String::new(),
            };
            [
                pane.id.to_string(),
                listed.workspace.clone(),
                listed.tab.clone(),
                pane_agent(pane),
                pane.state.to_string(),
                task,
            ]
        })
        .collect();
    table(
        ["ID", "WORKSPACE", "TAB", "AGENT", "STATE", "TASK"],
        &rows,
        "no panes",
    )
}

/// `agentdeck comment ls`: who wrote each, whether one of yours has reached an agent, and
/// its first line.
pub fn comments(comments: &[Comment]) -> String {
    let rows: Vec<[String; 4]> = comments
        .iter()
        .map(|comment| {
            let (from, status) = match comment.author {
                CommentAuthor::User => {
                    let status = if comment.delivered_at.is_some() {
                        "delivered"
                    } else {
                        "waiting"
                    };
                    ("you", status)
                }
                CommentAuthor::Agent => (comment.author_agent.map_or("agent", |a| a.as_str()), ""),
            };
            [
                comment.id.to_string(),
                from.to_owned(),
                status.to_owned(),
                first_line(&comment.body),
            ]
        })
        .collect();
    table(["ID", "FROM", "STATUS", "BODY"], &rows, "no comments")
}

/// A comment after `comment add`.
pub fn comment_created(comment: &Comment) -> String {
    format!("{}  task {}\n", comment.id, comment.task_id)
}

/// `agentdeck instruction ls`.
pub fn instructions(instructions: &[Instruction]) -> String {
    let rows: Vec<[String; 2]> = instructions
        .iter()
        .map(|instruction| [instruction.id.to_string(), first_line(&instruction.body)])
        .collect();
    table(["ID", "BODY"], &rows, "no instructions")
}

/// An instruction after `instruction add` or `edit`.
pub fn instruction_saved(instruction: &Instruction) -> String {
    format!("{}  ws {}\n", instruction.id, instruction.workspace_id)
}

/// `agentdeck pr sync` and `pr status`.
pub fn pr_status(status: &PrSyncStatus) -> String {
    let mut out = format!(
        "github:    {}\nlast sync: {}\n",
        status.login,
        status.last_synced_at.as_deref().unwrap_or("never"),
    );
    if let Some(login) = &status.synced_as {
        out.push_str(&format!("synced as: {login}\n"));
    }
    match &status.progress {
        Some(progress) if progress.stage == PrSyncStage::Details => {
            out.push_str("syncing:   PR details\n");
        }
        Some(progress) => out.push_str(&format!(
            "syncing:   {} of {} PRs\n",
            progress.prs_done, progress.prs
        )),
        None => {}
    }
    if let Some(error) = &status.last_error {
        out.push_str(&format!("last error: {error}\n"));
    }
    out
}

/// `n` PRs, as a count: "1 PR", "2 PRs".
fn prs(n: usize) -> String {
    if n == 1 {
        "1 PR".into()
    } else {
        format!("{n} PRs")
    }
}

/// What `pr close` asks before closing `n` PRs, whose repos are `repos` (one entry per PR
/// task named): the question, a line per repo with its count, then `[y/N]`.
pub fn close_prompt(n: usize, repos: &[&str]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for repo in repos {
        *counts.entry(repo).or_default() += 1;
    }
    let width = counts.keys().map(|repo| repo.len()).max().unwrap_or(0);
    let mut out = format!(
        "Close {} on GitHub? Their repos' owners will see them closed. You can reopen a PR on \
         GitHub.\n",
        prs(n)
    );
    for (repo, count) in counts {
        out.push_str(&format!("  {repo:width$}  {}\n", prs(count)));
    }
    out.push_str("[y/N] ");
    out
}

/// `pr close`'s results: `closed <repo> #<n>` or `failed <repo> #<n>: <error>` per PR, with
/// the task's id for one that isn't a PR task, then "Closed 12 of 14" when any failed.
pub fn close_results(results: &[PrCloseResult], prs: &HashMap<TaskId, &PullRequest>) -> String {
    let mut out = String::new();
    for result in results {
        let name = prs.get(&result.task_id).map_or_else(
            || result.task_id.to_string(),
            |pr| format!("{} #{}", pr.repo, pr.number),
        );
        if result.ok {
            out.push_str(&format!("closed {name}\n"));
        } else {
            let error = result.error.as_deref().unwrap_or("unknown error");
            out.push_str(&format!("failed {name}: {error}\n"));
        }
    }
    let closed = results.iter().filter(|result| result.ok).count();
    if closed < results.len() {
        out.push_str(&format!("Closed {closed} of {}\n", results.len()));
    }
    out
}

/// The settings, a key and its value per line; only `key`'s line when one is given. A value an
/// environment variable sets says so.
pub fn settings(all: &Settings, key: Option<&str>) -> String {
    let entries = settings::entries(all);
    let width = entries
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (name, value) in &entries {
        if key.is_some_and(|key| key != name) {
            continue;
        }
        let shown = if key.is_some() {
            format!("{name}  {}", setting_value(value))
        } else {
            format!("{name:<width$}  {}", setting_value(value))
        };
        out.push_str(&shown);
        if all.overridden_by_env.contains(name) {
            out.push_str("  (an environment variable overrides config.toml)");
        }
        out.push('\n');
    }
    out
}

/// A setting's value as typed on the command line: text without quotes.
pub fn setting_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// What `ws rm` without `--force` would lose.
pub fn dirty(report: &DirtyReport) -> String {
    let mut out = String::new();
    for repo in &report.repos {
        out.push_str(&format!("{}\n", repo.path));
        for file in &repo.uncommitted {
            out.push_str(&format!("  uncommitted: {file}\n"));
        }
        for branch in &repo.unpushed_branches {
            let commits = match branch.commits {
                1 => "1 commit".to_owned(),
                n => format!("{n} commits"),
            };
            out.push_str(&format!("  unpushed:    {} ({commits})\n", branch.name));
        }
    }
    out
}

/// Claude Code and Codex, as `mcp install` names them, with where agentdeck stands in each.
fn mcp_agents(registration: &McpRegistration) -> [(&'static str, &McpAgent); 2] {
    [
        ("Claude Code", &registration.claude),
        ("Codex", &registration.codex),
    ]
}

/// Whether `mcp install` would change either agent's config.
pub fn mcp_changes_anything(registration: &McpRegistration) -> bool {
    mcp_agents(registration)
        .iter()
        .any(|(_, agent)| matches!(agent.state, McpState::Missing | McpState::Outdated))
}

/// Whether either agent's config couldn't be read or changed.
pub fn mcp_failed(registration: &McpRegistration) -> bool {
    mcp_agents(registration)
        .iter()
        .any(|(_, agent)| agent.error.is_some())
}

/// What `mcp install` does to `agent`'s config, or why it does nothing.
fn mcp_change(name: &str, agent: &McpAgent, command: &str) -> String {
    let add = format!("claude mcp add --scope user agentdeck -- {command} mcp");
    let config = &agent.config;
    match (name, agent.state) {
        ("Claude Code", McpState::Missing) => format!("run {add}"),
        ("Claude Code", McpState::Outdated) => format!(
            "run claude mcp remove --scope user agentdeck, then {add}, replacing the agentdeck \
             server it runs now"
        ),
        (_, McpState::Missing) => format!("add [mcp_servers.agentdeck] to {config}"),
        (_, McpState::Outdated) => format!("replace [mcp_servers.agentdeck] in {config}"),
        (_, state) => mcp_state(agent, state),
    }
}

/// An agent's line when `mcp install` leaves it alone.
fn mcp_state(agent: &McpAgent, state: McpState) -> String {
    match state {
        McpState::Added => "already added".to_owned(),
        McpState::NotInstalled => "not installed; skipped".to_owned(),
        McpState::Unreadable | McpState::Missing | McpState::Outdated => format!(
            "can't read {}: {}",
            agent.config,
            agent.error.as_deref().unwrap_or("unknown error")
        ),
    }
}

/// `mcp install`'s question: what it changes in each agent's config, then `[y/N]`.
pub fn mcp_prompt(registration: &McpRegistration) -> String {
    let mut out = "Add agentdeck's MCP server to your agents' own config, so agents you start \
                   yourself in a shell pane have its tools:\n"
        .to_owned();
    for (name, agent) in mcp_agents(registration) {
        let change = mcp_change(name, agent, &registration.command);
        out.push_str(&format!("  {name}: {change}\n"));
    }
    out.push_str("Add it? [y/N] ");
    out
}

/// `mcp install` when it has nothing to change: a line per agent.
pub fn mcp_unchanged(registration: &McpRegistration) -> String {
    mcp_agents(registration)
        .iter()
        .map(|(name, agent)| format!("{name}: {}\n", mcp_state(agent, agent.state)))
        .collect()
}

/// `mcp install`'s results: a line per agent, from where it stood `before` to `after`.
pub fn mcp_results(before: &McpRegistration, after: &McpRegistration) -> String {
    let mut out = String::new();
    for ((name, was), (_, now)) in mcp_agents(before).into_iter().zip(mcp_agents(after)) {
        let line = match (&now.error, now.state) {
            (Some(why), _) => format!("couldn't add agentdeck: {why}"),
            (None, McpState::Added) if was.state != McpState::Added => {
                format!("added agentdeck ({})", now.config)
            }
            (None, state) => mcp_state(now, state),
        };
        out.push_str(&format!("{name}: {line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::{DirtyRepo, GhLogin, UnpushedBranch};
    use agentdeck_core::{Agent, TaskKind, TaskStatus};

    use super::*;

    fn workspace_named(slug: &str, name: &str, archived: bool) -> Workspace {
        Workspace {
            archived_at: archived.then(|| "2026-02-01T00:00:00Z".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            dir: format!("/work/{slug}"),
            dir_owned: false,
            id: "0190a1b2-c3d4-7e5f-8a9b-000000000001".parse().unwrap(),
            muted: false,
            name: name.into(),
            slug: slug.into(),
        }
    }

    fn a_task(title: &str, body: &str) -> Task {
        Task {
            body: body.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            done_at: None,
            id: "0190a1b2-c3d4-7e5f-8a9b-000000000002".parse().unwrap(),
            kind: TaskKind::Plain,
            pr: None,
            unseen_agent_comments: 0,
            status: TaskStatus::Doing,
            title: title.into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            workspace_id: None,
        }
    }

    mod table {
        use super::*;

        #[test]
        fn should_align_columns_under_their_headers() {
            let out = table(
                ["A", "LONGER"],
                &[["wide cell".into(), "x".into()], ["b".into(), "y".into()]],
                "none",
            );

            assert_eq!(out, "A          LONGER\nwide cell  x\nb          y\n");
        }

        #[test]
        fn should_say_so_when_empty() {
            assert_eq!(table(["A"], &[], "no things"), "no things\n");
        }
    }

    mod workspaces {
        use super::*;

        #[test]
        fn should_mark_archived_workspaces() {
            let out = workspaces(&[workspace_named("old", "Old", true)]);

            assert!(out.contains("Old (archived)"), "{out}");
        }
    }

    mod panes {
        use agentdeck_core::{AgentState, PaneKind};

        use super::*;

        const PANE_1: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000011";
        const PANE_2: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000012";
        const PANE_3: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000013";
        const TASK: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000020";

        fn listed(id: &str, kind: PaneKind, tab: &str) -> ListedPane {
            ListedPane {
                pane: Pane {
                    agent: None,
                    argv: vec!["sh".into()],
                    created_at: "2026-01-01T00:00:00Z".into(),
                    cwd: "/work/web".into(),
                    exit_code: None,
                    hosted_agent: None,
                    id: id.parse().unwrap(),
                    kind,
                    session_id: None,
                    state: AgentState::Idle,
                    tab_id: "0190a1b2-c3d4-7e5f-8a9b-000000000030".parse().unwrap(),
                    task_id: None,
                    title: String::new(),
                },
                workspace_id: "0190a1b2-c3d4-7e5f-8a9b-000000000001".parse().unwrap(),
                workspace: "web".into(),
                tab: tab.into(),
                task_title: None,
            }
        }

        #[test]
        fn should_list_id_workspace_tab_agent_state_and_task() {
            let mut agent = listed(PANE_1, PaneKind::Agent, "agents");
            agent.pane.agent = Some(Agent::Claude);
            agent.pane.state = AgentState::Blocked;
            agent.pane.task_id = Some(TASK.parse().unwrap());
            agent.task_title = Some("Fix the flaky test".into());
            let mut hosting = listed(PANE_2, PaneKind::Shell, "shell");
            hosting.pane.hosted_agent = Some(Agent::Codex);
            hosting.pane.state = AgentState::Working;
            let mut exited = listed(PANE_3, PaneKind::Shell, "shell");
            exited.pane.state = AgentState::Exited;
            exited.pane.task_id = Some(TASK.parse().unwrap());

            let out = panes(&[agent, hosting, exited]);

            assert_eq!(
                out,
                format!(
                    "ID{pad}  WORKSPACE  TAB     AGENT          STATE    TASK\n\
                     {PANE_1}  web        agents  claude         blocked  Fix the flaky test\n\
                     {PANE_2}  web        shell   hosting codex  working\n\
                     {PANE_3}  web        shell   shell          exited   {TASK}\n",
                    pad = " ".repeat(PANE_1.len() - 2)
                )
            );
        }

        #[test]
        fn should_say_so_when_no_pane_runs() {
            assert_eq!(panes(&[]), "no panes\n");
        }
    }

    mod task {
        use super::*;

        #[test]
        fn should_put_a_task_without_a_workspace_in_the_inbox() {
            let out = super::task(&a_task("Fix it", ""));

            assert!(out.contains("workspace: inbox"), "{out}");
        }

        #[test]
        fn should_end_with_the_body() {
            let out = super::task(&a_task("Fix it", "Steps:\n1. look"));

            assert!(out.ends_with("\nSteps:\n1. look\n"), "{out}");
        }

        #[test]
        fn should_say_the_sign_in_cannot_see_an_unreachable_pr() {
            let out = super::task(&Task {
                pr: Some(PullRequest {
                    checks: None,
                    draft: false,
                    head_ref: "feat".into(),
                    number: 8,
                    repo: "acme/secret".into(),
                    review: None,
                    state: agentdeck_core::PrState::Open,
                    synced_at: String::new(),
                    unreachable: true,
                    updated_at: String::new(),
                    url: "https://github.com/acme/secret/pull/8".into(),
                }),
                ..a_task("Fix it", "")
            });

            assert!(
                out.contains(
                    "  pr:        https://github.com/acme/secret/pull/8 (open, unreachable, \
                     checks none, review none)\n"
                ),
                "{out}"
            );
        }
    }

    mod comments {
        use super::*;

        fn comment(author: CommentAuthor, agent: Option<Agent>, delivered: bool) -> Comment {
            Comment {
                author,
                author_agent: agent,
                author_pane: None,
                body: "first\nsecond".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                delivered_at: delivered.then(|| "2026-01-01T01:00:00Z".into()),
                delivered_to_pane: None,
                id: "0190a1b2-c3d4-7e5f-8a9b-000000000003".parse().unwrap(),
                seen_at: None,
                task_id: "0190a1b2-c3d4-7e5f-8a9b-000000000001".parse().unwrap(),
            }
        }

        #[test]
        fn should_say_who_wrote_it_and_whether_an_agent_has_it() {
            let out = comments(&[
                comment(CommentAuthor::User, None, false),
                comment(CommentAuthor::User, None, true),
                comment(CommentAuthor::Agent, Some(Agent::Codex), false),
                comment(CommentAuthor::Agent, None, false),
            ]);

            let lines: Vec<&str> = out.lines().collect();
            assert!(lines[0].starts_with("ID"), "{out}");
            assert!(lines[1].contains("you    waiting"), "{out}");
            assert!(lines[2].contains("you    delivered"), "{out}");
            assert!(lines[3].contains("codex"), "{out}");
            assert!(!lines[3].contains("waiting"), "{out}");
            assert!(lines[4].contains("agent"), "{out}");
        }

        #[test]
        fn should_show_only_the_first_line_of_a_body() {
            let out = comments(&[comment(CommentAuthor::User, None, false)]);

            assert!(out.contains("first …"), "{out}");
            assert!(!out.contains("second"), "{out}");
        }

        #[test]
        fn should_say_when_there_are_none() {
            assert_eq!(comments(&[]), "no comments\n");
        }
    }

    mod instructions {
        use super::*;

        #[test]
        fn should_list_each_id_and_first_line() {
            let instruction = Instruction {
                body: "first\nsecond".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                id: "0190a1b2-c3d4-7e5f-8a9b-000000000003".parse().unwrap(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                workspace_id: "0190a1b2-c3d4-7e5f-8a9b-000000000001".parse().unwrap(),
            };

            let out = instructions(&[instruction]);

            assert_eq!(
                out,
                "ID                                    BODY\n\
                 0190a1b2-c3d4-7e5f-8a9b-000000000003  first …\n"
            );
        }
    }

    mod pr_status {
        use super::*;

        #[test]
        fn should_say_never_before_the_first_sync() {
            let out = pr_status(&PrSyncStatus {
                last_error: Some("gh is not logged in".into()),
                last_synced_at: None,
                login: GhLogin::LoggedOut,
                max_age_days: 90,
                progress: None,
                synced_as: None,
            });

            assert_eq!(
                out,
                "github:    logged_out\nlast sync: never\nlast error: gh is not logged in\n"
            );
        }

        #[test]
        fn should_name_the_login_prs_last_synced_as() {
            let out = pr_status(&PrSyncStatus {
                last_error: None,
                last_synced_at: Some("2026-11-14T10:00:00.000Z".into()),
                login: GhLogin::LoggedOut,
                max_age_days: 90,
                progress: None,
                synced_as: Some("octocat".into()),
            });

            assert_eq!(
                out,
                "github:    logged_out\nlast sync: 2026-11-14T10:00:00.000Z\nsynced as: octocat\n"
            );
        }
    }

    mod pr_status_syncing {
        use agentdeck_client::types::{PrSyncProgress, PrSyncStage};

        use super::*;

        #[test]
        fn should_say_how_far_a_running_sync_is() {
            let out = pr_status(&PrSyncStatus {
                last_error: None,
                last_synced_at: Some("2026-11-14T10:00:00.000Z".into()),
                login: GhLogin::LoggedIn,
                max_age_days: 90,
                progress: Some(PrSyncProgress {
                    prs_done: 100,
                    prs: 271,
                    stage: PrSyncStage::Listing,
                }),
                synced_as: None,
            });

            assert_eq!(
                out,
                "github:    logged_in\nlast sync: 2026-11-14T10:00:00.000Z\nsyncing:   100 of 271 PRs\n"
            );
        }
    }

    mod pr_status_details {
        use agentdeck_client::types::{PrSyncProgress, PrSyncStage};

        use super::*;

        #[test]
        fn should_say_it_is_fetching_details() {
            let out = pr_status(&PrSyncStatus {
                last_error: None,
                last_synced_at: None,
                login: GhLogin::LoggedIn,
                max_age_days: 90,
                progress: Some(PrSyncProgress {
                    prs_done: 271,
                    prs: 271,
                    stage: PrSyncStage::Details,
                }),
                synced_as: None,
            });

            assert!(out.contains("syncing:   PR details\n"), "{out}");
        }
    }

    mod close_prompt {
        use super::*;

        #[test]
        fn should_ask_with_a_line_per_repo_and_its_count() {
            assert_eq!(
                close_prompt(3, &["acme/widgets", "acme/api", "acme/widgets"]),
                "Close 3 PRs on GitHub? Their repos' owners will see them closed. You can \
                 reopen a PR on GitHub.\n  acme/api      1 PR\n  acme/widgets  2 PRs\n[y/N] "
            );
        }

        #[test]
        fn should_say_1_pr() {
            assert!(close_prompt(1, &["acme/api"]).starts_with("Close 1 PR on GitHub?"));
        }
    }

    mod close_results {
        use super::*;

        fn pr(number: i64) -> PullRequest {
            PullRequest {
                checks: None,
                draft: false,
                head_ref: "feat".into(),
                number,
                repo: "acme/api".into(),
                review: None,
                state: agentdeck_core::PrState::Open,
                synced_at: String::new(),
                unreachable: false,
                updated_at: String::new(),
                url: String::new(),
            }
        }

        fn id(n: u8) -> TaskId {
            format!("019a1dd1-5b85-75ae-af0b-6d1a8b2110{n:02}")
                .parse()
                .expect("a task id")
        }

        #[test]
        fn should_say_what_closed_and_what_failed_then_how_many_closed() {
            let (seven, eight) = (pr(7), pr(8));
            let prs = HashMap::from([(id(1), &seven), (id(2), &eight)]);
            let results = [
                PrCloseResult {
                    task_id: id(1),
                    ok: true,
                    error: None,
                },
                PrCloseResult {
                    task_id: id(2),
                    ok: false,
                    error: Some("Not allowed".into()),
                },
                PrCloseResult {
                    task_id: id(3),
                    ok: false,
                    error: Some("Not an open PR".into()),
                },
            ];

            assert_eq!(
                close_results(&results, &prs),
                format!(
                    "closed acme/api #7\nfailed acme/api #8: Not allowed\nfailed {}: Not an \
                     open PR\nClosed 1 of 3\n",
                    id(3)
                )
            );
        }

        #[test]
        fn should_not_count_when_every_pr_closed() {
            let seven = pr(7);
            let prs = HashMap::from([(id(1), &seven)]);
            let results = [PrCloseResult {
                task_id: id(1),
                ok: true,
                error: None,
            }];

            assert_eq!(close_results(&results, &prs), "closed acme/api #7\n");
        }
    }

    mod dirty {
        use super::*;

        #[test]
        fn should_list_each_repos_files_and_branches() {
            let out = dirty(&DirtyReport {
                repos: vec![DirtyRepo {
                    path: "/work/web".into(),
                    uncommitted: vec![" M src/main.rs".into()],
                    unpushed_branches: vec![
                        UnpushedBranch {
                            name: "feat/x".into(),
                            commits: 2,
                        },
                        UnpushedBranch {
                            name: "fix/y".into(),
                            commits: 1,
                        },
                    ],
                }],
            });

            assert_eq!(
                out,
                "/work/web\n  uncommitted:  M src/main.rs\n  unpushed:    feat/x (2 commits)\n  \
                 unpushed:    fix/y (1 commit)\n"
            );
        }
    }

    mod mcp_install {
        use super::*;

        fn agent(state: McpState, config: &str) -> McpAgent {
            McpAgent {
                state,
                config: config.to_owned(),
                error: None,
            }
        }

        fn registration(claude: McpState, codex: McpState) -> McpRegistration {
            McpRegistration {
                command: "/opt/bin/agentdeck".to_owned(),
                claude: agent(claude, "~/.claude.json"),
                codex: agent(codex, "~/.codex/config.toml"),
            }
        }

        #[test]
        fn should_ask_naming_the_command_and_the_codex_table() {
            let prompt = mcp_prompt(&registration(McpState::Missing, McpState::Missing));

            assert_eq!(
                prompt,
                "Add agentdeck's MCP server to your agents' own config, so agents you start \
                 yourself in a shell pane have its tools:\n  Claude Code: run claude mcp add \
                 --scope user agentdeck -- /opt/bin/agentdeck mcp\n  Codex: add \
                 [mcp_servers.agentdeck] to ~/.codex/config.toml\nAdd it? [y/N] "
            );
        }

        #[test]
        fn should_say_what_it_replaces_and_what_it_skips() {
            let prompt = mcp_prompt(&registration(McpState::Outdated, McpState::NotInstalled));

            assert!(
                prompt.contains("  Claude Code: run claude mcp remove --scope user agentdeck, then claude mcp add"),
                "{prompt}"
            );
            assert!(
                prompt.contains("  Codex: not installed; skipped\n"),
                "{prompt}"
            );
        }

        #[test]
        fn should_change_nothing_once_both_are_added_or_not_installed() {
            assert!(!mcp_changes_anything(&registration(
                McpState::Added,
                McpState::NotInstalled
            )));
            assert!(mcp_changes_anything(&registration(
                McpState::Added,
                McpState::Outdated
            )));
        }

        #[test]
        fn should_say_which_were_added_and_why_one_failed() {
            let before = registration(McpState::Missing, McpState::Missing);
            let mut after = registration(McpState::Added, McpState::Missing);
            after.codex.error = Some("~/.codex/config.toml: expected `]`".to_owned());

            assert_eq!(
                mcp_results(&before, &after),
                "Claude Code: added agentdeck (~/.claude.json)\nCodex: couldn't add agentdeck: \
                 ~/.codex/config.toml: expected `]`\n"
            );
            assert!(mcp_failed(&after));
        }
    }
}
