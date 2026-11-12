//! Agent panes (spec §6, §9): launching `claude` and `codex` with a session and a launch
//! prompt, delivering task notes to a live agent, and resuming panes after a restart. The
//! agents are the stubs in `support::agents`; no test runs a real agent CLI.

use std::time::Duration;

use agentdeck_client::Client;
use agentdeck_client::types::{InputWhen, NewNote, NewTask, Note, PaneInput, Task};
use agentdeck_core::{Agent, AgentState, NoteId, PaneId, PaneKind, WorkspaceId};

use crate::support::agents::{codex_session, launches, trust_answer, typed};
use crate::support::terminal::{Place, eventually, in_state, printed, sh};
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

async fn note(
    client: &Client,
    workspace: Option<WorkspaceId>,
    task: Option<&Task>,
    body: &str,
) -> Note {
    client
        .note_create()
        .body(NewNote {
            body: body.into(),
            workspace_id: workspace,
            task_id: task.map(|task| task.id),
        })
        .send()
        .await
        .expect("create a note")
        .into_inner()
}

async fn read_note(client: &Client, id: NoteId) -> Note {
    client
        .note_list()
        .send()
        .await
        .expect("list notes")
        .into_inner()
        .into_iter()
        .find(|note| note.id == id)
        .expect("the note")
}

/// Waits until note `id` is marked read by `pane`.
async fn read_by(client: &Client, id: NoteId, pane: PaneId) -> Note {
    eventually(&format!("note {id} to be read by the pane"), || async {
        let note = read_note(client, id).await;
        (note.read_by_pane == Some(pane)).then_some(note)
    })
    .await
}

async fn type_now(client: &Client, pane: PaneId, text: &str) {
    client
        .pane_input()
        .id(pane)
        .body(PaneInput {
            text: text.into(),
            when: InputWhen::Now,
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

async fn pane(client: &Client, id: PaneId) -> agentdeck_client::types::Pane {
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
                .body(place.new_agent(Agent::Claude, None))
                .send()
                .await
                .expect("launch claude");

            assert_eq!(response.status().as_u16(), 201);
            let pane = response.into_inner();
            let session = pane.session_id.clone().expect("a session id");
            assert!(uuid::Uuid::parse_str(&session).is_ok(), "{session}");
            assert_eq!(pane.kind, PaneKind::Agent);
            assert_eq!(pane.argv, ["claude", "--session-id", session.as_str()]);
            assert_eq!(
                launched(&daemon, pane.id, 1).await,
                [["--session-id", &session]]
            );
            in_state(&daemon.client, pane.id, AgentState::Idle).await;
        }

        #[tokio::test]
        async fn should_carry_the_task_and_its_unread_notes_in_the_launch_prompt_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let ws = place.workspace.id;
            let task = task(
                &daemon.client,
                ws,
                "Fix the login bug",
                "It fails on Safari.",
            )
            .await;
            let on_ws = note(&daemon.client, Some(ws), None, "Use the staging DB").await;
            let on_task = note(&daemon.client, None, Some(&task), "Repro in issue 7").await;
            let other = Place::new(&daemon).await;
            let elsewhere = note(&daemon.client, Some(other.workspace.id), None, "Not you").await;

            let pane = place
                .launch(&daemon.client, Agent::Claude, Some(task.id))
                .await;

            let session = pane.session_id.clone().expect("a session id");
            let prompt = "Task: Fix the login bug\n\nIt fails on Safari.\n\n\
                          Notes left for you:\n- Use the staging DB\n- Repro in issue 7";
            assert_eq!(pane.argv, ["claude", "--session-id", &session, prompt]);
            assert_eq!(
                launched(&daemon, pane.id, 1).await,
                [["--session-id", session.as_str(), prompt]]
            );
            read_by(&daemon.client, on_ws.id, pane.id).await;
            read_by(&daemon.client, on_task.id, pane.id).await;
            assert_eq!(read_note(&daemon.client, elsewhere.id).await.read_at, None);
        }

        #[tokio::test]
        async fn should_leave_out_notes_already_read_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let ws = place.workspace.id;
            let first = place.launch(&daemon.client, Agent::Claude, None).await;
            let delivered = note(&daemon.client, Some(ws), None, "For the first").await;
            // A workspace note waits for the next launch: the live pane doesn't get it.
            let second = place.launch(&daemon.client, Agent::Claude, None).await;
            read_by(&daemon.client, delivered.id, second.id).await;

            let third = place.launch(&daemon.client, Agent::Claude, None).await;

            assert_eq!(third.argv.len(), 3, "no launch prompt: {:?}", third.argv);
            assert!(typed(&daemon, first.id).is_empty());
        }

        #[tokio::test]
        async fn should_reject_an_argv_400() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let mut new = place.new_agent(Agent::Claude, None);
            new.argv = Some(vec!["claude".into(), "--model".into(), "opus".into()]);

            let (status, body) = error_response(daemon.client.pane_create().body(new).send().await);

            assert_eq!(status, 400);
            assert_eq!(body.error_code.as_deref(), Some("invalid_argv"));
        }

        #[tokio::test]
        async fn should_reject_an_agent_pane_that_names_no_agent_400() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let mut new = place.new_agent(Agent::Claude, None);
            new.agent = None;

            let (status, body) = error_response(daemon.client.pane_create().body(new).send().await);

            assert_eq!(status, 400);
            assert_eq!(body.error_code.as_deref(), Some("unsupported_kind"));
        }

        #[tokio::test]
        async fn should_reject_an_unknown_task_400() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let new = place.new_agent(Agent::Claude, Some(uuid::Uuid::now_v7().into()));

            let (status, body) = error_response(daemon.client.pane_create().body(new).send().await);

            assert_eq!(status, 400);
            assert_eq!(body.error_code.as_deref(), Some("unknown_task"));
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
            let task = task(&daemon.client, place.workspace.id, "Port the parser", "").await;

            let pane = place
                .launch(&daemon.client, Agent::Codex, Some(task.id))
                .await;

            assert_eq!(pane.argv, ["codex", "Task: Port the parser"]);
            assert_eq!(
                launched(&daemon, pane.id, 1).await,
                [["Task: Port the parser"]]
            );
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

