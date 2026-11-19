//! Agent panes (spec §6, §9): launching `claude` and `codex` with a session and a launch
//! prompt, delivering the user's task comments to a live agent, and resuming panes after a
//! restart. The
//! agents are the stubs in `support::agents`; no test runs a real agent CLI.

use std::time::Duration;

use agentdeck_client::Client;
use agentdeck_client::types::{
    Comment, InputWhen, Instruction, NewComment, NewInstruction, NewTask, Pane, PaneInput,
    PanePatch, Task,
};
use agentdeck_core::{Agent, AgentState, CommentId, PaneId, PaneKind, TaskId, WorkspaceId};

use crate::support::agents::{
    codex_session, launches, task_brief, task_header, task_var, trust_answer, typed, with_mcp,
};
use crate::support::terminal::{
    Place, daemon_with_shell_rules, eventually, holds_for, in_state, printed, sh,
};
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

/// Asserts `launch` starts claude afresh, on a session other than `old`, with `task`'s launch
/// prompt; returns its session.
fn fresh_claude(launch: &[String], old: &str, task: &Task) -> String {
    let session = launch
        .iter()
        .skip_while(|arg| *arg != "--session-id")
        .nth(1)
        .unwrap_or_else(|| panic!("a fresh start on a session: {launch:?}"))
        .clone();
    assert_ne!(session, old, "a new session");
    assert!(!launch.contains(&"--resume".to_owned()), "{launch:?}");
    let prompt = launch.last().expect("a launch prompt");
    assert!(prompt.starts_with(&task_header(task)), "{prompt}");
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
                .body(place.new_agent(Agent::Claude, None))
                .send()
                .await
                .expect("launch claude");

            assert_eq!(response.status().as_u16(), 201);
            let pane = response.into_inner();
            let session = pane.session_id.clone().expect("a session id");
            assert!(uuid::Uuid::parse_str(&session).is_ok(), "{session}");
            assert_eq!(pane.kind, PaneKind::Agent);
            let args = with_mcp(Agent::Claude, &["--session-id", &session]);
            assert_eq!(pane.argv[0], "claude");
            assert_eq!(pane.argv[1..], args);
            assert_eq!(launched(&daemon, pane.id, 1).await, [args]);
            assert_eq!(task_var(&daemon, pane.id).as_deref(), Some("unset"));
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
            let prompt = format!(
                "{}\n\nIt fails on Safari.\n\n\
                 Instructions for this workspace:\n  - Use the staging DB\n  \
                 - Push only after 20 green runs\n\
                 Comments since your last session:\n  - Repro in issue 7",
                task_header(&task)
            );
            let args = with_mcp(Agent::Claude, &["--session-id", &session, &prompt]);
            assert_eq!(pane.argv[1..], args);
            assert_eq!(launched(&daemon, pane.id, 1).await, [args]);
            assert_eq!(task_var(&daemon, pane.id), Some(task.id.to_string()));
            delivered_to(&daemon.client, task.id, from_you.id, pane.id).await;
        }

        #[tokio::test]
        async fn should_let_the_agent_comment_on_its_task_through_its_mcp_server_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
            let report_back = std::path::Path::new(&place.workspace.dir).join("report-back");
            std::fs::write(&report_back, "").expect("asking the stub to report back");

            let pane = place
                .launch(&daemon.client, Agent::Claude, Some(task.id))
                .await;

            let reported = eventually("the agent's comment on its task", || async {
                daemon
                    .client
                    .comment_list()
                    .id(task.id)
                    .send()
                    .await
                    .expect("list comments")
                    .into_inner()
                    .into_iter()
                    .find(|comment| comment.body == "reported back")
            })
            .await;
            assert_eq!(reported.author_agent, Some(Agent::Claude));
            assert_eq!(reported.author_pane, Some(pane.id));
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
                second.argv.last().unwrap(),
                &format!(
                    "{}\n\nInstructions for this workspace:\n  - Use the staging DB",
                    task_header(&task)
                )
            );
        }

        #[tokio::test]
        async fn should_start_without_a_prompt_with_no_task_or_instructions_201() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;

            let pane = place.launch(&daemon.client, Agent::Claude, None).await;

            assert_eq!(
                pane.argv.last().map(String::as_str),
                pane.session_id.as_deref(),
                "no launch prompt: {:?}",
                pane.argv
            );
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

            let args = with_mcp(Agent::Codex, &[&task_header(&task)]);
            assert_eq!(pane.argv[0], "codex");
            assert_eq!(pane.argv[1..], args);
            assert_eq!(launched(&daemon, pane.id, 1).await, [args]);
            let task_id = task.id.to_string();
            assert_eq!(task_var(&daemon, pane.id), Some(task_id));
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

