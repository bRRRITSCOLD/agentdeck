//! Agents hosted in a shell pane (spec §3, §7, §9): `claude` or `codex` started by hand in an
//! interactive `sh` becomes the pane's foreground process, the pane hosts it while it runs,
//! and is a shell again once it exits. The agents are the stubs in `support::agents`, which
//! exit on a submitted `/exit`.

use std::time::Duration;

use agentdeck_client::Client;
use agentdeck_client::types::{
    Comment, InputWhen, NewComment, NewTask, Pane, PaneInput, PanePatch, Task,
};
use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{Agent, AgentState, CommentAuthor, PaneId, PaneKind, TaskId};

use crate::support::TestDaemon;
use crate::support::agents::{launches, typed};
use crate::support::terminal::{Place, eventually, in_state, printed};
use crate::support::ws::{Frame, Ws};

/// An interactive `sh` pane, at its prompt.
async fn shell(daemon: &TestDaemon, place: &Place) -> Pane {
    let pane = place.spawn(&daemon.client, &["sh"]).await;
    printed(&daemon.client, pane.id, "$ ").await;
    pane
}

/// Types `text` into `pane` at once: as is, or with `submit` as a message then Enter.
async fn input(client: &Client, pane: PaneId, text: &str, submit: bool) {
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

/// Runs `program` at the shell's prompt, as the user would.
async fn run(client: &Client, pane: PaneId, program: &str) {
    input(client, pane, &format!("{program}\r"), false).await;
}

/// Submits `/exit` to the hosted agent, which ends it.
async fn exit_agent(client: &Client, pane: PaneId) {
    input(client, pane, "/exit", true).await;
}

async fn read(client: &Client, pane: PaneId) -> Pane {
    client
        .pane_get()
        .id(pane)
        .send()
        .await
        .expect("read the pane")
        .into_inner()
}

/// Waits until `pane` hosts `agent` (`None`: none) over REST.
async fn hosting(client: &Client, pane: PaneId, agent: Option<Agent>) -> Pane {
    eventually(&format!("the pane to host {agent:?}"), || async {
        let read = read(client, pane).await;
        (read.hosted_agent == agent).then_some(read)
    })
    .await
}

async fn task(client: &Client, place: &Place, title: &str) -> Task {
    client
        .task_create()
        .body(NewTask {
            title: title.into(),
            body: None,
            status: None,
            workspace_id: Some(place.workspace.id),
        })
        .send()
        .await
        .expect("create a task")
        .into_inner()
}

async fn link(client: &Client, pane: PaneId, task: TaskId) {
    client
        .pane_update()
        .id(pane)
        .body(PanePatch {
            task_id: Some(task),
        })
        .send()
        .await
        .expect("link the pane");
}

async fn comment(client: &Client, task: TaskId, body: &str, pane: Option<PaneId>) -> Comment {
    client
        .comment_create()
        .id(task)
        .body(NewComment {
            body: body.into(),
            pane,
        })
        .send()
        .await
        .expect("add a comment")
        .into_inner()
}

async fn read_comment(client: &Client, task: TaskId, comment: &Comment) -> Comment {
    (client.comment_list().id(task).send().await)
        .expect("list comments")
        .into_inner()
        .into_iter()
        .find(|c| c.id == comment.id)
        .expect("the comment")
}

/// Waits until the stub agents in `pane` have submitted exactly `prompts`.
async fn submitted(daemon: &TestDaemon, pane: PaneId, prompts: &str) {
    eventually(&format!("{prompts:?} to be submitted"), || async {
        (typed(daemon, pane) == prompts).then_some(())
    })
    .await;
}

mod detection {
    use super::*;

    #[tokio::test]
    async fn should_host_claude_started_in_a_shell_until_it_exits() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        assert_eq!(pane.hosted_agent, None);

        run(&daemon.client, pane.id, "claude").await;
        let hosted = hosting(&daemon.client, pane.id, Some(Agent::Claude)).await;
        assert_eq!(hosted.kind, PaneKind::Shell, "the pane stays a shell");
        assert_eq!(hosted.agent, None);

        exit_agent(&daemon.client, pane.id).await;
        hosting(&daemon.client, pane.id, None).await;
        run(&daemon.client, pane.id, "echo back-$((40+2))").await;
        printed(&daemon.client, pane.id, "back-42").await;
    }

    #[tokio::test]
    async fn should_host_codex_started_in_a_shell() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;

        run(&daemon.client, pane.id, "codex").await;

        hosting(&daemon.client, pane.id, Some(Agent::Codex)).await;
        exit_agent(&daemon.client, pane.id).await;
        hosting(&daemon.client, pane.id, None).await;
    }

    #[tokio::test]
    async fn should_tell_clients_each_time_the_hosted_agent_changes() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        let mut ws = Ws::connect(&daemon).await;
        let changed = |frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::Pane,
                id,
            }) if id == pane.id.to_string() => Some(()),
            _ => None,
        };

        run(&daemon.client, pane.id, "claude").await;
        ws.until(changed).await;
        assert_eq!(
            read(&daemon.client, pane.id).await.hosted_agent,
            Some(Agent::Claude),
            "saved before the event"
        );

        exit_agent(&daemon.client, pane.id).await;
        ws.until(changed).await;
        assert_eq!(read(&daemon.client, pane.id).await.hosted_agent, None);
    }

    #[tokio::test]
    async fn should_not_host_an_agent_in_an_agent_pane() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;

        let pane = place.launch(&daemon.client, Agent::Claude, None).await;

        let idle = in_state(&daemon.client, pane.id, AgentState::Idle).await;
        assert_eq!(idle.hosted_agent, None);
    }
}

