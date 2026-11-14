//! Note operations that take more than one call.

use agentdeck_core::{NoteId, PaneId};

use crate::types::{InputWhen, NotePatch, PaneInput};
use crate::{ApiError, Client};

/// Sends note `note`, whose text is `body`, to `pane` at once, typed with Enter as
/// `agentdeck agent send --now` does, then marks it read by `pane` (spec §9). A note the pane
/// didn't take stays unread.
pub async fn send_note(
    client: &Client,
    note: NoteId,
    body: &str,
    pane: PaneId,
) -> Result<(), ApiError> {
    client
        .pane_input()
        .id(pane)
        .body(PaneInput {
            text: format!("{body}\r"),
            when: InputWhen::Now,
        })
        .send()
        .await
        .map_err(ApiError::from)?;
    client
        .note_update()
        .id(note)
        .body(NotePatch {
            read: Some(true),
            read_by_pane: Some(pane),
            ..NotePatch::default()
        })
        .send()
        .await
        .map_err(ApiError::from)?;
    Ok(())
}