/// Launches a claude pane, on `task` when given, held at the folder trust dialog, so what is
/// typed for it waits; then trusts the folder again, so later panes start idle.
async fn held_agent(daemon: &TestDaemon, place: &Place, task: Option<TaskId>) -> Pane {
    let untrusted = std::path::Path::new(&place.workspace.dir).join("untrusted");
    std::fs::write(&untrusted, "").expect("marking the folder untrusted");
    let pane = place.launch(&daemon.client, Agent::Claude, task).await;
    in_state(&daemon.client, pane.id, AgentState::Blocked).await;
    std::fs::remove_file(&untrusted).expect("trusting the folder");
    pane
}

/// Submits a marker to `pane` and returns everything typed into it once the marker has
/// reached its stub. A comment is marked delivered once its bytes are queued for the
/// pane's writer, before the stub has read them; the writer types in order, so anything
/// delivered before the marker was typed has arrived ahead of it.
async fn typed_through_marker(daemon: &TestDaemon, pane: PaneId) -> String {
    submit_now(&daemon.client, pane, "marker").await;
    eventually("the marker to be typed", || async {
        let typed = typed(daemon, pane);
        typed.ends_with("marker\n").then_some(typed)
    })
    .await
}

/// Answers `pane`'s trust dialog and returns everything typed into it once it is idle.
/// What was waiting for its prompt is typed as it idles, ahead of the marker.
async fn typed_once_released(daemon: &TestDaemon, pane: PaneId) -> String {
    type_now(&daemon.client, pane, "\r").await;
    in_state(&daemon.client, pane, AgentState::Idle).await;
    typed_through_marker(daemon, pane).await
}

mod comment_delivery {
    use super::*;

    #[tokio::test]
    async fn should_type_a_waiting_comment_only_into_a_newer_agent_linked_meanwhile() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let older = held_agent(&daemon, &place, Some(task.id)).await;
        let waiting = comment(&daemon.client, &task, "Also check Firefox", None).await;
        let newer = place.launch(&daemon.client, Agent::Claude, None).await;
        in_state(&daemon.client, newer.id, AgentState::Idle).await;

        link(&daemon.client, newer.id, Some(task.id)).await;

