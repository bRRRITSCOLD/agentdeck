//! Agent panes (spec §6): launching `claude` and `codex` with a session and no prompt,
//! restarting them and resuming panes after a restart. The agents are the stubs in
//! `support::agents`; no test runs a real agent CLI.

use std::time::Duration;

use agentdeck_client::Client;
use agentdeck_client::types::{InputWhen, NewTask, Pane, PaneInput, PanePatch, Task};
use agentdeck_core::{Agent, AgentState, PaneId, PaneKind, TaskId, WorkspaceId};

use crate::support::agents::{args, codex_session, launches, task_var, typed};
use crate::support::terminal::{Place, eventually, holds_for, in_state, printed, sh};
use crate::support::{TestDaemon, error_response};

async fn task(client: &Client, workspace: WorkspaceId, title: &str, body: &str) -> Task {
    client
        .task_create()
        .body(NewTask {
            title: title.into(),
            body: Some(body.into()),
            status: None,
            workspace_id: Some(workspace),
        })
        .send()
        .await
        .expect("create a task")
        .into_inner()
}

/// Launches an agent of `agent`, links it to `task` and has it exit at its prompt.
async fn exited_agent(daemon: &TestDaemon, place: &Place, agent: Agent, task: &Task) -> Pane {
    let pane = place.launch(&daemon.client, agent).await;
    link(&daemon.client, pane.id, Some(task.id)).await;
    // Each stub sets its title once it reads what is typed; the codex stub never idles.
    let title = match agent {
        Agent::Claude => "\u{1b}]0;\u{2733} Claude Code",
        _ => "\u{1b}]0;codex",
    };
    printed(&daemon.client, pane.id, title).await;
    submit_now(&daemon.client, pane.id, "/exit").await;
    in_state(&daemon.client, pane.id, AgentState::Exited).await;
    pane
}

/// Links `pane` to `task`, or unlinks it.
async fn link(client: &Client, pane: PaneId, task: Option<TaskId>) {
    client
        .pane_update()
        .id(pane)
        .body(PanePatch { task_id: task })
        .send()
        .await
        .expect("link the pane");
}

/// Types `text` into `pane` at once, as is.
async fn type_now(client: &Client, pane: PaneId, text: &str) {
    input_now(client, pane, text, false).await;
}

/// Submits `text` to `pane` at once: typed, then Enter.
async fn submit_now(client: &Client, pane: PaneId, text: &str) {
    input_now(client, pane, text, true).await;
}

async fn input_now(client: &Client, pane: PaneId, text: &str, submit: bool) {
    client
        .pane_input()
        .id(pane)
        .body(PaneInput {
            text: text.into(),
            when: InputWhen::Now,
            submit,
        })
        .send()
        .await
        .expect("type into the pane");
}

/// Waits until the pane's stub has been launched `n` times; returns every launch.
async fn launched(daemon: &TestDaemon, pane: PaneId, n: usize) -> Vec<Vec<String>> {
    eventually(&format!("launch {n} of the stub agent"), || async {
        let launches = launches(daemon, pane);
        (launches.len() >= n).then_some(launches)
    })
    .await
}

/// How long a test waits to see that the daemon starts no further launch of a pane's agent:
/// it starts one afresh as soon as a resume exits, well within this.
const NO_FURTHER_LAUNCH: Duration = Duration::from_secs(1);

/// Fails unless the pane's stub stays launched exactly `n` times for [`NO_FURTHER_LAUNCH`].
async fn launched_no_more_than(daemon: &TestDaemon, pane: PaneId, n: usize) {
    holds_for(
        &format!("the stub agent to stay at {n} launches"),
        NO_FURTHER_LAUNCH,
        || async { launches(daemon, pane).len() == n },
    )
    .await;
}

/// Asserts `launch` starts claude afresh, on a session other than `old`, with no prompt;
/// returns its session.
fn fresh_claude(launch: &[String], old: &str) -> String {
    let session = launch
        .iter()
        .skip_while(|arg| *arg != "--session-id")
        .nth(1)
        .unwrap_or_else(|| panic!("a fresh start on a session: {launch:?}"))
        .clone();
    assert_ne!(session, old, "a new session");
    assert!(!launch.contains(&"--resume".to_owned()), "{launch:?}");
    assert_eq!(launch, args(&["--session-id", &session]), "no prompt");
    session
}

