//! Panes running `sh` through a real daemon: spawning them, and waiting on what they print
//! and on their state. Every wait polls a condition against a deadline; none sleeps a guess.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agentdeck_client::Client;
use agentdeck_client::types::{NewPane, NewTab, NewWorkspace, Pane, Tab, Workspace};
use agentdeck_core::{Agent, AgentState, PaneId, PaneKind};
use futures_util::TryStreamExt;

use super::TestDaemon;

/// How long a pane gets to print or change state.
pub const DEADLINE: Duration = Duration::from_secs(30);

/// A workspace and a tab in it, for panes to go in, and a fresh folder for them to start in:
/// a workspace has none of its own.
pub struct Place {
    pub workspace: Workspace,
    pub tab: Tab,
    /// The folder its panes start in.
    pub dir: PathBuf,
}

impl Place {
    pub async fn new(daemon: &TestDaemon) -> Place {
        let dir = daemon.new_dir("work");
        let workspace = daemon
            .client
            .workspace_create()
            .body(NewWorkspace {
                name: "ws".into(),
                link: Vec::new(),
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
        Place {
            workspace,
            tab,
            dir,
        }
    }

    /// The folder its panes start in, as the API takes it.
    pub fn folder(&self) -> String {
        self.dir.to_str().expect("a UTF-8 path").to_owned()
    }

    /// A shell pane running `argv` in [`Place::dir`].
    pub fn new_pane(&self, argv: &[&str]) -> NewPane {
        NewPane {
            tab_id: self.tab.id,
            kind: PaneKind::Shell,
            agent: None,
            argv: Some(argv.iter().map(|arg| (*arg).to_owned()).collect()),
            cwd: self.folder(),
        }
    }

    /// An agent pane of `agent` in [`Place::dir`].
    pub fn new_agent(&self, agent: Agent) -> NewPane {
        NewPane {
            tab_id: self.tab.id,
            kind: PaneKind::Agent,
            agent: Some(agent),
            argv: None,
            cwd: self.folder(),
        }
    }

    /// Launches an agent pane of `agent`.
    pub async fn launch(&self, client: &Client, agent: Agent) -> Pane {
        client
            .pane_create()
            .body(self.new_agent(agent))
            .send()
            .await
            .expect("launch an agent")
            .into_inner()
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

/// Fails the test with `what` unless `check` holds on every poll for `period`: for something
/// that must not happen, once whatever would make it happen has had its chance.
pub async fn holds_for<F, Fut>(what: &str, period: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let until = Instant::now() + period;
    while Instant::now() < until {
        assert!(check().await, "{what}");
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

/// How many reader threads `daemon` runs, one per pane whose PTY is still open: its threads
/// named `pane-read-<id>`, which the kernel cuts to 15 bytes.
pub fn reader_threads(daemon: &TestDaemon) -> usize {
    let tasks = format!("/proc/{}/task", daemon.info.pid);
    std::fs::read_dir(tasks)
        .expect("the daemon's threads")
        .flatten()
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.starts_with("pane-read"))
        })
        .count()
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
