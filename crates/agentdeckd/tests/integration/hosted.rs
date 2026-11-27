//! Agents hosted in a shell pane (spec §3, §7): `claude` or `codex` started by hand in an
//! interactive `sh` becomes the pane's foreground process, the pane hosts it while it runs,
//! and is a shell again once it exits. The agents are the stubs in `support::agents`, which
//! exit on a submitted `/exit`.

use std::time::Duration;

use agentdeck_client::Client;
use agentdeck_client::types::{InputWhen, LastHosted, Pane, PaneInput};
use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{Agent, AgentState, PaneId, PaneKind};

use crate::support::TestDaemon;
use crate::support::agents::{claude_session, codex_session, launches, typed};
use crate::support::terminal::{Place, eventually, holds_for, in_state, printed};
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

        let pane = place.launch(&daemon.client, Agent::Claude).await;

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

    #[tokio::test]
    async fn should_not_read_the_shell_as_working_once_its_agent_exits() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "claude").await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        exit_agent(&daemon.client, pane.id).await;
        hosting(&daemon.client, pane.id, None).await;

        // The agent's last output and the shell's prompt come either side of the switch;
        // neither is the shell at work. Long past a tick, a settle and the 2 s of recent output.
        holds_for(
            "the shell to stay out of working",
            Duration::from_millis(2_500),
            || async { read(&daemon.client, pane.id).await.state != AgentState::Working },
        )
        .await;
        assert_eq!(
            read(&daemon.client, pane.id).await.state,
            AgentState::Unknown
        );
    }
}

mod input {
    use super::*;

    #[tokio::test]
    async fn should_submit_input_to_a_hosted_agent_as_one_paste_then_enter() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "claude").await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        // `agent send` types a message this way (spec §4); its newline stays in it.
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

mod last_hosted {
    use super::*;

    const SESSION: &str = "5973b6c0-94b8-487b-a530-2aeb6098ae0e";

    /// Waits until `pane` reports the agent it hosted last, and returns it.
    async fn left(client: &Client, pane: PaneId) -> LastHosted {
        eventually("the pane to report its last hosted agent", || async {
            read(client, pane).await.last_hosted
        })
        .await
    }

    #[tokio::test]
    async fn should_name_the_session_its_command_line_resumed() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(
            &daemon.client,
            pane.id,
            &format!("claude --resume {SESSION}"),
        )
        .await;
        let hosted = hosting(&daemon.client, pane.id, Some(Agent::Claude)).await;
        assert_eq!(hosted.last_hosted, None);

        exit_agent(&daemon.client, pane.id).await;

        let last = left(&daemon.client, pane.id).await;
        assert_eq!(
            (last.agent, last.session_id.as_deref()),
            (Agent::Claude, Some(SESSION))
        );
    }

    #[tokio::test]
    async fn should_find_the_session_file_a_typed_claude_wrote_before_telling_clients() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "claude").await;
        hosting(&daemon.client, pane.id, Some(Agent::Claude)).await;
        let mut ws = Ws::connect(&daemon).await;

        exit_agent(&daemon.client, pane.id).await;
        ws.until(|frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::Pane,
                id,
            }) if id == pane.id.to_string() => Some(()),
            _ => None,
        })
        .await;

        let read = read(&daemon.client, pane.id).await;
        assert_eq!(read.hosted_agent, None);
        let last = read.last_hosted.expect("found before the event");
        assert_eq!(last.agent, Agent::Claude);
        assert_eq!(last.session_id, claude_session(&daemon, pane.id));
        assert!(last.session_id.is_some(), "the stub wrote one");
    }

    #[tokio::test]
    async fn should_find_the_session_file_a_typed_codex_wrote_for_its_folder() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "codex").await;
        hosting(&daemon.client, pane.id, Some(Agent::Codex)).await;

        exit_agent(&daemon.client, pane.id).await;

        let last = left(&daemon.client, pane.id).await;
        assert_eq!(last.agent, Agent::Codex);
        assert_eq!(last.session_id, codex_session(&daemon, pane.id));
        assert!(last.session_id.is_some(), "the stub wrote one");
    }

    #[tokio::test]
    async fn should_forget_it_while_the_shell_hosts_an_agent_again() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "claude").await;
        hosting(&daemon.client, pane.id, Some(Agent::Claude)).await;
        exit_agent(&daemon.client, pane.id).await;
        left(&daemon.client, pane.id).await;

        run(
            &daemon.client,
            pane.id,
            &format!("claude --resume {SESSION}"),
        )
        .await;

        let hosted = hosting(&daemon.client, pane.id, Some(Agent::Claude)).await;
        assert_eq!(hosted.last_hosted, None);
    }

    #[tokio::test]
    async fn should_report_none_for_a_shell_that_hosted_no_agent() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;

        run(&daemon.client, pane.id, "echo plain-$((40+2))").await;
        printed(&daemon.client, pane.id, "plain-42").await;

        assert_eq!(read(&daemon.client, pane.id).await.last_hosted, None);
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

mod stop {
    use super::*;

    #[tokio::test]
    async fn should_stop_the_hosted_agent_and_leave_the_shell_running_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = shell(&daemon, &place).await;
        run(&daemon.client, pane.id, "claude").await;
        hosting(&daemon.client, pane.id, Some(Agent::Claude)).await;

        let response = (daemon.client.pane_stop().id(pane.id).send().await).expect("stop");

        assert_eq!(response.status().as_u16(), 204);
        let after = hosting(&daemon.client, pane.id, None).await;
        assert_ne!(after.state, AgentState::Exited);
        // The stub leaves the terminal raw, as a killed TUI does: a newline ends the line.
        input(
            &daemon.client,
            pane.id,
            "stty sane; echo shell-$((40+2))\n",
            false,
        )
        .await;
        printed(&daemon.client, pane.id, "shell-42").await;
        assert_ne!(
            read(&daemon.client, pane.id).await.state,
            AgentState::Exited
        );
    }
}
