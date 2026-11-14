//! Agent panes (spec §6, §9): launching `claude` and `codex` with a session and a launch
//! prompt, delivering the user's task comments to a live agent, and resuming panes after a
//! restart. The
//! agents are the stubs in `support::agents`; no test runs a real agent CLI.

use std::time::Duration;

use agentdeck_client::Client;
use agentdeck_client::types::{
    Comment, InputWhen, Instruction, NewComment, NewInstruction, NewTask, PaneInput, Task,
};
use agentdeck_core::{Agent, AgentState, CommentId, PaneId, PaneKind, TaskId, WorkspaceId};

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

async fn instruction(client: &Client, workspace: WorkspaceId, body: &str) -> Instruction {
    client
        .instruction_create()
        .id(workspace)
        .body(NewInstruction { body: body.into() })
        .send()
        .await
        .expect("add an instruction")
        .into_inner()
}

/// Adds a comment to `task`: the user's, or with `pane` that pane's agent's.
async fn comment(client: &Client, task: &Task, body: &str, pane: Option<PaneId>) -> Comment {
    client
        .comment_create()
        .id(task.id)
        .body(NewComment {
            body: body.into(),
            pane,
        })
        .send()
        .await
        .expect("add a comment")
        .into_inner()
}

async fn read_comment(client: &Client, task: TaskId, id: CommentId) -> Comment {
    client
        .comment_list()
        .id(task)
        .send()
        .await
        .expect("list comments")
        .into_inner()
        .into_iter()
        .find(|comment| comment.id == id)
        .expect("the comment")
}

/// Waits until comment `id` on `task` is delivered to `pane`.
async fn delivered_to(client: &Client, task: TaskId, id: CommentId, pane: PaneId) -> Comment {
    eventually(
        &format!("comment {id} to be delivered to the pane"),
        || async {
            let comment = read_comment(client, task, id).await;
            (comment.delivered_to_pane == Some(pane)).then_some(comment)
        },
    )
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
        async fn should_carry_the_task_instructions_and_new_comments_in_the_launch_prompt_201() {
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
            instruction(&daemon.client, ws, "Use the staging DB").await;
            instruction(&daemon.client, ws, "Push only after 20 green runs").await;
            let from_you = comment(&daemon.client, &task, "Repro in issue 7", None).await;
            let other = Place::new(&daemon).await;
            instruction(&daemon.client, other.workspace.id, "Not you").await;

            let pane = place
                .launch(&daemon.client, Agent::Claude, Some(task.id))
                .await;

            let session = pane.session_id.clone().expect("a session id");
            let prompt = "Task: Fix the login bug\n\nIt fails on Safari.\n\n\
                          Instructions for this workspace:\n  - Use the staging DB\n  \
                          - Push only after 20 green runs\n\
                          Comments since your last session:\n  - Repro in issue 7";
            assert_eq!(pane.argv, ["claude", "--session-id", &session, prompt]);
            assert_eq!(
                launched(&daemon, pane.id, 1).await,
                [["--session-id", session.as_str(), prompt]]
            );
            delivered_to(&daemon.client, task.id, from_you.id, pane.id).await;
        }

        #[tokio::test]
        async fn should_leave_out_delivered_and_agent_comments_but_keep_instructions_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let ws = place.workspace.id;
            let task = task(&daemon.client, ws, "Fix it", "").await;
            instruction(&daemon.client, ws, "Use the staging DB").await;
            let first = place
                .launch(&daemon.client, Agent::Claude, Some(task.id))
                .await;
            in_state(&daemon.client, first.id, AgentState::Idle).await;
            let delivered = comment(&daemon.client, &task, "For the first", None).await;
            delivered_to(&daemon.client, task.id, delivered.id, first.id).await;
            comment(&daemon.client, &task, "PR opened", Some(first.id)).await;

            let second = place
                .launch(&daemon.client, Agent::Claude, Some(task.id))
                .await;

            assert_eq!(
                second.argv[3],
                "Task: Fix it\n\nInstructions for this workspace:\n  - Use the staging DB"
            );
        }

        #[tokio::test]
        async fn should_start_without_a_prompt_with_no_task_or_instructions_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;

            let pane = place.launch(&daemon.client, Agent::Claude, None).await;

            assert_eq!(pane.argv.len(), 3, "no launch prompt: {:?}", pane.argv);
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

mod comment_delivery {
    use super::*;

    #[tokio::test]
    async fn should_type_the_users_comment_into_its_idle_agent_and_mark_it_delivered() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        let created = comment(&daemon.client, &task, "Also check Firefox", None).await;

        assert_eq!(created.delivered_at, None);
        eventually("the comment to be typed", || async {
            (typed(&daemon, pane.id) == "Also check Firefox\n").then_some(())
        })
        .await;
        let delivered = delivered_to(&daemon.client, task.id, created.id, pane.id).await;
        assert!(delivered.delivered_at.is_some());
    }

    #[tokio::test]
    async fn should_go_to_the_newest_live_agent_on_the_task() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let older = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        let newer = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        in_state(&daemon.client, newer.id, AgentState::Idle).await;

        let created = comment(&daemon.client, &task, "Also check Firefox", None).await;

        delivered_to(&daemon.client, task.id, created.id, newer.id).await;
        assert_eq!(typed(&daemon, older.id), "");
    }

    #[tokio::test]
    async fn should_never_deliver_an_agents_comment() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        let created = comment(&daemon.client, &task, "PR opened", Some(pane.id)).await;
        // Long past the 400 ms a state takes to settle: an idle agent would have had it.
        tokio::time::sleep(Duration::from_millis(1_500)).await;

        assert_eq!(typed(&daemon, pane.id), "");
        let comment = read_comment(&daemon.client, task.id, created.id).await;
        assert_eq!(
            (comment.delivered_at, comment.delivered_to_pane),
            (None, None)
        );
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

        let created = comment(&daemon.client, &task, "Also check Firefox", None).await;
        // Long past the 400 ms a state takes to settle: the comment is still held.
        tokio::time::sleep(Duration::from_millis(1_500)).await;

        assert_eq!(
            trust_answer(&daemon, pane.id),
            None,
            "the dialog was answered"
        );
        let held = read_comment(&daemon.client, task.id, created.id).await;
        assert_eq!(held.delivered_at, None);

        // The user answers it; then the agent idles and the comment goes in.
        type_now(&daemon.client, pane.id, "\r").await;
        delivered_to(&daemon.client, task.id, created.id, pane.id).await;
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
