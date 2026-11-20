//! The terminal modes a killed program leaves on (spec §7, #322): mouse reporting, bracketed
//! paste, the alternate screen and the rest go back to a fresh terminal's once the pane's
//! process exits or the agent a shell pane hosts leaves, so the next program (the shell, or
//! the agent started again) gets no mouse reports it never asked for. Each check renders the
//! pane's channel as the GUI does ([`Screen`]).

use agentdeck_client::Client;
use agentdeck_client::types::{NewPane, Pane, PaneInput, PaneRespawn, PaneRun};
use agentdeck_core::{Agent, AgentState, PaneId};
use alacritty_terminal::term::TermMode;

use crate::support::TestDaemon;
use crate::support::terminal::{Place, eventually, in_state, printed, sh};
use crate::support::ws::{Screen, Ws};

/// What a full-screen program turns on as it starts, as Codex does: SGR mouse reports,
/// bracketed paste and the alternate screen, after a line on the main screen.
const TURN_ON: &str =
    r"printf 'on the main screen\r\n\033[?1000h\033[?1006h\033[?2004h\033[?1049hon the alternate'";

/// The modes [`TURN_ON`] sets, which a fresh terminal has off.
fn left_on() -> TermMode {
    TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE | TermMode::BRACKETED_PASTE
}

/// Whether `screen` has every mode [`TURN_ON`] sets off, and the main screen back.
fn reset(screen: &Screen) -> bool {
    !screen.modes().intersects(left_on() | TermMode::ALT_SCREEN)
}

/// An `other` agent pane running `script` under `sh`, which `POST /v1/panes/{id}/stop` kills.
async fn agent(daemon: &TestDaemon, place: &Place, script: &str) -> Pane {
    (daemon.client.pane_create())
        .body(NewPane {
            argv: Some(sh(script).iter().map(|arg| (*arg).to_owned()).collect()),
            ..place.new_agent(Agent::Other, None)
        })
        .send()
        .await
        .expect("launch an other agent")
        .into_inner()
}

async fn stop(client: &Client, pane: PaneId) {
    (client.pane_stop().id(pane).send().await).expect("stop the pane's agent");
}

/// Attaches `pane` on `channel` and reads its output until the program has turned
/// [`TURN_ON`]'s modes on.
async fn attached_with_modes_on(ws: &mut Ws, pane: PaneId, channel: u32) -> Screen {
    let mut screen = ws.attach(pane, channel).await;
    screen
        .until(ws, |screen| {
            screen.modes().contains(left_on() | TermMode::ALT_SCREEN)
        })
        .await;
    screen
}

mod exit {
    use super::*;

    #[tokio::test]
    async fn should_turn_off_the_modes_a_killed_program_left_on() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = agent(&daemon, &place, &format!("{TURN_ON}; exec sleep 600")).await;
        let mut ws = Ws::connect(&daemon).await;
        let mut screen = attached_with_modes_on(&mut ws, pane.id, 1).await;

        stop(&daemon.client, pane.id).await;

        screen.until(&mut ws, reset).await;
        assert!(screen.shows("on the main screen"), "{:?}", screen.rows());
        assert!(!screen.shows("on the alternate"), "{:?}", screen.rows());
        in_state(&daemon.client, pane.id, AgentState::Exited).await;
        let attached_after = Ws::connect(&daemon).await.attach(pane.id, 1).await;
        assert!(
            reset(&attached_after),
            "a client attaching later: {:?}",
            attached_after.modes()
        );
    }
}

mod respawn {
    use super::*;

    async fn respawn(client: &Client, pane: PaneId, run: PaneRun) {
        (client.pane_respawn().id(pane))
            .body(PaneRespawn { run })
            .send()
            .await
            .expect("respawn");
    }

    #[tokio::test]
    async fn should_start_the_new_process_with_the_modes_off() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = agent(&daemon, &place, &format!("{TURN_ON}; exec sleep 600")).await;
        let mut ws = Ws::connect(&daemon).await;
        attached_with_modes_on(&mut ws, pane.id, 1).await;
        stop(&daemon.client, pane.id).await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;

        respawn(&daemon.client, pane.id, PaneRun::Shell).await;

        printed(&daemon.client, pane.id, "$ ").await;
        let screen = Ws::connect(&daemon).await.attach(pane.id, 1).await;
        assert!(reset(&screen), "{:?}", screen.modes());
    }
}

mod hosted {
    use super::*;

    /// Types `line` and Enter at the shell's prompt.
    async fn run(client: &Client, pane: PaneId, line: &str) {
        (client.pane_input().id(pane))
            .body(PaneInput {
                text: format!("{line}\r"),
                when: agentdeck_client::types::InputWhen::Now,
                submit: false,
            })
            .send()
            .await
            .expect("type into the pane");
    }

    async fn hosting(client: &Client, pane: PaneId, agent: Option<Agent>) {
        eventually(&format!("the pane to host {agent:?}"), || async {
            let read = (client.pane_get().id(pane).send().await).expect("read the pane");
            (read.into_inner().hosted_agent == agent).then_some(())
        })
        .await;
    }

    /// An interactive `sh` pane hosting the stub `codex`, started once [`TURN_ON`] ran in the
    /// shell's own line, with a client attached on channel 1 that has seen the modes on.
    async fn codex_with_modes_on(daemon: &TestDaemon, before: &str) -> (Pane, Ws, Screen) {
        let place = Place::new(daemon).await;
        let pane = place.spawn(&daemon.client, &["sh"]).await;
        printed(&daemon.client, pane.id, "$ ").await;
        run(
            &daemon.client,
            pane.id,
            &format!("{before}{TURN_ON}; codex"),
        )
        .await;
        hosting(&daemon.client, pane.id, Some(Agent::Codex)).await;
        let mut ws = Ws::connect(daemon).await;
        let screen = attached_with_modes_on(&mut ws, pane.id, 1).await;
        (pane, ws, screen)
    }

    #[tokio::test]
    async fn should_turn_off_the_modes_once_the_hosted_agent_is_stopped() {
        let daemon = TestDaemon::start().await;
        let (pane, mut ws, mut screen) = codex_with_modes_on(&daemon, "").await;

        stop(&daemon.client, pane.id).await;

        hosting(&daemon.client, pane.id, None).await;
        screen.until(&mut ws, reset).await;
        assert!(screen.shows("on the main screen"), "{:?}", screen.rows());
    }

    #[tokio::test]
    async fn should_keep_the_modes_the_shell_turns_on_at_its_prompt_once_the_agent_left() {
        let daemon = TestDaemon::start().await;
        // A prompt that turns bracketed paste on, as bash's and zsh's line editors do. It reads
        // `prompt>`, which the line typed to set it doesn't.
        let prompt = r#"PS1="$(printf '\033[?2004h%s> ' prompt)"; "#;
        let (pane, mut ws, mut screen) = codex_with_modes_on(&daemon, prompt).await;

        stop(&daemon.client, pane.id).await;

        hosting(&daemon.client, pane.id, None).await;
        screen
            .until(&mut ws, |screen| {
                !screen
                    .modes()
                    .intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::ALT_SCREEN)
            })
            .await;
        printed(&daemon.client, pane.id, "prompt> ").await;
        let later = Ws::connect(&daemon).await.attach(pane.id, 1).await;
        assert!(
            later.modes().contains(TermMode::BRACKETED_PASTE),
            "the prompt's bracketed paste: {:?}",
            later.modes()
        );
        assert!(
            !later
                .modes()
                .intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE)
        );
    }
}