async fn pane(client: &Client, id: PaneId) -> Pane {
    client
        .pane_get()
        .id(id)
        .send()
        .await
        .expect("read the pane")
        .into_inner()
}

mod post_v1_panes {
    use super::*;

    mod claude {
        use super::*;

        #[tokio::test]
        async fn should_start_claude_on_a_session_id_it_chose_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;

            let response = daemon
                .client
                .pane_create()
                .body(place.new_agent(Agent::Claude))
                .send()
                .await
                .expect("launch claude");

            assert_eq!(response.status().as_u16(), 201);
            let pane = response.into_inner();
            let session = pane.session_id.clone().expect("a session id");
            assert!(uuid::Uuid::parse_str(&session).is_ok(), "{session}");
            assert_eq!(pane.kind, PaneKind::Agent);
            let args = args(&["--session-id", &session]);
            assert_eq!(pane.argv[0], "claude");
            assert_eq!(pane.argv[1..], args);
            assert_eq!(launched(&daemon, pane.id, 1).await, [args]);
            assert_eq!(task_var(&daemon, pane.id).as_deref(), Some("unset"));
            in_state(&daemon.client, pane.id, AgentState::Idle).await;
        }

        #[tokio::test]
        async fn should_start_with_no_prompt_and_no_task_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            task(
                &daemon.client,
                place.workspace.id,
                "Fix it",
                "It fails on Safari.",
            )
            .await;

            let pane = place.launch(&daemon.client, Agent::Claude).await;

            let session = pane.session_id.clone().expect("a session id");
            assert_eq!(pane.argv, ["claude", "--session-id", session.as_str()]);
            assert_eq!(pane.task_id, None);
            in_state(&daemon.client, pane.id, AgentState::Idle).await;
            assert_eq!(typed(&daemon, pane.id), "", "nothing typed into it");
        }

        #[tokio::test]
        async fn should_reject_an_argv_400() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let mut new = place.new_agent(Agent::Claude);
            new.argv = Some(vec!["claude".into(), "--model".into(), "opus".into()]);

            let (status, body) = error_response(daemon.client.pane_create().body(new).send().await);

            assert_eq!(status, 400);
            assert_eq!(body.error_code.as_deref(), Some("invalid_argv"));
        }

        #[tokio::test]
        async fn should_reject_an_agent_pane_that_names_no_agent_400() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let mut new = place.new_agent(Agent::Claude);
            new.agent = None;

            let (status, body) = error_response(daemon.client.pane_create().body(new).send().await);

            assert_eq!(status, 400);
            assert_eq!(body.error_code.as_deref(), Some("unsupported_kind"));
        }
    }

    mod codex {
        use super::*;

        #[tokio::test]
        async fn should_find_the_session_codex_wrote_after_launch_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let earlier = daemon.root().join("home/.codex/sessions/2026/11/10");
            std::fs::create_dir_all(&earlier).expect("creating the sessions dir");
            std::fs::write(
                earlier
                    .join("rollout-2026-11-10T09-00-00-00000000-0000-4000-8000-000000000001.jsonl"),
                "",
            )
            .expect("writing an earlier session");

            let pane = place.launch(&daemon.client, Agent::Codex).await;

            assert_eq!(pane.argv, ["codex"], "no prompt");
            assert_eq!(launched(&daemon, pane.id, 1).await, [Vec::<String>::new()]);
            assert_eq!(task_var(&daemon, pane.id).as_deref(), Some("unset"));
            let written = eventually("the stub to write its session", || async {
                codex_session(&daemon, pane.id)
            })
            .await;
            let found = eventually("the pane's session id", || async {
                super::pane(&daemon.client, pane.id).await.session_id
            })
            .await;
            assert_eq!(found, written);
        }
    }
}

mod resume_after_restart {
    use super::*;

