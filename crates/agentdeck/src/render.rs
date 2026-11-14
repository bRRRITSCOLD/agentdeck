//! Human-readable output. `--json` prints the API's own objects instead.

use agentdeck_client::types::{DirtyReport, Note, Pane, PrSyncStatus, Task, Workspace};

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

/// Where a note is: `ws <id>` or `task <id>`.
fn note_target(note: &Note) -> String {
    match (note.workspace_id, note.task_id) {
        (Some(ws), _) => format!("ws {ws}"),
        (None, Some(task)) => format!("task {task}"),
        (None, None) => String::new(),
    }
}

/// `agentdeck note ls`.
pub fn notes(notes: &[Note]) -> String {
    let rows: Vec<[String; 4]> = notes
        .iter()
        .map(|note| {
            let read = if note.read_at.is_some() {
                "read"
            } else {
                "unread"
            };
            [
                note.id.to_string(),
                read.to_owned(),
                note_target(note),
                first_line(&note.body),
            ]
        })
        .collect();
    table(["ID", "READ", "ON", "BODY"], &rows, "no notes")
}

/// A note after `note add`.
pub fn note_created(note: &Note) -> String {
    format!("{}  {}\n", note.id, note_target(note))
}

/// `agentdeck note read`: the message itself.
pub fn note_body(note: &Note) -> String {
    let mut body = note.body.clone();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body
}

/// `agentdeck pr sync` and `pr status`.
pub fn pr_status(status: &PrSyncStatus) -> String {
    let mut out = format!(
        "gh:        {}\nlast sync: {}\n",
        status.login,
        status.last_synced_at.as_deref().unwrap_or("never"),
    );
    if let Some(progress) = &status.progress {
        out.push_str(&format!(
            "syncing:   page {} of {}\n",
            progress.pages_done, progress.pages
        ));
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
            out.push_str(&format!("  unpushed:    {branch}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::{DirtyRepo, GhLogin};
    use agentdeck_core::{TaskKind, TaskStatus};

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

    mod notes {
        use super::*;

        #[test]
        fn should_show_only_the_first_line_of_a_body() {
            let note = Note {
                body: "first\nsecond".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                id: "0190a1b2-c3d4-7e5f-8a9b-000000000003".parse().unwrap(),
                read_at: None,
                read_by_pane: None,
                task_id: None,
                workspace_id: Some("0190a1b2-c3d4-7e5f-8a9b-000000000001".parse().unwrap()),
            };

            let out = notes(&[note]);

            assert!(out.contains("unread"), "{out}");
            assert!(out.contains("first …"), "{out}");
            assert!(!out.contains("second"), "{out}");
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
                progress: None,
            });

            assert_eq!(
                out,
                "gh:        logged_out\nlast sync: never\nlast error: gh is not logged in\n"
            );
        }
    }

    mod pr_status_syncing {
        use agentdeck_client::types::PrSyncProgress;

        use super::*;

        #[test]
        fn should_say_how_far_a_running_sync_is() {
            let out = pr_status(&PrSyncStatus {
                last_error: None,
                last_synced_at: Some("2026-11-14T10:00:00.000Z".into()),
                login: GhLogin::LoggedIn,
                progress: Some(PrSyncProgress {
                    pages_done: 2,
                    pages: 6,
                }),
            });

            assert_eq!(
                out,
                "gh:        logged_in\nlast sync: 2026-11-14T10:00:00.000Z\nsyncing:   page 2 of 6\n"
            );
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
                    unpushed_branches: vec!["feat/x".into()],
                }],
            });

            assert_eq!(
                out,
                "/work/web\n  uncommitted:  M src/main.rs\n  unpushed:    feat/x\n"
            );
        }
    }
}