mod note_delivery {
    use super::*;

    #[tokio::test]
    async fn should_type_a_task_note_into_its_idle_agent_and_mark_it_read() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        let created = note(&daemon.client, None, Some(&task), "Also check Firefox").await;

        assert_eq!(created.read_at, None);
        eventually("the note to be typed", || async {
            (typed(&daemon, pane.id) == "Also check Firefox\n").then_some(())
        })
        .await;
        read_by(&daemon.client, created.id, pane.id).await;
    }

    #[tokio::test]
    async fn should_never_answer_the_trust_dialog() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        std::fs::write(
            std::path::Path::new(&place.workspace.dir).join("untrusted"),
            "",
        )
        .expect("marking the folder untrusted");
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        in_state(&daemon.client, pane.id, AgentState::Blocked).await;

        let created = note(&daemon.client, None, Some(&task), "Also check Firefox").await;
        // Long past the 400 ms a state takes to settle: the note is still held.
        tokio::time::sleep(Duration::from_millis(1_500)).await;

        assert_eq!(
            trust_answer(&daemon, pane.id),
            None,
            "the dialog was answered"
        );
        assert_eq!(read_note(&daemon.client, created.id).await.read_at, None);

        // The user answers it; then the agent idles and the note goes in.
        type_now(&daemon.client, pane.id, "\r").await;
        read_by(&daemon.client, created.id, pane.id).await;
        assert_eq!(trust_answer(&daemon, pane.id).as_deref(), Some("\n"));
        assert_eq!(typed(&daemon, pane.id), "Also check Firefox\n");
    }
}

mod resume_after_restart {
    use super::*;

    #[tokio::test]
    async fn should_resume_claude_and_codex_on_their_sessions() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let claude = place.launch(&daemon.client, Agent::Claude, None).await;
        let codex = place.launch(&daemon.client, Agent::Codex, None).await;
        let claude_session = claude.session_id.clone().expect("a claude session");
        let codex_session = eventually("the codex session", || async {
            pane(&daemon.client, codex.id).await.session_id
        })
        .await;
        in_state(&daemon.client, claude.id, AgentState::Idle).await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        let claude_launches = launched(&daemon, claude.id, 2).await;
        assert_eq!(claude_launches[1], ["--resume", claude_session.as_str()]);
        let codex_launches = launched(&daemon, codex.id, 2).await;
        assert_eq!(codex_launches[1], ["resume", codex_session.as_str()]);
        let resumed = in_state(&daemon.client, claude.id, AgentState::Idle).await;
        assert_eq!(resumed.session_id.as_deref(), Some(claude_session.as_str()));
        assert_eq!(resumed.argv, claude.argv, "argv stays as first launched");
        type_now(&daemon.client, claude.id, "still here\r").await;
        eventually("input to reach the resumed claude", || async {
            typed(&daemon, claude.id)
                .contains("still here")
                .then_some(())
        })
        .await;
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
                })
                .send()
                .await,
        );
        assert_eq!(status, 409);
        assert_eq!(body.error_code.as_deref(), Some("exited"));
    }
}
