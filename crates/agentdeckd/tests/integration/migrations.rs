//! Migrating a database an earlier daemon wrote: `agentdeckd` migrates it at start.

use std::path::Path;

use agentdeck_core::{PaneId, TaskId, WorkspaceId};
use rusqlite::{Connection, params};

use crate::support::TestDaemon;

/// The v1 schema, as the first migration made it.
const SCHEMA_0001: &str = include_str!("../../src/db/migrations/0001_init.sql");

/// Writes `$XDG_DATA_HOME/agentdeck/agentdeck.db` under `root` at schema 0001, holding a
/// workspace with a task, a note on each, and a task note a pane has read.
fn notes_era_db(root: &Path) {
    let dir = root.join("data/agentdeck");
    std::fs::create_dir_all(&dir).expect("creating the data dir");
    let conn = Connection::open(dir.join("agentdeck.db")).expect("opening the database");
    conn.execute_batch(SCHEMA_0001).expect("schema 0001");
    conn.pragma_update(None, "user_version", 1)
        .expect("marking it migrated to 0001");
    let workspace: WorkspaceId = uuid::Uuid::now_v7().into();
    let task: TaskId = uuid::Uuid::now_v7().into();
    let reader: PaneId = uuid::Uuid::now_v7().into();
    let workspace_dir = root.join("dirs/web");
    std::fs::create_dir_all(&workspace_dir).expect("creating the workspace dir");
    conn.execute(
        "INSERT INTO workspaces (id, slug, name, dir, dir_owned, created_at)
         VALUES (?1, 'web', 'web', ?2, 0, '2026-11-01T00:00:00.000Z')",
        params![workspace.to_string(), workspace_dir.to_str().unwrap()],
    )
    .expect("a workspace");
    conn.execute(
        "INSERT INTO tasks (id, title, workspace_id, created_at, updated_at)
         VALUES (?1, 'Fix the build', ?2, '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
        params![task.to_string(), workspace.to_string()],
    )
    .expect("a task");
    let note =
        "INSERT INTO notes (id, body, workspace_id, task_id, created_at, read_at, read_by_pane)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    conn.execute(
        note,
        params![
            uuid::Uuid::now_v7().to_string(),
            "Push only after 20 green runs",
            workspace.to_string(),
            None::<String>,
            "2026-11-02T00:00:00.000Z",
            "2026-11-03T00:00:00.000Z",
            reader.to_string(),
        ],
    )
    .expect("a workspace note");
    conn.execute(
        note,
        params![
            uuid::Uuid::now_v7().to_string(),
            "It fails on CI only",
            None::<String>,
            task.to_string(),
            "2026-11-02T00:00:00.000Z",
            "2026-11-03T00:00:00.000Z",
            reader.to_string(),
        ],
    )
    .expect("a read task note");
    conn.execute(
        note,
        params![
            uuid::Uuid::now_v7().to_string(),
            "Also check Firefox",
            None::<String>,
            task.to_string(),
            "2026-11-04T00:00:00.000Z",
            None::<String>,
            None::<String>,
        ],
    )
    .expect("an unread task note");
}

mod migration_0002 {
    use super::*;

    #[tokio::test]
    async fn should_drop_the_notes_table() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        notes_era_db(dirs.path());

        let daemon = TestDaemon::start_in(dirs).await;

        let conn =
            Connection::open(daemon.data_dir().join("agentdeck.db")).expect("opening the database");
        let notes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'notes'",
                [],
                |row| row.get(0),
            )
            .expect("reading the schema");
        assert_eq!(notes, 0);
    }
}

mod migration_0010 {
    use super::*;

    /// Every migration up to 0009, in order.
    const SCHEMA_0009: [&str; 9] = [
        SCHEMA_0001,
        include_str!("../../src/db/migrations/0002_comments_and_instructions.sql"),
        include_str!("../../src/db/migrations/0003_pr_aged_out.sql"),
        include_str!("../../src/db/migrations/0004_pr_workspace_picked.sql"),
        include_str!("../../src/db/migrations/0005_pane_hosted_agent.sql"),
        include_str!("../../src/db/migrations/0006_pr_unreachable.sql"),
        include_str!("../../src/db/migrations/0007_repositories.sql"),
        include_str!("../../src/db/migrations/0008_pane_folder.sql"),
        include_str!("../../src/db/migrations/0009_links.sql"),
    ];

    /// Writes a database at schema 0009 under `root` holding `acme/widgets` with PR 1 open,
    /// PR 2 open but aged out, and PR 3 merged, each with its task.
    fn aged_out_era_db(root: &Path) {
        let dir = root.join("data/agentdeck");
        std::fs::create_dir_all(&dir).expect("creating the data dir");
        let conn = Connection::open(dir.join("agentdeck.db")).expect("opening the database");
        for schema in SCHEMA_0009 {
            conn.execute_batch(schema).expect("an earlier schema");
        }
        conn.pragma_update(None, "user_version", 9)
            .expect("marking it migrated to 0009");
        conn.execute(
            "INSERT INTO repositories (id, full_name, url, private, archived)
             VALUES (42, 'acme/widgets', 'https://github.com/acme/widgets', 0, 0)",
            [],
        )
        .expect("a repository");
        let at = "2026-11-01T00:00:00.000Z";
        for (number, state, aged_out_at) in [
            (1, "open", None),
            (2, "open", Some("2026-11-02T00:00:00.000Z")),
            (3, "merged", None),
        ] {
            let task = uuid::Uuid::now_v7().to_string();
            conn.execute(
                "INSERT INTO tasks (id, title, kind, status, created_at, updated_at)
                 VALUES (?1, ?2, 'pr', 'review', ?3, ?3)",
                params![task, format!("PR {number}"), at],
            )
            .expect("a PR task");
            conn.execute(
                "INSERT INTO pull_requests (task_id, repo, number, url, state, draft, head_ref,
                   updated_at, synced_at, aged_out_at, repository_id, base_ref, author)
                 VALUES (?1, 'acme/widgets', ?2, ?3, ?4, 0, 'feat/x', ?5, ?5, ?6, 42, 'main',
                   'octocat')",
                params![
                    task,
                    number,
                    format!("https://github.com/acme/widgets/pull/{number}"),
                    state,
                    at,
                    aged_out_at,
                ],
            )
            .expect("a pull request");
        }
    }

    #[tokio::test]
    async fn should_finish_the_aged_out_and_merged_prs_and_keep_them() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        aged_out_era_db(dirs.path());

        let daemon = TestDaemon::start_in(dirs).await;

        let prs = (daemon.client.pr_list().finished(true).send().await)
            .expect("list PRs")
            .into_inner();
        let finished: Vec<(u32, bool)> = prs.iter().map(|pr| (pr.number, pr.finished)).collect();
        assert_eq!(finished, [(1, false), (2, true), (3, true)]);
    }
}