        delivered_to(&daemon.client, task.id, waiting.id, newer.id).await;
        assert_eq!(
            typed_through_marker(&daemon, newer.id).await,
            format!("{}Also check Firefox\nmarker\n", task_brief(&task))
        );
        assert_eq!(typed_once_released(&daemon, older.id).await, "marker\n");
        let comment = read_comment(&daemon.client, task.id, waiting.id).await;
        assert_eq!(comment.delivered_to_pane, Some(newer.id));
    }

    #[tokio::test]
    async fn should_keep_a_waiting_comment_for_its_newer_agent_when_an_older_one_is_linked() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let older = place.launch(&daemon.client, Agent::Claude, None).await;
        in_state(&daemon.client, older.id, AgentState::Idle).await;
        let newer = held_agent(&daemon, &place, Some(task.id)).await;
        let waiting = comment(&daemon.client, &task, "Also check Firefox", None).await;

        link(&daemon.client, older.id, Some(task.id)).await;

        // The link types at once into an idle pane, so anything it typed precedes the marker.
        assert_eq!(
            typed_through_marker(&daemon, older.id).await,
            format!("{}marker\n", task_brief(&task))
        );
        assert_eq!(
            typed_once_released(&daemon, newer.id).await,
            "Also check Firefox\nmarker\n"
        );
        delivered_to(&daemon.client, task.id, waiting.id, newer.id).await;
    }

    #[tokio::test]
    async fn should_not_type_a_waiting_comment_a_new_agents_launch_prompt_carried() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let older = held_agent(&daemon, &place, Some(task.id)).await;
        let waiting = comment(&daemon.client, &task, "Also check Firefox", None).await;

        let newer = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;

        let prompt = launched(&daemon, newer.id, 1).await[0].join(" ");
        assert!(prompt.contains("Also check Firefox"), "{prompt}");
        delivered_to(&daemon.client, task.id, waiting.id, newer.id).await;
        assert_eq!(typed_once_released(&daemon, older.id).await, "marker\n");
    }

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
        // Anything held for the older pane is typed as it idles, ahead of its marker.
        in_state(&daemon.client, older.id, AgentState::Idle).await;
        assert_eq!(typed_through_marker(&daemon, older.id).await, "marker\n");
    }

    #[tokio::test]
    async fn should_go_only_to_the_newest_of_two_agents_linked_after_they_started() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let older = place.launch(&daemon.client, Agent::Claude, None).await;
        let newer = place.launch(&daemon.client, Agent::Claude, None).await;
        for pane in [older.id, newer.id] {
            in_state(&daemon.client, pane, AgentState::Idle).await;
            link(&daemon.client, pane, Some(task.id)).await;
        }

        let created = comment(&daemon.client, &task, "Also check Firefox", None).await;

        delivered_to(&daemon.client, task.id, created.id, newer.id).await;
        assert_eq!(
            typed_through_marker(&daemon, newer.id).await,
            format!("{}Also check Firefox\nmarker\n", task_brief(&task))
        );
        assert_eq!(
            typed_through_marker(&daemon, older.id).await,
            format!("{}marker\n", task_brief(&task))
        );
    }

    #[tokio::test]
    async fn should_type_the_task_then_its_waiting_comments_into_an_agent_linked_to_it() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(
            &daemon.client,
            place.workspace.id,
            "Fix it",
            "It fails on CI",
        )
        .await;
        let first = comment(&daemon.client, &task, "Also check Firefox", None).await;
        let second = comment(&daemon.client, &task, "And Safari", None).await;
        let pane = place.launch(&daemon.client, Agent::Claude, None).await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        link(&daemon.client, pane.id, Some(task.id)).await;

        let expected = format!("{}Also check Firefox\nAnd Safari\n", task_brief(&task));
        assert!(expected.contains("\n\nIt fails on CI\n"), "{expected}");
        eventually("the task and its comments to be typed", || async {
            (typed(&daemon, pane.id) == expected).then_some(())
        })
        .await;
        delivered_to(&daemon.client, task.id, first.id, pane.id).await;
        delivered_to(&daemon.client, task.id, second.id, pane.id).await;
    }

    #[tokio::test]
    async fn should_type_neither_the_task_nor_its_comments_into_a_shell_linked_to_it() {
        let daemon = daemon_with_shell_rules().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let waiting = comment(&daemon.client, &task, "Also check Firefox", None).await;
        let pane = place
            .spawn(
                &daemon.client,
                &sh("printf '\\033]0;idle\\007'; cat > typed"),
            )
            .await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        link(&daemon.client, pane.id, Some(task.id)).await;
        // Long past the 100 ms the shell's state takes to settle: an agent would have had it.
        tokio::time::sleep(Duration::from_millis(1_000)).await;

        let comment = read_comment(&daemon.client, task.id, waiting.id).await;
        assert_eq!(comment.delivered_at, None);
        let typed =
            std::fs::read_to_string(std::path::Path::new(&place.workspace.dir).join("typed"));
        assert_eq!(typed.unwrap_or_default(), "");
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
        assert_eq!(
            typed_through_marker(&daemon, pane.id).await,
            "Also check Firefox\nmarker\n"
        );
    }
    #[tokio::test]
    async fn should_pass_a_held_comment_to_the_next_agent_when_its_pane_is_linked_elsewhere() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task_a = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let task_b = task(&daemon.client, place.workspace.id, "Ship it", "").await;
        let older = place
            .launch(&daemon.client, Agent::Claude, Some(task_a.id))
            .await;
        in_state(&daemon.client, older.id, AgentState::Idle).await;
        let newer = held_agent(&daemon, &place, Some(task_a.id)).await;
        let waiting = comment(&daemon.client, &task_a, "Also check Firefox", None).await;

        link(&daemon.client, newer.id, Some(task_b.id)).await;

        delivered_to(&daemon.client, task_a.id, waiting.id, older.id).await;
        assert_eq!(
            typed_once_released(&daemon, newer.id).await,
            format!("{}marker\n", task_brief(&task_b)),
            "only the task it is linked to now"
        );
    }

    #[tokio::test]
    async fn should_pass_a_held_comment_to_the_next_agent_when_its_pane_exits() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let older = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        in_state(&daemon.client, older.id, AgentState::Idle).await;
        let newer = held_agent(&daemon, &place, Some(task.id)).await;
        let waiting = comment(&daemon.client, &task, "Also check Firefox", None).await;

        // Ctrl+C at the stand-in trust dialog ends the stub.
        type_now(&daemon.client, newer.id, "\u{3}").await;

        in_state(&daemon.client, newer.id, AgentState::Exited).await;
        delivered_to(&daemon.client, task.id, waiting.id, older.id).await;
    }

    #[tokio::test]
    async fn should_leave_a_held_comment_waiting_on_the_task_when_no_other_agent_is_live() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let held = held_agent(&daemon, &place, Some(task.id)).await;
        let waiting = comment(&daemon.client, &task, "Also check Firefox", None).await;
        type_now(&daemon.client, held.id, "\u{3}").await;
        in_state(&daemon.client, held.id, AgentState::Exited).await;

        let next = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;

        let prompt = launched(&daemon, next.id, 1).await[0].join(" ");
        assert!(prompt.contains("Also check Firefox"), "{prompt}");
        delivered_to(&daemon.client, task.id, waiting.id, next.id).await;
    }
}

