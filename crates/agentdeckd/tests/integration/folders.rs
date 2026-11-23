//! A Pane's current folder (spec §6): the folder its shell last reported with OSC 7, else its
//! process's working directory, else the folder it launched in; kept after it exits, sent as
//! `changed` when it moves, and where it resumes after a restart. Panes run `sh`, which sends
//! no OSC 7 of its own.

use std::path::{Path, PathBuf};

use agentdeck_client::Client;
use agentdeck_client::types::{InputWhen, Pane, PaneInput};
use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{AgentState, PaneId};

use crate::support::TestDaemon;
use crate::support::terminal::{Place, eventually, in_state, printed, sh};
use crate::support::ws::{Frame, Ws};

/// The pane as `GET /v1/panes/{id}` answers now.
async fn read(client: &Client, id: PaneId) -> Pane {
    client
        .pane_get()
        .id(id)
        .send()
        .await
        .expect("read the pane")
        .into_inner()
}

/// Waits until the pane reads as being in `folder`.
async fn in_folder(client: &Client, id: PaneId, folder: &Path) -> Pane {
    eventually(
        &format!("the pane to be in {}", folder.display()),
        || async {
            let pane = read(client, id).await;
            (Path::new(&pane.folder) == folder).then_some(pane)
        },
    )
    .await
}

/// Types `text` into the pane, as is, at once.
async fn type_now(client: &Client, id: PaneId, text: &str) {
    client
        .pane_input()
        .id(id)
        .body(PaneInput {
            text: text.into(),
            when: InputWhen::Now,
            submit: false,
        })
        .send()
        .await
        .expect("type into the pane");
}

/// A dir `name` inside the workspace's dir.
fn sub_dir(place: &Place, name: &str) -> PathBuf {
    let dir = Path::new(&place.workspace.dir).join(name);
    std::fs::create_dir(&dir).expect("creating a dir in the workspace");
    dir
}

/// A script printing an OSC 7 report of `dir`, as a shell's prompt does, then `rest`.
fn report(dir: &Path, rest: &str) -> String {
    format!(
        "printf '\\033]7;file://localhost%s\\033\\\\' '{}'; {rest}",
        dir.display()
    )
}

mod current_folder {
    use super::*;

    mod launch_folder {
        use super::*;

        #[tokio::test]
        async fn should_be_the_launch_folder_for_a_pane_that_never_moved() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;

            let pane = place.spawn(&daemon.client, &["true"]).await;