    #[tokio::test]
    async fn should_resume_claude_and_codex_on_their_sessions() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let claude = place.launch(&daemon.client, Agent::Claude).await;
        let codex = place.launch(&daemon.client, Agent::Codex).await;
        let claude_session = claude.session_id.clone().expect("a claude session");
        let codex_session = eventually("the codex session", || async {
            pane(&daemon.client, codex.id).await.session_id
        })
        .await;
        in_state(&daemon.client, claude.id, AgentState::Idle).await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        let claude_launches = launched(&daemon, claude.id, 2).await;
        assert_eq!(claude_launches[1], args(&["--resume", &claude_session]));
        let codex_launches = launched(&daemon, codex.id, 2).await;
        assert_eq!(codex_launches[1], args(&["resume", &codex_session]));
        let resumed = in_state(&daemon.client, claude.id, AgentState::Idle).await;
        assert_eq!(resumed.session_id.as_deref(), Some(claude_session.as_str()));
        assert_eq!(resumed.argv, claude.argv, "argv stays as first launched");
        submit_now(&daemon.client, claude.id, "still here").await;
        eventually("input to reach the resumed claude", || async {
            (typed(&daemon, claude.id) == "still here\n").then_some(())
        })
        .await;
    }

    #[tokio::test]
    async fn should_give_a_resumed_agent_its_task() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let claude = place.launch(&daemon.client, Agent::Claude).await;
        link(&daemon.client, claude.id, Some(task.id)).await;
        in_state(&daemon.client, claude.id, AgentState::Idle).await;
        let (_, dirs) = daemon.terminate();
        let first = dirs.path().join(format!("agents/{}/task", claude.id));
        std::fs::remove_file(first).expect("forgetting the first launch's task");

        let daemon = TestDaemon::start_in(dirs).await;

        launched(&daemon, claude.id, 2).await;
        assert_eq!(task_var(&daemon, claude.id), Some(task.id.to_string()));
    }

    #[tokio::test]
    async fn should_start_a_claude_that_cannot_find_its_session_afresh_on_its_task() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let claude = place.launch(&daemon.client, Agent::Claude).await;
        link(&daemon.client, claude.id, Some(task.id)).await;
        let old = claude.session_id.clone().expect("a claude session");
        in_state(&daemon.client, claude.id, AgentState::Idle).await;
        let (_, dirs) = daemon.terminate();
        std::fs::write(
            std::path::Path::new(&place.workspace.dir).join("lost-session"),
            "",
        )
        .expect("losing the session");

        let daemon = TestDaemon::start_in(dirs).await;

        let launches = launched(&daemon, claude.id, 3).await;
        assert_eq!(launches[1], args(&["--resume", &old]));
        let session = fresh_claude(&launches[2], &old);
        let fresh = in_state(&daemon.client, claude.id, AgentState::Idle).await;
        assert_eq!(fresh.session_id, Some(session));
        assert_eq!(fresh.task_id, Some(task.id));
    }

    #[tokio::test]
    async fn should_bring_other_panes_back_as_a_shell_in_their_cwd() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let shell = place
            .spawn(&daemon.client, &sh("echo started; exec sleep 600"))
            .await;
        printed(&daemon.client, shell.id, "started").await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        type_now(&daemon.client, shell.id, "pwd; echo resumed-$((40+2))\r").await;
        let text = printed(&daemon.client, shell.id, "resumed-42").await;
        assert!(text.contains(&place.workspace.dir), "{text}");
        assert!(
            !text.contains("started"),
            "a fresh process, not the old one"
        );
    }

    #[tokio::test]
    async fn should_leave_an_exited_pane_exited() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let done = place.spawn(&daemon.client, &["true"]).await;
        in_state(&daemon.client, done.id, AgentState::Exited).await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        assert_eq!(
            pane(&daemon.client, done.id).await.state,
            AgentState::Exited
        );
        let (status, body) = error_response(
            daemon
                .client
                .pane_input()
                .id(done.id)
                .body(PaneInput {
                    text: "x".into(),
                    when: InputWhen::Now,
                    submit: false,
                })
                .send()
                .await,
        );
        assert_eq!(status, 409);
        assert_eq!(body.error_code.as_deref(), Some("exited"));
    }
}

mod resume_as_a_shell {
    use agentdeck_client::types::NewPane;

    use super::*;

