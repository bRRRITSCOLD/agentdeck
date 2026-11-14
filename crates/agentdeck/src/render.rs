//! Human-readable output. `--json` prints the API's own objects instead.

use agentdeck_client::types::{
    Comment, DirtyReport, Instruction, Pane, PrSyncStage, PrSyncStatus, Task, Workspace,
};
use agentdeck_core::CommentAuthor;

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
