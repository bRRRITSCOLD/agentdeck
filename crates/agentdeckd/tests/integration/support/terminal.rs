//! Panes running `sh` through a real daemon: spawning them, and waiting on what they print
//! and on their state. Every wait polls a condition against a deadline; none sleeps a guess.

use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use agentdeck_client::Client;
use agentdeck_client::types::{NewPane, NewTab, NewWorkspace, Pane, Tab, Workspace};
use agentdeck_core::{AgentState, PaneId, PaneKind};
use futures_util::TryStreamExt;

use super::TestDaemon;

/// How long a pane gets to print or change state.
pub const DEADLINE: Duration = Duration::from_secs(30);

/// A workspace (attached to a fresh dir) and a tab in it, for panes to go in.
pub struct Place {
    pub workspace: Workspace,
    pub tab: Tab,
}

impl Place {
    pub async fn new(daemon: &TestDaemon) -> Place {
        let dir = daemon.new_dir("work");
        let workspace = daemon
            .client
            .workspace_create()
            .body(NewWorkspace {
                name: "ws".into(),
                dir: Some(dir.to_str().expect("a UTF-8 path").into()),
            })
            .send()
            .await
            .expect("create a workspace")
            .into_inner();
        let tab = daemon
            .client
            .tab_create()
            .id(workspace.id)
            .body(NewTab {
                title: "tab".into(),
                layout: None,
            })
            .send()
            .await
            .expect("create a tab")
            .into_inner();
        Place { workspace, tab }
    }

    /// A shell pane running `argv` in the workspace's dir.
    pub fn new_pane(&self, argv: &[&str]) -> NewPane {
        NewPane {
            tab_id: self.tab.id,
            kind: PaneKind::Shell,
            agent: None,
            argv: Some(argv.iter().map(|arg| (*arg).to_owned()).collect()),
            cwd: self.workspace.dir.clone(),
            task_id: None,
        }
    }

    /// Spawns a shell pane running `argv`.
    pub async fn spawn(&self, client: &Client, argv: &[&str]) -> Pane {
        client
            .pane_create()
            .body(self.new_pane(argv))
            .send()
            .await
            .expect("spawn a pane")
            .into_inner()
    }
}

/// `sh -c <script>`.
pub fn sh(script: &str) -> [&str; 3] {
    ["sh", "-c", script]
}

/// Polls `check` until it gives a value, failing the test with `what` after [`DEADLINE`].
pub async fn eventually<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The pane's raw output tail.
pub async fn tail(client: &Client, pane: PaneId, bytes: Option<u64>) -> Vec<u8> {
    let mut request = client.pane_tail().id(pane);
    if let Some(bytes) = bytes {
        request = request.bytes(bytes);
    }
    let response = request.send().await.expect("read the tail");
    let chunks: Vec<_> = response
        .into_inner()
        .into_inner()
        .try_collect()
        .await
        .expect("the tail's body");
    chunks.concat()
}

/// Waits until the pane's output holds `needle`; returns the whole tail as text.
pub async fn printed(client: &Client, pane: PaneId, needle: &str) -> String {
    eventually(&format!("the pane to print {needle:?}"), || async {
        let text = String::from_utf8_lossy(&tail(client, pane, None).await).into_owned();
        text.contains(needle).then_some(text)
    })
    .await
}

/// Waits until the pane reads as `state` over REST.
pub async fn in_state(client: &Client, pane: PaneId, state: AgentState) -> Pane {
    eventually(&format!("the pane to be {state}"), || async {
        let pane = client
            .pane_get()
            .id(pane)
            .send()
            .await
            .expect("read the pane")
            .into_inner();
        (pane.state == state).then_some(pane)
    })
    .await
}

/// Whether process `pid` is alive (a zombie counts as dead).
pub fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit(')')
            .next()
            .is_some_and(|rest| !rest.trim_start().starts_with('Z'))
    })
}

/// Shell panes whose title is exactly `idle` are idle, after 100 ms; every daemon started on
/// `root` afterwards reads it.
pub fn shell_rules(root: &Path) {
    let dir = root.join("config/agentdeck/rules");
    std::fs::create_dir_all(&dir).expect("creating the rules dir");
    std::fs::write(
        dir.join("shell.toml"),
        "agent = \"shell\"\n\n[[rule]]\nstate = \"idle\"\ntitle_regex = '^idle$'\n\n\
         [settle]\nmin_ms = 100\n",
    )
    .expect("writing shell.toml");
}

/// A daemon whose shell panes follow [`shell_rules`].
pub async fn daemon_with_shell_rules() -> TestDaemon {
    let dirs = tempfile::tempdir().expect("a temp dir");
    shell_rules(dirs.path());
    TestDaemon::start_in(dirs).await
}