    #[tokio::test]
    async fn should_turn_an_agent_pane_that_comes_back_as_a_shell_into_a_shell_pane() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = daemon
            .client
            .pane_create()
            .body(NewPane {
                kind: PaneKind::Agent,
                agent: Some(Agent::Other),
                argv: Some(sh("exec sleep 600").map(String::from).to_vec()),
                ..place.new_pane(&[])
            })
            .send()
            .await
            .expect("start an other agent")
            .into_inner();
        link(&daemon.client, pane.id, Some(task.id)).await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        let resumed = pane_now(&daemon.client, pane.id).await;
        assert_eq!(
            (resumed.kind, resumed.agent, resumed.session_id.as_deref()),
            (PaneKind::Shell, None, None)
        );
        assert_eq!(resumed.argv, ["/bin/sh"], "the shell it runs now");
        assert_eq!(resumed.task_id, Some(task.id), "the link stays");
        printed(&daemon.client, pane.id, "$ ").await;
        type_now(&daemon.client, pane.id, "claude\r").await;
        eventually("the shell to host claude", || async {
            (pane_now(&daemon.client, pane.id).await.hosted_agent == Some(Agent::Claude))
                .then_some(())
        })
        .await;
    }

    async fn pane_now(client: &Client, id: PaneId) -> Pane {
        pane(client, id).await
    }
}

mod patch_v1_panes_id {
    use super::*;

    #[tokio::test]
    async fn should_unlink_an_agent_pane_whose_agent_exited_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = exited_agent(&daemon, &place, Agent::Claude, &task).await;

        let response = (daemon.client.pane_update().id(pane.id))
            .body(PanePatch { task_id: None })
            .send()
            .await
            .expect("unlink");

        assert_eq!(response.status().as_u16(), 200);
        let unlinked = response.into_inner();
        assert_eq!(unlinked.task_id, None);
        assert_eq!(unlinked.state, AgentState::Exited);
    }
}

mod post_v1_panes_id_respawn {
    use agentdeck_client::types::{PaneRespawn, PaneRun};

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

    /// The status and error code a refused respawn of `pane` answers with.
    async fn refused(client: &Client, pane: PaneId, run: PaneRun) -> (u16, Option<String>) {
        let (status, body) = error_response(
            client
                .pane_respawn()
                .id(pane)
                .body(PaneRespawn { run })
                .send()
                .await,
        );
        (status, body.error_code)
    }

    #[tokio::test]
    async fn should_resume_an_exited_claude_on_its_session_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = exited_agent(&daemon, &place, Agent::Claude, &task).await;
        let session = pane.session_id.clone().expect("a claude session");

        let restarted = respawn(&daemon.client, pane.id, PaneRun::Agent).await;