mod task_delivery {
    use super::*;

    #[tokio::test]
    async fn should_type_the_task_into_an_agent_linked_to_it_once_it_waits_at_its_prompt() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(
            &daemon.client,
            place.workspace.id,
            "Fix it",
            "It fails on CI",
        )
        .await;
        let waiting = comment(&daemon.client, &task, "Also check Firefox", None).await;
        let pane = held_agent(&daemon, &place, None).await;

        link(&daemon.client, pane.id, Some(task.id)).await;
        // Long past the 400 ms a state takes to settle: the task is still held.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert_eq!(
            trust_answer(&daemon, pane.id),
            None,
            "the dialog was answered"
        );
        assert_eq!(typed(&daemon, pane.id), "");

        assert_eq!(
            typed_once_released(&daemon, pane.id).await,
            format!("{}Also check Firefox\nmarker\n", task_brief(&task))
        );
        delivered_to(&daemon.client, task.id, waiting.id, pane.id).await;
    }

    #[tokio::test]
    async fn should_not_type_the_task_again_when_linked_to_it_again() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = place.launch(&daemon.client, Agent::Claude, None).await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;
        link(&daemon.client, pane.id, Some(task.id)).await;
        eventually("the task to be typed", || async {
            (typed(&daemon, pane.id) == task_brief(&task)).then_some(())
        })
        .await;

        link(&daemon.client, pane.id, Some(task.id)).await;

        assert_eq!(
            typed_through_marker(&daemon, pane.id).await,
            format!("{}marker\n", task_brief(&task))
        );
    }

    #[tokio::test]
    async fn should_not_type_the_task_its_launch_prompt_carried_when_linked_to_it() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        link(&daemon.client, pane.id, Some(task.id)).await;

        assert_eq!(typed_through_marker(&daemon, pane.id).await, "marker\n");
    }

    #[tokio::test]
    async fn should_type_nothing_on_unlinking() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let pane = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        link(&daemon.client, pane.id, None).await;

        assert_eq!(typed_through_marker(&daemon, pane.id).await, "marker\n");
    }

    #[tokio::test]
    async fn should_type_the_task_an_agent_is_linked_to_instead_of_the_one_it_was_launched_on() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let first = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let second = task(&daemon.client, place.workspace.id, "Ship it", "Tag v2").await;
        let pane = place
            .launch(&daemon.client, Agent::Claude, Some(first.id))
            .await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        link(&daemon.client, pane.id, Some(second.id)).await;

        assert_eq!(
            typed_through_marker(&daemon, pane.id).await,
            format!("{}marker\n", task_brief(&second))
        );
    }
    #[tokio::test]
    async fn should_type_only_the_task_an_agent_is_linked_to_when_relinked_before_its_prompt() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let first = task(&daemon.client, place.workspace.id, "Fix it", "").await;
        let second = task(&daemon.client, place.workspace.id, "Ship it", "Tag v2").await;
        let waiting = comment(&daemon.client, &first, "Also check Firefox", None).await;
        let pane = held_agent(&daemon, &place, None).await;
        link(&daemon.client, pane.id, Some(first.id)).await;

        link(&daemon.client, pane.id, Some(second.id)).await;

        assert_eq!(
            typed_once_released(&daemon, pane.id).await,
            format!("{}marker\n", task_brief(&second))
        );
        let comment = read_comment(&daemon.client, first.id, waiting.id).await;
        assert_eq!(
            (comment.delivered_at, comment.delivered_to_pane),
            (None, None)
        );
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
        assert_eq!(
            claude_launches[1],
            with_mcp(Agent::Claude, &["--resume", &claude_session])
        );
        let codex_launches = launched(&daemon, codex.id, 2).await;
        assert_eq!(
            codex_launches[1],
            with_mcp(Agent::Codex, &["resume", &codex_session])
        );
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
        let claude = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
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
        let claude = place
            .launch(&daemon.client, Agent::Claude, Some(task.id))
            .await;
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
        assert_eq!(launches[1], with_mcp(Agent::Claude, &["--resume", &old]));
        let session = fresh_claude(&launches[2], &old, &task);
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
                task_id: Some(task.id),
                ..place.new_pane(&[])
            })
            .send()
            .await
            .expect("start an other agent")
            .into_inner();
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
        let waiting = comment(&daemon.client, &task, "Also check Firefox", None).await;
        type_now(&daemon.client, pane.id, "claude\r").await;
        delivered_to(&daemon.client, task.id, waiting.id, pane.id).await;
        assert_eq!(
            pane_now(&daemon.client, pane.id).await.hosted_agent,
            Some(Agent::Claude)
        );
    }

    async fn pane_now(client: &Client, id: PaneId) -> Pane {
        pane(client, id).await
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

    /// Launches an agent of `agent` on `task` and has it exit at its prompt.
    async fn exited_agent(daemon: &TestDaemon, place: &Place, agent: Agent, task: &Task) -> Pane {
        let pane = place.launch(&daemon.client, agent, Some(task.id)).await;
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
            with_mcp(Agent::Claude, &["--resume", &session])
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
            assert_eq!(launches[1], with_mcp(Agent::Claude, &["--resume", &old]));
            let session = fresh_claude(&launches[2], &old, &task);
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
            let session = fresh_claude(&launches[2], &old, &task);
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
            let pane = place
                .launch(&daemon.client, Agent::Codex, Some(task.id))
                .await;
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
            assert_eq!(launches[1], with_mcp(Agent::Codex, &["resume", &old]));
            assert!(!launches[2].contains(&"resume".to_owned()), "{launches:?}");
            let prompt = launches[2].last().expect("a launch prompt");
            assert!(prompt.starts_with(&task_header(&task)), "{prompt}");
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
        assert!(!again.contains(&"resume".to_owned()), "{again:?}");
        let prompt = again.last().expect("a launch prompt");
        assert!(prompt.starts_with(&task_header(&task)), "{prompt}");
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
        let pane = place.launch(&daemon.client, Agent::Claude, None).await;

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
        let pane = place.launch(&daemon.client, Agent::Claude, None).await;
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
                ..place.new_agent(Agent::Other, None)
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
            with_mcp(Agent::Claude, &["--resume", &session])
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
