//! The database at start (spec §2, §3): a fresh data dir gets the schema from the one first
//! migration, and a database from before the domain reset, one with a `tasks` table, is moved
//! aside with its WAL and SHM files so the daemon starts empty. A database from the first
//! migration loses its Workspaces' folder columns and keeps the rest.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use rusqlite::config::DbConfig;

use crate::support::TestDaemon;

/// `$XDG_DATA_HOME/agentdeck` under `root`.
fn data_dir(root: &Path) -> PathBuf {
    root.join("data/agentdeck")
}

/// `path` with `suffix` after it, as SQLite names a database's sidecar files.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The names of the tables in the database at `path`, sorted.
fn tables(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).expect("opening the database");
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .expect("reading the schema");
    stmt.query_map([], |row| row.get(0))
        .expect("reading the schema")
        .collect::<rusqlite::Result<_>>()
        .expect("reading the schema")
}

/// Writes a database from before the domain reset under `root`, in WAL mode, with its last
/// writes still in its WAL: a workspace with a task, and a pane linked to it. Its WAL (and
/// SHM, when SQLite keeps one) is left beside it, as after a crash.
fn pre_reset_db(root: &Path) -> PathBuf {
    let dir = data_dir(root);
    std::fs::create_dir_all(&dir).expect("creating the data dir");
    let path = dir.join("agentdeck.db");
    let conn = Connection::open(&path).expect("opening the database");
    conn.pragma_update(None, "journal_mode", "WAL")
        .expect("WAL mode");
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .expect("keeping the WAL at close");
    conn.pragma_update(None, "wal_autocheckpoint", 0)
        .expect("no automatic checkpoint");
    conn.execute_batch(
        "CREATE TABLE workspaces (id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE,
           name TEXT NOT NULL, dir TEXT NOT NULL, dir_owned INTEGER NOT NULL,
           muted INTEGER NOT NULL DEFAULT 0, archived_at TEXT, created_at TEXT NOT NULL);
         CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL,
           workspace_id TEXT REFERENCES workspaces ON DELETE SET NULL);
         CREATE TABLE panes (id TEXT PRIMARY KEY, task_id TEXT REFERENCES tasks);
         INSERT INTO workspaces (id, slug, name, dir, dir_owned, created_at)
           VALUES ('w1', 'web', 'web', '/srv/web', 0, '2026-11-01T00:00:00.000000Z');
         INSERT INTO tasks (id, title, workspace_id) VALUES ('t1', 'Fix the build', 'w1');
         INSERT INTO panes (id, task_id) VALUES ('p1', 't1');
         PRAGMA user_version = 10;",
    )
    .expect("the pre-reset schema");
    drop(conn);
    assert!(
        with_suffix(&path, "-wal").is_file(),
        "the WAL stays beside the database"
    );
    path
}

mod fresh_start {
    use super::*;

    #[tokio::test]
    async fn should_create_the_schema_with_no_tasks_comments_or_instructions() {
        let daemon = TestDaemon::start().await;

        assert_eq!(
            tables(&daemon.data_dir().join("agentdeck.db")),
            [
                "links",
                "panes",
                "pr_sync",
                "pull_requests",
                "repositories",
                "tabs",
                "workspaces"
            ]
        );
    }

    #[tokio::test]
    async fn should_key_pull_requests_by_repository_and_number_with_no_unique_name() {
        let daemon = TestDaemon::start().await;
        let conn = Connection::open(daemon.data_dir().join("agentdeck.db")).expect("opening");

        let mut key: Vec<(i64, String)> = conn
            .prepare("SELECT pk, name FROM pragma_table_info('pull_requests') WHERE pk > 0")
            .expect("reading the columns")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("reading the columns")
            .collect::<rusqlite::Result<_>>()
            .expect("reading the columns");
        key.sort();
        let unique: Vec<String> = conn
            .prepare(
                "SELECT name FROM pragma_index_list('pull_requests')
                 WHERE \"unique\" = 1 AND origin <> 'pk'",
            )
            .expect("reading the indexes")
            .query_map([], |row| row.get(0))
            .expect("reading the indexes")
            .collect::<rusqlite::Result<_>>()
            .expect("reading the indexes");

        assert_eq!(
            key,
            [(1, "repository_id".to_owned()), (2, "number".to_owned())]
        );
        assert_eq!(unique, Vec::<String>::new());
    }

    #[tokio::test]
    async fn should_keep_a_current_database_across_a_restart() {
        let daemon = TestDaemon::start().await;
        daemon
            .client
            .workspace_create()
            .body_map(|new| new.name("web"))
            .send()
            .await
            .expect("create a workspace");
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        let workspaces = (daemon.client.workspace_list().send().await)
            .expect("list workspaces")
            .into_inner();
        assert_eq!(workspaces.len(), 1);
        let aside = with_suffix(&daemon.data_dir().join("agentdeck.db"), ".pre-domain-reset");
        assert!(!aside.exists(), "nothing moved aside");
    }
}

