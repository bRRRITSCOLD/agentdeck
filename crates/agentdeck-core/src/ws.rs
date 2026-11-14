//! The WebSocket protocol on `/v1/ws` (spec §5), shared by the daemon and its clients.
//!
//! - **Binary frames** carry terminal bytes behind a channel id: [`frame`] builds one,
//!   [`parse_frame`] splits one. Client → daemon is PTY input; daemon → client is a snapshot
//!   or live output.
//! - **Text frames** are JSON tagged with `t`: [`ClientMessage`] one way, [`ServerMessage`]
//!   the other.
//!
//! A channel is a number the client picks when it attaches to a pane; every message about
//! that attachment carries it.

use serde::{Deserialize, Serialize};

use crate::id::PaneId;
use crate::state::AgentState;

/// A client's number for one attachment, chosen in [`ClientMessage::Attach`].
pub type Channel = u32;

/// The bytes of a binary frame: the channel as a big-endian `u32`, then `bytes`.
pub fn frame(channel: Channel, bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&channel.to_be_bytes());
    frame.extend_from_slice(bytes);
    frame
}

/// Splits a binary frame into its channel and bytes; `None` when it is shorter than the
/// 4-byte channel id.
pub fn parse_frame(frame: &[u8]) -> Option<(Channel, &[u8])> {
    let (channel, bytes) = frame.split_first_chunk::<4>()?;
    Some((Channel::from_be_bytes(*channel), bytes))
}

/// A text frame from a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Attach `pane` on `channel`: the daemon answers [`ServerMessage::Attached`], one binary
    /// frame holding the snapshot, then live output. Attaching a channel that is already
    /// attached detaches it first.
    Attach {
        /// The pane to attach.
        pane: PaneId,
        /// The client's number for this attachment.
        channel: Channel,
    },
    /// Stop receiving output on `channel`. The pane keeps running.
    Detach {
        /// The attachment to end.
        channel: Channel,
    },
    /// Resize the pane on `channel`. Only the pane's primary client is obeyed; any other
    /// gets [`ServerMessage::Resize`] back with the size the pane keeps.
    Resize {
        /// The attachment.
        channel: Channel,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Make this client the pane's primary (the focused window). The last size it asked for,
    /// if any, is applied.
    Primary {
        /// The attachment.
        channel: Channel,
    },
}

/// A text frame from the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The attachment is set up; the next binary frame on `channel` is the snapshot, which
    /// the client feeds into a fresh `Term` of `cols` × `rows`.
    Attached {
        /// The attachment.
        channel: Channel,
        /// The attached pane.
        pane: PaneId,
        /// The pane's columns.
        cols: u16,
        /// The pane's rows.
        rows: u16,
        /// The pane's title.
        title: String,
        /// The pane's state.
        state: AgentState,
    },
    /// The pane on `channel` is now `cols` × `rows`; bytes after this were written at that
    /// size.
    Resize {
        /// The attachment.
        channel: Channel,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// The pane on `channel` set its title.
    Title {
        /// The attachment.
        channel: Channel,
        /// The new title.
        title: String,
    },
    /// A pane's state changed. Sent to every client, attached or not.
    State {
        /// The pane.
        pane: PaneId,
        /// Its new state.
        state: AgentState,
    },
    /// A pane's process exited. Sent to every client, attached or not.
    Exit {
        /// The pane.
        pane: PaneId,
        /// The exit code; `None` when it couldn't be read.
        code: Option<i64>,
    },
    /// An entity changed: refetch it over REST. Sent to every client.
    Changed {
        /// Which kind of entity.
        entity: Entity,
        /// Its id.
        id: String,
    },
    /// The client fell more than 1 MiB behind on `channel`, which gets no more output. Attach
    /// again for a fresh snapshot.
    Lagged {
        /// The attachment.
        channel: Channel,
    },
    /// A client message couldn't be acted on.
    Error {
        /// The attachment it was about, if any.
        channel: Option<Channel>,
        /// Machine-readable: `not_found` (no such pane), `not_attached` (no such channel),
        /// `invalid_message` (not a message this protocol knows).
        code: String,
        /// What went wrong.
        message: String,
    },
}