        assert_eq!(restarted.id, pane.id);
        assert_eq!(
            (restarted.kind, restarted.agent, restarted.exit_code),
            (PaneKind::Agent, Some(Agent::Claude), None)
        );
        assert_eq!(
            launched(&daemon, pane.id, 2).await[1],
            args(&["--resume", &session])
        );
        assert_eq!(task_var(&daemon, pane.id), Some(task.id.to_string()));
        in_state(&daemon.client, pane.id, AgentState::Idle).await;
        submit_now(&daemon.client, pane.id, "still here").await;
        eventually("input to reach the restarted claude", || async {
            typed(&daemon, pane.id)
                .ends_with("still here\n")
                .then_some(())
        })
        .await;
    }

    mod resume_that_fails_at_once {
        use super::*;

        /// An exited claude on `task` whose next starts find `file` in its folder.
        async fn exited_with(daemon: &TestDaemon, task: &Task, place: &Place, file: &str) -> Pane {
            let pane = exited_agent(daemon, place, Agent::Claude, task).await;
            std::fs::write(std::path::Path::new(&place.workspace.dir).join(file), "")
                .expect("writing the stub's file");
            pane
        }

        #[tokio::test]
        async fn should_start_claude_afresh_on_its_task_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
            let pane = exited_with(&daemon, &task, &place, "lost-session").await;
            let old = pane.session_id.clone().expect("a claude session");

            respawn(&daemon.client, pane.id, PaneRun::Agent).await;

            let launches = launched(&daemon, pane.id, 3).await;
            assert_eq!(launches[1], args(&["--resume", &old]));
            let session = fresh_claude(&launches[2], &old);
            let fresh = in_state(&daemon.client, pane.id, AgentState::Idle).await;
            assert_eq!(
                (fresh.kind, fresh.agent, fresh.session_id),
                (PaneKind::Agent, Some(Agent::Claude), Some(session))
            );
            assert_eq!(task_var(&daemon, pane.id), Some(task.id.to_string()));
            launched_no_more_than(&daemon, pane.id, 3).await;
        }

        #[tokio::test]
        async fn should_start_it_afresh_only_once_when_that_fails_too_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
            let pane = exited_with(&daemon, &task, &place, "broken").await;
            let old = pane.session_id.clone().expect("a claude session");

            respawn(&daemon.client, pane.id, PaneRun::Agent).await;

            let launches = launched(&daemon, pane.id, 3).await;
            let session = fresh_claude(&launches[2], &old);
            let exited = in_state(&daemon.client, pane.id, AgentState::Exited).await;
            assert_eq!(exited.exit_code, Some(1));
            launched_no_more_than(&daemon, pane.id, 3).await;
            assert_eq!(pane_session(&daemon.client, pane.id).await, Some(session));
        }

        /// Codex exits 1 at once for a session it can't find, as Claude Code does, so the
        /// same fallback covers it with no reading of its screen (#262).
        #[tokio::test]
        async fn should_start_codex_afresh_on_its_task_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
            let pane = place.launch(&daemon.client, Agent::Codex).await;
            link(&daemon.client, pane.id, Some(task.id)).await;
            let old = eventually("the codex session", || async {
                pane_session(&daemon.client, pane.id).await
            })
            .await;
            printed(&daemon.client, pane.id, "\u{1b}]0;codex").await;
            submit_now(&daemon.client, pane.id, "/exit").await;
            in_state(&daemon.client, pane.id, AgentState::Exited).await;
            std::fs::write(
                std::path::Path::new(&place.workspace.dir).join("lost-session"),
                "",
            )
            .expect("losing the session");

            respawn(&daemon.client, pane.id, PaneRun::Agent).await;

            let launches = launched(&daemon, pane.id, 3).await;
            assert_eq!(launches[1], args(&["resume", &old]));
            assert!(
                launches[2].is_empty(),
                "afresh, with no prompt: {launches:?}"
            );
            let session = eventually("the new codex session", || async {
                pane_session(&daemon.client, pane.id)
                    .await
                    .filter(|session| *session != old)
            })
            .await;
            assert_eq!(Some(session), codex_session(&daemon, pane.id));
            assert_eq!(task_var(&daemon, pane.id), Some(task.id.to_string()));
            launched_no_more_than(&daemon, pane.id, 3).await;
        }

        #[tokio::test]
        async fn should_leave_a_resumed_claude_that_exits_cleanly_exited_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
            let pane = exited_agent(&daemon, &place, Agent::Claude, &task).await;
            let old = pane.session_id.clone().expect("a claude session");

            respawn(&daemon.client, pane.id, PaneRun::Agent).await;
            in_state(&daemon.client, pane.id, AgentState::Idle).await;
            submit_now(&daemon.client, pane.id, "/exit").await;

            let exited = in_state(&daemon.client, pane.id, AgentState::Exited).await;
            assert_eq!(exited.exit_code, Some(0));
            launched_no_more_than(&daemon, pane.id, 2).await;
            assert_eq!(pane_session(&daemon.client, pane.id).await, Some(old));
        }
    }

    #[tokio::test]
    async fn should_start_an_agent_whose_session_is_not_known_afresh_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = exited_agent(&daemon, &place, Agent::Codex, &task).await;
        let (_, dirs) = daemon.terminate();
        // As for a Codex pane whose session file never turned up.
        rusqlite::Connection::open(dirs.path().join("data/agentdeck/agentdeck.db"))
            .expect("opening the database")
            .execute(
                "UPDATE panes SET session_id = NULL WHERE id = ?1",
                [pane.id.to_string()],
            )
            .expect("forgetting the session");
        let daemon = TestDaemon::start_in(dirs).await;

        respawn(&daemon.client, pane.id, PaneRun::Agent).await;

        let again = &launched(&daemon, pane.id, 2).await[1];
        assert!(again.is_empty(), "afresh, with no prompt: {again:?}");
        let session = eventually("the new codex session", || async {
            pane_session(&daemon.client, pane.id).await
        })
        .await;
        assert_eq!(Some(session), codex_session(&daemon, pane.id));
    }

    #[tokio::test]
    async fn should_turn_an_exited_agent_pane_into_a_shell_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = exited_agent(&daemon, &place, Agent::Claude, &task).await;

        let shell = respawn(&daemon.client, pane.id, PaneRun::Shell).await;

        assert_eq!(
            (shell.kind, shell.agent, shell.session_id.as_deref()),
            (PaneKind::Shell, None, None)
        );
        assert_eq!(shell.task_id, Some(task.id));
        printed(&daemon.client, pane.id, "$ ").await;
        type_now(&daemon.client, pane.id, "pwd; echo back-$((40+2))\r").await;
        let text = printed(&daemon.client, pane.id, "back-42").await;
        assert!(text.contains(&place.workspace.dir), "{text}");
        type_now(&daemon.client, pane.id, "claude\r").await;
        eventually("the shell to host claude", || async {
            (super::pane(&daemon.client, pane.id).await.hosted_agent == Some(Agent::Claude))
                .then_some(())
        })
        .await;
    }

    #[tokio::test]
    async fn should_refuse_a_pane_whose_process_still_runs_409() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.launch(&daemon.client, Agent::Claude).await;

        assert_eq!(
            refused(&daemon.client, pane.id, PaneRun::Shell).await,
            (409, Some("running".into()))
        );
    }

    #[tokio::test]
    async fn should_refuse_to_run_an_agent_in_a_shell_pane_400() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &["true"]).await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;

        assert_eq!(
            refused(&daemon.client, pane.id, PaneRun::Agent).await,
            (400, Some("unsupported_kind".into()))
        );
    }

    async fn pane_session(client: &Client, id: PaneId) -> Option<String> {
        pane(client, id).await.session_id
    }
}

