//! A Pane's current folder (spec §6): the folder its shell last reported with OSC 7, else its
//! process's working directory, else the folder it launched in; kept after it exits, sent as
//! `changed` when it moves, and where it resumes after a restart and starts again on Open
//! shell or a fresh Restart agent (spec §4). Panes run `sh`, which sends no OSC 7 of its own,
//! and the stub agents in `support::agents`.

use std::path::{Path, PathBuf};

use agentdeck_client::Client;
use agentdeck_client::types::{InputWhen, Pane, PaneInput, PaneRespawn, PaneRun};
use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{Agent, AgentState, PaneId};

use crate::support::TestDaemon;
use crate::support::agents::{args, launch_folders, launches};
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

/// A dir `name` inside the place's folder.
fn sub_dir(place: &Place, name: &str) -> PathBuf {
    let dir = place.dir.join(name);
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

            assert_eq!(pane.folder, place.folder(), "as it starts");
            let exited = in_state(&daemon.client, pane.id, AgentState::Exited).await;
            assert_eq!(exited.folder, place.folder());
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

            // Its process stays in the place's folder: the report wins.
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
            assert_eq!(read(&daemon.client, pane.id).await.folder, place.folder());
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
            assert_eq!(exited.cwd, place.folder(), "the launch folder stays");
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
        assert_eq!(resumed.cwd, place.folder(), "the launch folder stays");
    }
}

mod post_v1_panes_id_respawn {
    use super::*;

    /// Respawns `pane` to run `run`; the pane as it is now.
    async fn respawn(client: &Client, pane: PaneId, run: PaneRun) -> Pane {
        client
            .pane_respawn()
            .id(pane)
            .body(PaneRespawn { run })
            .send()
            .await
            .expect("respawn")
            .into_inner()
    }

    /// Types `text` into the pane and Enter after it, at once.
    async fn submit_now(client: &Client, id: PaneId, text: &str) {
        client
            .pane_input()
            .id(id)
            .body(PaneInput {
                text: text.into(),
                when: InputWhen::Now,
                submit: true,
            })
            .send()
            .await
            .expect("submit to the pane");
    }

    /// Prints the folder the pane's shell is in, once it starts reading; its output.
    async fn shell_folder(client: &Client, id: PaneId) -> String {
        type_now(client, id, "pwd; echo back-$((40+2))\r").await;
        printed(client, id, "back-42").await
    }

    /// Asserts the shell printed `dir` as its folder.
    fn printed_folder(text: &str, dir: &Path) {
        assert!(text.contains(&format!("{}\r\n", dir.display())), "{text}");
    }

    /// An interactive `sh` pane that moved into `app` in the place's folder, then exited.
    async fn exited_moved_shell(daemon: &TestDaemon, place: &Place) -> (Pane, PathBuf) {
        let sub = sub_dir(place, "app");
        let pane = place.spawn(&daemon.client, &["sh"]).await;
        printed(&daemon.client, pane.id, "$ ").await;
        type_now(&daemon.client, pane.id, "cd app\r").await;
        in_folder(&daemon.client, pane.id, &sub).await;
        type_now(&daemon.client, pane.id, "exit\r").await;
        let exited = in_state(&daemon.client, pane.id, AgentState::Exited).await;
        assert_eq!(Path::new(&exited.folder), sub, "where it exited");
        (pane, sub)
    }

    /// Launches `agent` and has it exit at its prompt, once a codex has its session.
    async fn exited_agent(daemon: &TestDaemon, place: &Place, agent: Agent) -> Pane {
        let pane = place.launch(&daemon.client, agent).await;
        let title = match agent {
            Agent::Claude => "\u{1b}]0;\u{2733} Claude Code",
            _ => "\u{1b}]0;codex",
        };
        printed(&daemon.client, pane.id, title).await;
        eventually("the agent's session", || async {
            read(&daemon.client, pane.id).await.session_id
        })
        .await;
        submit_now(&daemon.client, pane.id, "/exit").await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await
    }

    /// Restarts the daemon with exited `pane`'s last known folder set to `folder`, as for an
    /// agent whose folder moved, and its session forgotten when `forget_session`, as for a
    /// Codex pane whose session file never turned up. An exited pane stays exited.
    async fn with_last_known_folder(
        daemon: TestDaemon,
        pane: PaneId,
        folder: &Path,
        forget_session: bool,
    ) -> TestDaemon {
        let (_, dirs) = daemon.terminate();
        let db = rusqlite::Connection::open(dirs.path().join("data/agentdeck/agentdeck.db"))
            .expect("opening the database");
        let folder = folder.to_str().expect("a UTF-8 path");
        db.execute(
            "UPDATE panes SET folder = ?2 WHERE id = ?1",
            [pane.to_string().as_str(), folder],
        )
        .expect("moving the pane");
        if forget_session {
            db.execute(
                "UPDATE panes SET session_id = NULL WHERE id = ?1",
                [pane.to_string()],
            )
            .expect("forgetting the session");
        }
        TestDaemon::start_in(dirs).await
    }

