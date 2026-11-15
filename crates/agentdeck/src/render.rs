//! Human-readable output. `--json` prints the API's own objects instead.

use agentdeck_client::settings;
use std::collections::{BTreeMap, HashMap};

use agentdeck_client::types::{
    Comment, DirtyReport, Instruction, Pane, PrCloseResult, PrSyncStage, PrSyncStatus, PullRequest,
    Settings, Task, Workspace,
};
use agentdeck_core::{CommentAuthor, TaskId};
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
        out.push_str(&format!(
            "  pr:        {} ({}, checks {}, review {})\n",
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
        "gh:        {}\nlast sync: {}\n",
        status.login,
        status.last_synced_at.as_deref().unwrap_or("never"),
    );
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
            });

            assert_eq!(
                out,
                "gh:        logged_out\nlast sync: never\nlast error: gh is not logged in\n"
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
            });

            assert_eq!(
                out,
                "gh:        logged_in\nlast sync: 2026-11-14T10:00:00.000Z\nsyncing:   100 of 271 PRs\n"
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
}
