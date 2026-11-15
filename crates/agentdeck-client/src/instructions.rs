//! Instruction operations that take more than one step.

use agentdeck_core::PaneId;

use crate::types::{InputWhen, PaneInput};
use crate::{ApiError, Client};

/// Submits an instruction's `body` to `pane` at once (typed, then Enter), as `agentdeck agent
/// send --now` does: the GUI's "Send to agent" (spec §9). The instruction itself is unchanged; it
/// stays for every later launch prompt.
pub async fn send_instruction(client: &Client, body: &str, pane: PaneId) -> Result<(), ApiError> {
    client
        .pane_input()
        .id(pane)
        .body(PaneInput {
            text: body.trim_end().to_owned(),
            when: InputWhen::Now,
            submit: true,
        })
        .send()
        .await
        .map_err(ApiError::from)?;
    Ok(())
}