    /// Waits for the pane's stub agent's `n`th launch; its argv and the folder it started in.
    async fn launch(daemon: &TestDaemon, pane: PaneId, n: usize) -> (Vec<String>, PathBuf) {
        eventually(&format!("launch {n} of the stub agent"), || async {
            let argv = launches(daemon, pane).into_iter().nth(n - 1)?;
            let folder = launch_folders(daemon, pane).into_iter().nth(n - 1)?;
            Some((argv, folder))
        })
        .await
    }

    mod open_shell {
        use super::*;

        #[tokio::test]
        async fn should_start_in_the_last_known_folder_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let (pane, sub) = exited_moved_shell(&daemon, &place).await;

            let shell = respawn(&daemon.client, pane.id, PaneRun::Shell).await;

            printed_folder(&shell_folder(&daemon.client, pane.id).await, &sub);
            assert_eq!(
                (Path::new(&shell.cwd), Path::new(&shell.folder)),
                (&*sub, &*sub)
            );
        }

        #[tokio::test]
        async fn should_start_in_the_launch_folder_once_the_last_known_one_is_gone_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let (pane, sub) = exited_moved_shell(&daemon, &place).await;
            std::fs::remove_dir(&sub).expect("removing the pane's folder");

            let shell = respawn(&daemon.client, pane.id, PaneRun::Shell).await;