mod state_rules {
    use super::*;

    #[tokio::test]
    async fn should_run_the_agents_rules_while_it_is_hosted_then_the_shells() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;

        // Claude Code's `✳` title reads idle under its rules; a shell has no rule for it.
        run(&daemon.client, pane.id, "claude").await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        exit_agent(&daemon.client, pane.id).await;
        hosting(&daemon.client, pane.id, None).await;
        // Under the shell's rules, a quiet shell is unknown, never idle or done.
        in_state(&daemon.client, pane.id, AgentState::Unknown).await;
    }
}

mod comment_delivery {
    use super::*;

    #[tokio::test]
    async fn should_type_the_tasks_waiting_comments_into_an_agent_once_it_is_hosted() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, &place, "Fix it").await;
        let pane = shell(&daemon, &place).await;
        link(&daemon.client, pane.id, task.id).await;
        let waiting = comment(&daemon.client, task.id, "Also check Firefox", None).await;

        run(&daemon.client, pane.id, "claude").await;

        submitted(&daemon, pane.id, "Also check Firefox\n").await;
        let delivered = read_comment(&daemon.client, task.id, &waiting).await;
        assert_eq!(delivered.delivered_to_pane, Some(pane.id));
        assert_eq!(
            launches(&daemon, pane.id),
            [Vec::<String>::new()],
            "started by hand, with no launch prompt"
        );
    }

    #[tokio::test]
    async fn should_type_a_new_comment_into_the_hosted_agent() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, &place, "Fix it").await;
        let pane = shell(&daemon, &place).await;
        link(&daemon.client, pane.id, task.id).await;
        run(&daemon.client, pane.id, "claude").await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        let new = comment(&daemon.client, task.id, "Then open the PR", None).await;

        submitted(&daemon, pane.id, "Then open the PR\n").await;
        let delivered = read_comment(&daemon.client, task.id, &new).await;
        assert_eq!(delivered.delivered_to_pane, Some(pane.id));
    }

    #[tokio::test]
    async fn should_hold_a_comment_while_no_agent_is_hosted_until_one_is_again() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, &place, "Fix it").await;
        let pane = shell(&daemon, &place).await;
        link(&daemon.client, pane.id, task.id).await;
        run(&daemon.client, pane.id, "claude").await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;
        exit_agent(&daemon.client, pane.id).await;
        hosting(&daemon.client, pane.id, None).await;

        let held = comment(&daemon.client, task.id, "echo typed-into-the-shell", None).await;
        // Long past the 400 ms an agent's state takes to settle: a hosted agent would have it.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        let unread = read_comment(&daemon.client, task.id, &held).await;
        assert_eq!(
            unread.delivered_at, None,
            "the link stays, but delivers nothing"
        );
        let output = String::from_utf8_lossy(
            &crate::support::terminal::tail(&daemon.client, pane.id, None).await,
        )
        .into_owned();
        assert!(!output.contains("typed-into-the-shell"), "{output}");

        run(&daemon.client, pane.id, "claude").await;

        submitted(&daemon, pane.id, "echo typed-into-the-shell\n").await;
        let delivered = read_comment(&daemon.client, task.id, &held).await;
        assert_eq!(delivered.delivered_to_pane, Some(pane.id));
    }

    #[tokio::test]
    async fn should_submit_input_to_a_hosted_agent_as_one_paste_then_enter() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "claude").await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        // Send to agent types an instruction this way (spec §9); its newline stays in it.
        input(
            &daemon.client,
            pane.id,
            "Use the staging DB\nthen test",
            true,
        )
        .await;

        submitted(&daemon, pane.id, "Use the staging DB\nthen test\n").await;
    }
}

mod authorship {
    use super::*;

    #[tokio::test]
    async fn should_author_a_comment_from_the_pane_as_the_agent_it_hosts() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client, &place, "Fix it").await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "codex").await;
        hosting(&daemon.client, pane.id, Some(Agent::Codex)).await;

        let hosted = comment(&daemon.client, task.id, "PR opened", Some(pane.id)).await;

        assert_eq!(hosted.author, CommentAuthor::Agent);
        assert_eq!(hosted.author_agent, Some(Agent::Codex));
        assert_eq!(hosted.author_pane, Some(pane.id));

        exit_agent(&daemon.client, pane.id).await;
        hosting(&daemon.client, pane.id, None).await;
        let shell = comment(&daemon.client, task.id, "from the shell", Some(pane.id)).await;
        assert_eq!(shell.author_agent, None, "no agent once it left");
    }
}

mod resume_after_restart {
    use super::*;

    #[tokio::test]
    async fn should_bring_the_shell_back_without_its_hosted_agent() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "claude").await;
        hosting(&daemon.client, pane.id, Some(Agent::Claude)).await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        let resumed = read(&daemon.client, pane.id).await;
        assert_eq!(resumed.hosted_agent, None);
        assert_ne!(resumed.state, AgentState::Exited);
        run(&daemon.client, pane.id, "echo resumed-$((40+2))").await;
        printed(&daemon.client, pane.id, "resumed-42").await;
        assert_eq!(
            launches(&daemon, pane.id).len(),
            1,
            "claude isn't started again"
        );
        assert_eq!(read(&daemon.client, pane.id).await.hosted_agent, None);
    }
}