/// The names of the columns of `table` in the database at `path`, in order.
fn columns(path: &Path, table: &str) -> Vec<String> {
    let conn = Connection::open(path).expect("opening the database");
    conn.prepare(&format!(
        "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
    ))
    .expect("reading the columns")
    .query_map([], |row| row.get(0))
    .expect("reading the columns")
    .collect::<rusqlite::Result<_>>()
    .expect("reading the columns")
}

/// Writes a database under `root` as the first migration left it, before Workspaces lost
/// their folders: workspace `web`, with a scratch folder, and a Link to a Repository.
fn first_migration_db(root: &Path) -> PathBuf {
    let dir = data_dir(root);
    std::fs::create_dir_all(&dir).expect("creating the data dir");
    let path = dir.join("agentdeck.db");
    let conn = Connection::open(&path).expect("opening the database");
    conn.execute_batch(include_str!("../../src/db/migrations/0001_init.sql"))
        .expect("the first migration");
    conn.execute_batch(
        "INSERT INTO workspaces (id, slug, name, dir, dir_owned, muted, created_at)
           VALUES ('0190a1b2-c3d4-7e5f-8a9b-000000000001', 'web', 'Web', '/srv/web', 1, 1,
                   '2026-11-01T00:00:00.000000Z');
         INSERT INTO repositories (id, full_name, url, private, archived)
           VALUES (1296269, 'acme/widgets', 'https://github.com/acme/widgets', 0, 0);
         INSERT INTO links (workspace_id, repository_id)
           VALUES ('0190a1b2-c3d4-7e5f-8a9b-000000000001', 1296269);
         PRAGMA user_version = 1;",
    )
    .expect("a workspace with a folder");
    path
}

mod workspace_folders {
    use super::*;

    #[tokio::test]
    async fn should_drop_the_folder_columns_and_keep_each_workspace_and_its_links() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        let path = first_migration_db(dirs.path());

        let daemon = TestDaemon::start_in(dirs).await;

        assert_eq!(
            columns(&path, "workspaces"),
            ["id", "slug", "name", "muted", "archived_at", "created_at"]
        );
        let workspaces = (daemon.client.workspace_list().send().await)
            .expect("list workspaces")
            .into_inner();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(
            (workspaces[0].slug.as_str(), workspaces[0].muted),
            ("web", true)
        );
        let linked = (daemon.client.repository_list().send().await)
            .expect("list Repositories")
            .into_inner();
        assert_eq!(linked[0].workspaces, [workspaces[0].id]);
    }
}

mod pre_reset_database {
    use super::*;

    #[tokio::test]
    async fn should_move_it_aside_with_its_wal_and_shm_files_and_start_empty() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        let path = pre_reset_db(dirs.path());
        let had_shm = with_suffix(&path, "-shm").is_file();

        let daemon = TestDaemon::start_in(dirs).await;

        let workspaces = (daemon.client.workspace_list().send().await)
            .expect("list workspaces")
            .into_inner();
        assert!(workspaces.is_empty(), "an empty database: {workspaces:?}");
        assert!(!tables(&path).contains(&"tasks".to_owned()));
        let aside = with_suffix(&path, ".pre-domain-reset");
        assert!(
            with_suffix(&aside, "-wal").is_file(),
            "the WAL moved with it"
        );
        assert_eq!(with_suffix(&aside, "-shm").is_file(), had_shm);
        // Its last writes were only in the WAL: moved with it, they are still there.
        let old = Connection::open(&aside).expect("opening the old database");
        let title: String = old
            .query_row("SELECT title FROM tasks WHERE id = 't1'", [], |row| {
                row.get(0)
            })
            .expect("the old task");
        assert_eq!(title, "Fix the build");
    }

    #[tokio::test]
    async fn should_log_where_it_went() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        pre_reset_db(dirs.path());

        let daemon = TestDaemon::start_in(dirs).await;

        let log = std::fs::read_to_string(daemon.state_dir().join("agentdeckd.log"))
            .expect("reading the log");
        assert!(
            log.lines().any(|line| line.contains("WARN")
                && line.contains("before the domain reset")
                && line.contains("agentdeck.db.pre-domain-reset")),
            "{log}"
        );
    }

    #[tokio::test]
    async fn should_leave_old_scratch_folders_untouched() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        pre_reset_db(dirs.path());
        let scratch = data_dir(dirs.path()).join("workspaces/web");
        std::fs::create_dir_all(&scratch).expect("a scratch folder");
        std::fs::write(scratch.join("notes.md"), "work in progress").expect("a file in it");

        let _daemon = TestDaemon::start_in(dirs).await;

        assert_eq!(
            std::fs::read_to_string(scratch.join("notes.md")).expect("the file stays"),
            "work in progress"
        );
    }

    #[tokio::test]
    async fn should_never_overwrite_a_database_moved_aside_before() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        let path = pre_reset_db(dirs.path());
        let first = with_suffix(&path, ".pre-domain-reset");
        std::fs::write(&first, "an earlier one").expect("an earlier moved-aside database");

        let _daemon = TestDaemon::start_in(dirs).await;

        assert_eq!(
            std::fs::read_to_string(&first).expect("the earlier one stays"),
            "an earlier one"
        );
        assert!(with_suffix(&first, ".2").is_file(), "moved beside it");
    }
}