            printed_folder(&shell_folder(&daemon.client, pane.id).await, &place.dir);
            assert_eq!(shell.folder, place.folder());
        }

        #[tokio::test]
        async fn should_start_in_home_once_the_launch_folder_is_gone_too_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let (pane, _) = exited_moved_shell(&daemon, &place).await;
            std::fs::remove_dir_all(&place.dir).expect("removing both folders");

            let shell = respawn(&daemon.client, pane.id, PaneRun::Shell).await;

            printed_folder(&shell_folder(&daemon.client, pane.id).await, &daemon.home());
            assert_eq!(Path::new(&shell.folder), daemon.home());
        }

        #[tokio::test]
        async fn should_start_an_exited_agent_pane_in_its_last_known_folder_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = exited_agent(&daemon, &place, Agent::Claude).await;
            let elsewhere = daemon.new_dir("elsewhere");
            let daemon = with_last_known_folder(daemon, pane.id, &elsewhere, false).await;

            respawn(&daemon.client, pane.id, PaneRun::Shell).await;

            printed_folder(&shell_folder(&daemon.client, pane.id).await, &elsewhere);
        }
    }

    mod restart_agent {
        use super::*;

        mod fresh {
            use super::*;

            #[tokio::test]
            async fn should_start_in_the_last_known_folder_200() {
                let daemon = TestDaemon::start().await;
                let place = Place::new(&daemon).await;
                let pane = exited_agent(&daemon, &place, Agent::Codex).await;
                let elsewhere = daemon.new_dir("elsewhere");
                let daemon = with_last_known_folder(daemon, pane.id, &elsewhere, true).await;

                let fresh = respawn(&daemon.client, pane.id, PaneRun::Agent).await;

                let (argv, folder) = launch(&daemon, pane.id, 2).await;
                assert!(argv.is_empty(), "afresh, with no prompt: {argv:?}");
                assert_eq!(folder, elsewhere);
                assert_eq!(
                    (Path::new(&fresh.cwd), Path::new(&fresh.folder)),
                    (&*elsewhere, &*elsewhere),
                    "the folder its new session needs"
                );
            }

            #[tokio::test]
            async fn should_start_in_the_launch_folder_once_the_last_known_one_is_gone_200() {
                let daemon = TestDaemon::start().await;
                let place = Place::new(&daemon).await;
                let pane = exited_agent(&daemon, &place, Agent::Codex).await;
                let gone = daemon.root().join("gone");
                let daemon = with_last_known_folder(daemon, pane.id, &gone, true).await;

                respawn(&daemon.client, pane.id, PaneRun::Agent).await;

                assert_eq!(launch(&daemon, pane.id, 2).await.1, place.dir);
            }

            #[tokio::test]
            async fn should_start_in_home_once_the_launch_folder_is_gone_too_200() {
                let daemon = TestDaemon::start().await;
                let place = Place::new(&daemon).await;
                let pane = exited_agent(&daemon, &place, Agent::Codex).await;
                let gone = daemon.root().join("gone");
                let daemon = with_last_known_folder(daemon, pane.id, &gone, true).await;
                std::fs::remove_dir_all(&place.dir).expect("removing the launch folder");

                let fresh = respawn(&daemon.client, pane.id, PaneRun::Agent).await;

                assert_eq!(launch(&daemon, pane.id, 2).await.1, daemon.home());
                assert_eq!(Path::new(&fresh.cwd), daemon.home());
            }

            #[tokio::test]
            async fn should_run_an_other_agent_again_in_its_last_known_folder_200() {
                let daemon = TestDaemon::start().await;
                let place = Place::new(&daemon).await;
                let sub = sub_dir(&place, "app");
                let script = "echo \"in:$PWD\"; cd app 2>/dev/null; read line";
                let pane = (daemon.client.pane_create())
                    .body(agentdeck_client::types::NewPane {
                        argv: Some(sh(script).iter().map(|arg| (*arg).to_owned()).collect()),
                        ..place.new_agent(Agent::Other)
                    })
                    .send()
                    .await
                    .expect("launch an other agent")
                    .into_inner();
                in_folder(&daemon.client, pane.id, &sub).await;
                type_now(&daemon.client, pane.id, "\r").await;
                in_state(&daemon.client, pane.id, AgentState::Exited).await;

                respawn(&daemon.client, pane.id, PaneRun::Agent).await;

                printed(
                    &daemon.client,
                    pane.id,
                    &format!("in:{}\r\n", sub.display()),
                )
                .await;
            }
        }

        mod resume {
            use super::*;

            #[tokio::test]
            async fn should_resume_claude_in_its_launch_folder_not_its_last_known_one_200() {
                let daemon = TestDaemon::start().await;
                let place = Place::new(&daemon).await;
                let pane = exited_agent(&daemon, &place, Agent::Claude).await;
                let session = pane.session_id.clone().expect("a claude session");
                let elsewhere = daemon.new_dir("elsewhere");
                let daemon = with_last_known_folder(daemon, pane.id, &elsewhere, false).await;

                let resumed = respawn(&daemon.client, pane.id, PaneRun::Agent).await;

                let (argv, folder) = launch(&daemon, pane.id, 2).await;
                assert_eq!(argv, args(&["--resume", &session]));
                assert_eq!(
                    folder, place.dir,
                    "the folder Claude Code keys its session by"
                );
                assert_eq!(
                    (resumed.cwd.as_str(), resumed.folder.as_str()),
                    (place.folder().as_str(), place.folder().as_str())
                );
            }

            #[tokio::test]
            async fn should_resume_codex_in_its_launch_folder_not_its_last_known_one_200() {
                let daemon = TestDaemon::start().await;
                let place = Place::new(&daemon).await;
                let pane = exited_agent(&daemon, &place, Agent::Codex).await;
                let session = pane.session_id.clone().expect("a codex session");
                let elsewhere = daemon.new_dir("elsewhere");
                let daemon = with_last_known_folder(daemon, pane.id, &elsewhere, false).await;

                respawn(&daemon.client, pane.id, PaneRun::Agent).await;

                let (argv, folder) = launch(&daemon, pane.id, 2).await;
                assert_eq!(argv, args(&["resume", &session]));
                assert_eq!(
                    folder, place.dir,
                    "the folder Codex recorded its session in"
                );
            }

            #[tokio::test]
            async fn should_start_afresh_in_the_last_known_folder_once_the_launch_folder_is_gone_200()
             {
                let daemon = TestDaemon::start().await;
                let place = Place::new(&daemon).await;
                let pane = exited_agent(&daemon, &place, Agent::Claude).await;
                let old = pane.session_id.clone().expect("a claude session");
                let elsewhere = daemon.new_dir("elsewhere");
                let daemon = with_last_known_folder(daemon, pane.id, &elsewhere, false).await;
                std::fs::remove_dir_all(&place.dir).expect("removing the launch folder");

                let fresh = respawn(&daemon.client, pane.id, PaneRun::Agent).await;

                let (argv, folder) = launch(&daemon, pane.id, 2).await;
                assert_eq!(argv[0], "--session-id", "afresh: {argv:?}");
                assert_ne!(argv[1], old);
                assert_eq!(folder, elsewhere);
                assert_eq!(fresh.session_id.as_deref(), Some(argv[1].as_str()));
                assert_eq!(Path::new(&fresh.cwd), elsewhere);
            }

            #[tokio::test]
            async fn should_resume_a_session_started_afresh_where_it_started_200() {
                let daemon = TestDaemon::start().await;
                let place = Place::new(&daemon).await;
                let pane = exited_agent(&daemon, &place, Agent::Claude).await;
                let elsewhere = daemon.new_dir("elsewhere");
                let daemon = with_last_known_folder(daemon, pane.id, &elsewhere, false).await;
                std::fs::remove_dir_all(&place.dir).expect("removing the launch folder");
                let fresh = respawn(&daemon.client, pane.id, PaneRun::Agent).await;
                let session = fresh.session_id.clone().expect("its new session");
                printed(&daemon.client, pane.id, "\u{1b}]0;\u{2733} Claude Code").await;
                submit_now(&daemon.client, pane.id, "/exit").await;
                in_state(&daemon.client, pane.id, AgentState::Exited).await;

                respawn(&daemon.client, pane.id, PaneRun::Agent).await;

                let (argv, folder) = launch(&daemon, pane.id, 3).await;
                assert_eq!(argv, args(&["--resume", &session]));
                assert_eq!(folder, elsewhere);
            }
        }
    }
}