/// The kinds of entity a [`ServerMessage::Changed`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entity {
    /// `/v1/workspaces/{id}`.
    Workspace,
    /// `/v1/tabs/{id}`.
    Tab,
    /// `/v1/panes/{id}`.
    Pane,
    /// `/v1/tasks/{id}`.
    Task,
    /// A task comment: `/v1/tasks/{task}/comments`.
    Comment,
    /// A workspace instruction: `/v1/workspaces/{workspace}/instructions`.
    Instruction,
    /// `/v1/prs/status`: a PR sync began, read a page or ended, or the age cutoff changed.
    /// Its id is always `status`.
    PrSync,
    /// `/v1/settings`: a setting changed. Its id is always `settings`.
    Settings,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn pane() -> PaneId {
        uuid::Uuid::from_u128(7).into()
    }

    mod frame {
        use super::*;

        #[test]
        fn should_prefix_the_bytes_with_the_big_endian_channel() {
            assert_eq!(frame(3, b"hi"), [0, 0, 0, 3, b'h', b'i']);
        }

        #[test]
        fn should_split_back_into_channel_and_bytes() {
            let bytes = frame(0x0102_0304, b"\x1b[0m");
            assert_eq!(
                parse_frame(&bytes),
                Some((0x0102_0304, b"\x1b[0m".as_slice()))
            );
        }

        #[test]
        fn should_allow_an_empty_payload() {
            assert_eq!(parse_frame(&[0, 0, 0, 9]), Some((9, [].as_slice())));
        }

        #[test]
        fn should_reject_a_frame_shorter_than_the_channel_id() {
            assert_eq!(parse_frame(&[0, 0, 1]), None);
        }
    }

    mod client_message {
        use super::*;

        #[test]
        fn should_read_the_spec_shapes() {
            let cases = [
                (
                    json!({"t": "attach", "pane": pane(), "channel": 3}),
                    ClientMessage::Attach {
                        pane: pane(),
                        channel: 3,
                    },
                ),
                (
                    json!({"t": "detach", "channel": 3}),
                    ClientMessage::Detach { channel: 3 },
                ),
                (
                    json!({"t": "resize", "channel": 3, "cols": 120, "rows": 40}),
                    ClientMessage::Resize {
                        channel: 3,
                        cols: 120,
                        rows: 40,
                    },
                ),
                (
                    json!({"t": "primary", "channel": 3}),
                    ClientMessage::Primary { channel: 3 },
                ),
            ];
            for (json, message) in cases {
                assert_eq!(
                    serde_json::from_value::<ClientMessage>(json).unwrap(),
                    message
                );
            }
        }

        #[test]
        fn should_reject_an_unknown_tag() {
            assert!(serde_json::from_value::<ClientMessage>(json!({"t": "launch"})).is_err());
        }
    }

    mod server_message {
        use super::*;

        #[test]
        fn should_write_the_spec_shapes() {
            let cases = [
                (
                    ServerMessage::Attached {
                        channel: 3,
                        pane: pane(),
                        cols: 100,
                        rows: 30,
                        title: "✳ Claude Code".into(),
                        state: AgentState::Idle,
                    },
                    json!({"t": "attached", "channel": 3, "pane": pane(), "cols": 100,
                           "rows": 30, "title": "✳ Claude Code", "state": "idle"}),
                ),
                (
                    ServerMessage::Resize {
                        channel: 3,
                        cols: 120,
                        rows: 40,
                    },
                    json!({"t": "resize", "channel": 3, "cols": 120, "rows": 40}),
                ),
                (
                    ServerMessage::Title {
                        channel: 3,
                        title: "◐ Claude Code".into(),
                    },
                    json!({"t": "title", "channel": 3, "title": "◐ Claude Code"}),
                ),
                (
                    ServerMessage::State {
                        pane: pane(),
                        state: AgentState::Working,
                    },
                    json!({"t": "state", "pane": pane(), "state": "working"}),
                ),
                (
                    ServerMessage::Exit {
                        pane: pane(),
                        code: Some(0),
                    },
                    json!({"t": "exit", "pane": pane(), "code": 0}),
                ),
                (
                    ServerMessage::Changed {
                        entity: Entity::Task,
                        id: "abc".into(),
                    },
                    json!({"t": "changed", "entity": "task", "id": "abc"}),
                ),
                (
                    ServerMessage::Changed {
                        entity: Entity::Comment,
                        id: "abc".into(),
                    },
                    json!({"t": "changed", "entity": "comment", "id": "abc"}),
                ),
                (
                    ServerMessage::Changed {
                        entity: Entity::Instruction,
                        id: "abc".into(),
                    },
                    json!({"t": "changed", "entity": "instruction", "id": "abc"}),
                ),
                (
                    ServerMessage::Changed {
                        entity: Entity::Settings,
                        id: "settings".into(),
                    },
                    json!({"t": "changed", "entity": "settings", "id": "settings"}),
                ),
                (
                    ServerMessage::Lagged { channel: 3 },
                    json!({"t": "lagged", "channel": 3}),
                ),
                (
                    ServerMessage::Error {
                        channel: Some(3),
                        code: "not_found".into(),
                        message: "no such pane".into(),
                    },
                    json!({"t": "error", "channel": 3, "code": "not_found",
                           "message": "no such pane"}),
                ),
            ];
            for (message, json) in cases {
                assert_eq!(serde_json::to_value(&message).unwrap(), json);
            }
        }
    }
}
