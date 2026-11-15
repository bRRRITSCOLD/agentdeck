//! `agentdeck_client::instructions`: the instruction operations that take more than one
//! step, against a real daemon.

use agentdeck_client::instructions::send_instruction;
use agentdeck_client::types::NewInstruction;
use agentdeck_core::{Agent, AgentState};

use crate::support::TestDaemon;
use crate::support::agents::typed;
use crate::support::terminal::{Place, eventually, in_state, printed, sh};

mod send_instruction {
    use super::*;

    #[tokio::test]
    async fn should_submit_it_to_an_agent_that_reads_text_typed_with_its_enter_as_a_paste() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.launch(&daemon.client, Agent::Claude, None).await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        send_instruction(
            &daemon.client,
            "Keep the arrow keys in TabsList\nand test them\n",
            pane.id,
        )
        .await
        .expect("send the instruction");

        eventually("the agent to submit the instruction", || async {
            let typed = typed(&daemon, pane.id);
            (!typed.is_empty()).then_some(typed)
        })
        .await;
        assert_eq!(
            typed(&daemon, pane.id),
            "Keep the arrow keys in TabsList\nand test them\n"
        );
    }

    #[tokio::test]
    async fn should_type_the_instruction_with_enter_now_and_keep_it() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = (place.spawn(
            &daemon.client,
            &sh("read line; echo \"got:$line\"; sleep 60"),
        ))
        .await;
        let added = daemon
            .client
            .instruction_create()
            .id(place.workspace.id)
            .body(NewInstruction {
                body: "Keep the arrow keys in TabsList".into(),
            })
            .send()
            .await
            .expect("add an instruction")
            .into_inner();

        send_instruction(&daemon.client, &added.body, pane.id)
            .await
            .expect("send the instruction");

        printed(
            &daemon.client,
            pane.id,
            "got:Keep the arrow keys in TabsList",
        )
        .await;
        let kept = daemon
            .client
            .instruction_list()
            .id(place.workspace.id)
            .send()
            .await
            .expect("list instructions")
            .into_inner();
        assert_eq!(kept.len(), 1, "an instruction is never consumed");
    }

    #[tokio::test]
    async fn should_fail_when_the_pane_is_gone() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        (daemon.client.pane_delete().id(pane.id).send())
            .await
            .expect("delete the pane");

        let sent = send_instruction(&daemon.client, "too late", pane.id).await;

        assert!(sent.is_err());
    }
}