mod post_v1_panes_id_stop {
    use agentdeck_client::types::{NewPane, PaneRespawn, PaneRun};

    use super::*;
    use crate::support::terminal::alive;

    async fn stop(client: &Client, pane: PaneId) -> u16 {
        let response = client.pane_stop().id(pane).send().await.expect("stop");
        response.status().as_u16()
    }

    /// The status and error code a refused stop of `pane` answers with.
    async fn refused(client: &Client, pane: PaneId) -> (u16, Option<String>) {
        let (status, body) = error_response(client.pane_stop().id(pane).send().await);
        (status, body.error_code)
    }

    /// A claude pane at its prompt, launched on its own session.
    async fn idle_claude(daemon: &TestDaemon, place: &Place) -> Pane {
        let pane = place.launch(&daemon.client, Agent::Claude).await;
        printed(&daemon.client, pane.id, "\u{1b}]0;\u{2733} Claude Code").await;
        pane
    }

    #[tokio::test]
    async fn should_stop_the_agent_and_keep_the_pane_exited_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let launched = idle_claude(&daemon, &place).await;

        assert_eq!(stop(&daemon.client, launched.id).await, 204);

        let stopped = in_state(&daemon.client, launched.id, AgentState::Exited).await;
        assert_eq!(
            (stopped.kind, stopped.agent, &stopped.session_id),
            (PaneKind::Agent, Some(Agent::Claude), &launched.session_id)
        );
        let tabs = (daemon.client.tab_list().id(place.workspace.id).send().await)
            .expect("list tabs")
            .into_inner();
        assert_eq!(tabs[0].id, place.tab.id, "the pane's tab stays");
    }

    #[tokio::test]
    async fn should_kill_a_process_that_ignores_sighup_after_3_s_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let script = "trap '' HUP; echo pid:$$; while :; do sleep 0.1; done";
        let pane = (daemon.client.pane_create())
            .body(NewPane {
                argv: Some(sh(script).iter().map(|arg| (*arg).to_owned()).collect()),
                ..place.new_agent(Agent::Other)
            })
            .send()
            .await
            .expect("launch an other agent")
            .into_inner();
        let text = printed(&daemon.client, pane.id, "pid:").await;
        let pid: u32 = text[text.find("pid:").expect("a pid") + 4..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|digits| digits.parse().ok())
            .expect("a pid");

        assert_eq!(stop(&daemon.client, pane.id).await, 204);

        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(alive(pid), "SIGHUP alone shouldn't stop it");
        in_state(&daemon.client, pane.id, AgentState::Exited).await;
        assert!(!alive(pid));
    }

    #[tokio::test]
    async fn should_resume_a_stopped_claude_on_restart_agent_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = idle_claude(&daemon, &place).await;
        let session = pane.session_id.clone().expect("a claude session");
        stop(&daemon.client, pane.id).await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;

        (daemon.client.pane_respawn().id(pane.id))
            .body(PaneRespawn {
                run: PaneRun::Agent,
            })
            .send()
            .await
            .expect("respawn");

        assert_eq!(
            launched(&daemon, pane.id, 2).await[1],
            args(&["--resume", &session])
        );
        in_state(&daemon.client, pane.id, AgentState::Idle).await;
    }

    #[tokio::test]
    async fn should_not_start_afresh_a_resume_stopped_at_once_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = idle_claude(&daemon, &place).await;
        stop(&daemon.client, pane.id).await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;
        (daemon.client.pane_respawn().id(pane.id))
            .body(PaneRespawn {
                run: PaneRun::Agent,
            })
            .send()
            .await
            .expect("respawn");
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        assert_eq!(stop(&daemon.client, pane.id).await, 204);

        in_state(&daemon.client, pane.id, AgentState::Exited).await;
        launched_no_more_than(&daemon, pane.id, 2).await;
        assert_eq!(pane_now(&daemon.client, pane.id).await, AgentState::Exited);
    }

    #[tokio::test]
    async fn should_not_resume_a_stopped_pane_after_a_restart_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = idle_claude(&daemon, &place).await;
        stop(&daemon.client, pane.id).await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        assert_eq!(pane_now(&daemon.client, pane.id).await, AgentState::Exited);
        assert_eq!(launches(&daemon, pane.id).len(), 1);
    }

    /// Quit with Stop the agents, then a daemon restart inside the kill grace (issue #321):
    /// the agent hasn't exited yet when the daemon stops, and still mustn't come back.
    #[tokio::test]
    async fn should_not_resume_a_pane_stopped_just_before_a_restart_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let script = "trap '' HUP; echo ready; while :; do sleep 0.1; done";
        let pane = (daemon.client.pane_create())
            .body(NewPane {
                argv: Some(sh(script).iter().map(|arg| (*arg).to_owned()).collect()),
                ..place.new_agent(Agent::Other)
            })
            .send()
            .await
            .expect("launch an other agent")
            .into_inner();
        printed(&daemon.client, pane.id, "ready").await;
        assert_eq!(stop(&daemon.client, pane.id).await, 204);
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        let row = super::pane(&daemon.client, pane.id).await;
        assert_eq!(
            (row.kind, row.agent, row.state),
            (PaneKind::Agent, Some(Agent::Other), AgentState::Exited)
        );
    }

    #[tokio::test]
    async fn should_refuse_a_pane_whose_process_has_exited_409() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = idle_claude(&daemon, &place).await;
        submit_now(&daemon.client, pane.id, "/exit").await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;

        assert_eq!(
            refused(&daemon.client, pane.id).await,
            (409, Some("exited".into()))
        );
    }

    #[tokio::test]
    async fn should_refuse_a_shell_hosting_no_agent_and_leave_it_running_409() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let shell = place.spawn(&daemon.client, &["sh"]).await;
        printed(&daemon.client, shell.id, "$ ").await;

        assert_eq!(
            refused(&daemon.client, shell.id).await,
            (409, Some("no_agent".into()))
        );
        type_now(&daemon.client, shell.id, "echo still-$((40+2))\r").await;
        printed(&daemon.client, shell.id, "still-42").await;
    }

    #[tokio::test]
    async fn should_answer_404_for_an_unknown_pane_404() {
        let daemon = TestDaemon::start().await;

        let (status, code) = refused(&daemon.client, uuid::Uuid::now_v7().into()).await;

        assert_eq!((status, code.as_deref()), (404, Some("not_found")));
    }

    async fn pane_now(client: &Client, id: PaneId) -> AgentState {
        pane(client, id).await.state
    }
}