            assert_eq!(pane.folder, place.workspace.dir, "as it starts");
            let exited = in_state(&daemon.client, pane.id, AgentState::Exited).await;
            assert_eq!(exited.folder, place.workspace.dir);
        }
    }

    mod osc_7 {
        use super::*;

        #[tokio::test]
        async fn should_be_the_folder_the_shell_last_reported() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let first = daemon.new_dir("first");
            let reported = daemon.new_dir("reported");
            let script = report(&first, &report(&reported, "exec sleep 600"));

            let pane = place.spawn(&daemon.client, &sh(&script)).await;

            // Its process stays in the workspace's dir: the report wins.
            in_folder(&daemon.client, pane.id, &reported).await;
        }

        #[tokio::test]
        async fn should_ignore_a_report_from_another_host() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let script = "printf '\\033]7;file://elsewhere/srv\\033\\\\'; echo reported; \
                          exec sleep 600";

            let pane = place.spawn(&daemon.client, &sh(script)).await;

            printed(&daemon.client, pane.id, "reported").await;
            assert_eq!(
                read(&daemon.client, pane.id).await.folder,
                place.workspace.dir
            );
        }

        #[tokio::test]
        async fn should_keep_the_last_report_once_the_pane_has_exited() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let reported = daemon.new_dir("reported");

            let pane = place
                .spawn(&daemon.client, &sh(&report(&reported, "exit 0")))
                .await;

            let exited = in_state(&daemon.client, pane.id, AgentState::Exited).await;
            assert_eq!(Path::new(&exited.folder), reported);
        }
    }

    mod working_directory {
        use super::*;

        #[tokio::test]
        async fn should_be_where_the_process_moved_without_an_osc_7() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let sub = sub_dir(&place, "app");

            let pane = place
                .spawn(&daemon.client, &sh("cd app && exec sleep 600"))
                .await;

            in_folder(&daemon.client, pane.id, &sub).await;
        }

        #[tokio::test]
        async fn should_keep_the_folder_once_the_pane_has_exited() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let sub = sub_dir(&place, "app");
            let pane = place
                .spawn(&daemon.client, &sh("cd app && read line"))
                .await;
            in_folder(&daemon.client, pane.id, &sub).await;

            type_now(&daemon.client, pane.id, "\r").await;

            let exited = in_state(&daemon.client, pane.id, AgentState::Exited).await;
            assert_eq!(Path::new(&exited.folder), sub);
            assert_eq!(exited.cwd, place.workspace.dir, "the launch folder stays");
        }
    }

    mod changed {
        use super::*;

        #[tokio::test]
        async fn should_tell_clients_when_the_pane_moves() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let sub = sub_dir(&place, "app");
            let pane = place.spawn(&daemon.client, &["sh"]).await;
            printed(&daemon.client, pane.id, "$ ").await;
            let mut ws = Ws::connect(&daemon).await;

            // Typed, not read: only the daemon's own reading of the folder sends `changed`.
            type_now(&daemon.client, pane.id, "cd app\r").await;

            ws.until(|frame| match frame {
                Frame::Message(ServerMessage::Changed {
                    entity: Entity::Pane,
                    id,
                }) if id == pane.id.to_string() => Some(()),
                _ => None,
            })
            .await;
            assert_eq!(Path::new(&read(&daemon.client, pane.id).await.folder), sub);
        }
    }
}

mod resume_after_restart {
    use super::*;

    /// An interactive `sh` pane moved into `app` in the workspace, without OSC 7.
    async fn moved_shell(daemon: &TestDaemon, place: &Place) -> (Pane, PathBuf) {
        let sub = sub_dir(place, "app");
        let pane = place.spawn(&daemon.client, &["sh"]).await;
        printed(&daemon.client, pane.id, "$ ").await;
        type_now(&daemon.client, pane.id, "cd app && echo moved-$((40+2))\r").await;
        printed(&daemon.client, pane.id, "moved-42").await;
        (pane, sub)
    }

    #[tokio::test]
    async fn should_resume_a_pane_in_its_last_known_folder() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let (pane, sub) = moved_shell(&daemon, &place).await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        type_now(&daemon.client, pane.id, "pwd; echo resumed-$((40+2))\r").await;
        let text = printed(&daemon.client, pane.id, "resumed-42").await;
        assert!(text.contains(&format!("{}\r\n", sub.display())), "{text}");
        assert_eq!(Path::new(&read(&daemon.client, pane.id).await.folder), sub);
    }

    #[tokio::test]
    async fn should_resume_a_pane_in_home_once_its_folder_is_gone() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let (pane, sub) = moved_shell(&daemon, &place).await;
        let home = daemon.root().join("home");
        let (_, dirs) = daemon.terminate();
        std::fs::remove_dir(&sub).expect("removing the pane's folder");

        let daemon = TestDaemon::start_in(dirs).await;

        type_now(&daemon.client, pane.id, "pwd; echo resumed-$((40+2))\r").await;
        let text = printed(&daemon.client, pane.id, "resumed-42").await;
        assert!(text.contains(&format!("{}\r\n", home.display())), "{text}");
        let resumed = read(&daemon.client, pane.id).await;
        assert_eq!(Path::new(&resumed.folder), home);
        assert_eq!(resumed.cwd, place.workspace.dir, "the launch folder stays");
    }
}
