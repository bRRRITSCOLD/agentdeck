//! A pane's attached clients (spec §5): who gets its output, who is behind, and whose size
//! wins.
//!
//! Each attachment is a [`Subscriber`] writing into its connection's [`Outbox`]. Pushing
//! into an outbox never blocks, so the pane does it under its own lock, which is what keeps
//! a snapshot and the live output after it in order. A [`Backlog`] counts the bytes a
//! channel has queued but its connection hasn't written yet; past [`LAG_LIMIT`] the channel
//! gets `lagged` and no more output.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agentdeck_core::ws::{Channel, ServerMessage};
use tokio::sync::mpsc;

/// How far behind a channel may fall before it gets `lagged`: 1 MiB.
pub const LAG_LIMIT: usize = 1 << 20;

/// A pane's size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

/// One WebSocket message for a connection's writer to send.
#[derive(Debug)]
pub enum Outgoing {
    /// A text frame.
    Message(ServerMessage),
    /// A binary frame of terminal bytes on `channel`. The writer calls [`Backlog::sent`] once
    /// it has written them, when there is a backlog to count against.
    Bytes {
        /// The attachment.
        channel: Channel,
        /// The bytes.
        bytes: Arc<[u8]>,
        /// The channel's backlog; `None` for bytes that don't count, like the snapshot.
        backlog: Option<Backlog>,
    },
}

/// The sending half of a connection's queue of [`Outgoing`] messages.
pub type Outbox = mpsc::UnboundedSender<Outgoing>;

/// Bytes queued on one channel and not yet written to its socket.
#[derive(Debug, Clone, Default)]
pub struct Backlog(Arc<AtomicUsize>);

impl Backlog {
    /// The writer wrote `len` bytes of this channel's output.
    pub fn sent(&self, len: usize) {
        self.0.fetch_sub(len, Ordering::Relaxed);
    }

    fn queued(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }

    fn queue(&self, len: usize) {
        self.0.fetch_add(len, Ordering::Relaxed);
    }
}

/// One attachment's id, unique across the daemon, so a connection that attaches a pane on
/// two channels has two subscribers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriberId(pub u64);

#[derive(Debug)]
struct Subscriber {
    id: SubscriberId,
    channel: Channel,
    outbox: Outbox,
    backlog: Backlog,
    /// The size this client last asked for, applied if it becomes primary.
    requested: Option<Size>,
}

impl Subscriber {
    /// Queues a message; false when the connection has gone.
    fn send(&self, message: ServerMessage) -> bool {
        self.outbox.send(Outgoing::Message(message)).is_ok()
    }
}

/// What to do with a client's resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDecision {
    /// Resize the pane.
    Apply(Size),
    /// Another client is primary: tell this one the size the pane keeps.
    Refuse,
}

/// A pane's subscribers, and which of them is primary.
#[derive(Debug, Default)]
pub struct Fanout {
    subscribers: Vec<Subscriber>,
    primary: Option<SubscriberId>,
}

impl Fanout {
    /// Adds an attachment. The caller has already queued its `attached` and snapshot.
    pub fn subscribe(&mut self, id: SubscriberId, channel: Channel, outbox: Outbox) {
        self.subscribers.push(Subscriber {
            id,
            channel,
            outbox,
            backlog: Backlog::default(),
            requested: None,
        });
    }

    /// Removes an attachment. A primary that leaves leaves the pane with none.
    pub fn unsubscribe(&mut self, id: SubscriberId) {
        self.subscribers.retain(|subscriber| subscriber.id != id);
        if self.primary == Some(id) {
            self.primary = None;
        }
    }

    /// Queues live output to every subscriber. One that would fall more than [`LAG_LIMIT`]
    /// behind gets `lagged` instead and is dropped, as is one whose connection has gone.
    pub fn output(&mut self, bytes: &[u8]) {
        let bytes: Arc<[u8]> = bytes.into();
        self.retain(|subscriber| {
            if subscriber.backlog.queued() + bytes.len() > LAG_LIMIT {
                subscriber.send(ServerMessage::Lagged {
                    channel: subscriber.channel,
                });
                return false;
            }
            subscriber.backlog.queue(bytes.len());
            subscriber
                .outbox
                .send(Outgoing::Bytes {
                    channel: subscriber.channel,
                    bytes: bytes.clone(),
                    backlog: Some(subscriber.backlog.clone()),
                })
                .is_ok()
        });
    }

    /// Tells every subscriber the pane's new title.
    pub fn title(&mut self, title: &str) {
        self.retain(|subscriber| {
            subscriber.send(ServerMessage::Title {
                channel: subscriber.channel,
                title: title.to_owned(),
            })
        });
    }

    /// Tells every subscriber but `except` (the client that asked) the pane's new size.
    pub fn resized(&mut self, size: Size, except: Option<SubscriberId>) {
        self.retain(|subscriber| {
            Some(subscriber.id) == except || subscriber.send(resize(subscriber.channel, size))
        });
    }

    /// Tells one subscriber the pane's size, as the answer to a refused resize.
    pub fn tell_size(&self, id: SubscriberId, size: Size) {
        if let Some(subscriber) = self.find(id) {
            subscriber.send(resize(subscriber.channel, size));
        }
    }

    /// A client asked for `size`. The primary client's size wins; while there is none, any
    /// client's does.
    pub fn request_resize(&mut self, id: SubscriberId, size: Size) -> ResizeDecision {
        let Some(subscriber) = self.subscribers.iter_mut().find(|s| s.id == id) else {
            return ResizeDecision::Refuse;
        };
        subscriber.requested = Some(size);
        match self.primary {
            Some(primary) if primary != id => ResizeDecision::Refuse,
            _ => ResizeDecision::Apply(size),
        }
    }

    /// Makes `id` the primary client. Returns the size it last asked for, to apply now.
    pub fn make_primary(&mut self, id: SubscriberId) -> Option<Size> {
        let subscriber = self.find(id)?;
        let requested = subscriber.requested;
        self.primary = Some(id);
        requested
    }

    fn find(&self, id: SubscriberId) -> Option<&Subscriber> {
        self.subscribers.iter().find(|subscriber| subscriber.id == id)
    }

    /// Keeps the subscribers `send` returns true for; a primary that is dropped is forgotten.
    fn retain(&mut self, mut send: impl FnMut(&Subscriber) -> bool) {
        let primary = &mut self.primary;
        self.subscribers.retain(|subscriber| {
            let keep = send(subscriber);
            if !keep && *primary == Some(subscriber.id) {
                *primary = None;
            }
            keep
        });
    }
}

fn resize(channel: Channel, size: Size) -> ServerMessage {
    ServerMessage::Resize {
        channel,
        cols: size.cols,
        rows: size.rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: Size = Size { cols: 80, rows: 24 };
    const BIG: Size = Size {
        cols: 120,
        rows: 40,
    };

    struct Client {
        id: SubscriberId,
        inbox: mpsc::UnboundedReceiver<Outgoing>,
    }

    impl Client {
        /// Everything queued so far, writing bytes as they come, as a live connection would.
        fn drain(&mut self) -> Vec<Outgoing> {
            let mut all = Vec::new();
            while let Ok(outgoing) = self.inbox.try_recv() {
                if let Outgoing::Bytes {
                    bytes,
                    backlog: Some(backlog),
                    ..
                } = &outgoing
                {
                    backlog.sent(bytes.len());
                }
                all.push(outgoing);
            }
            all
        }

        fn messages(&mut self) -> Vec<ServerMessage> {
            self.drain()
                .into_iter()
                .filter_map(|outgoing| match outgoing {
                    Outgoing::Message(message) => Some(message),
                    Outgoing::Bytes { .. } => None,
                })
                .collect()
        }

        fn bytes(&mut self) -> Vec<u8> {
            self.drain()
                .into_iter()
                .flat_map(|outgoing| match outgoing {
                    Outgoing::Bytes { bytes, .. } => bytes.to_vec(),
                    Outgoing::Message(_) => Vec::new(),
                })
                .collect()
        }
    }

    fn attach(fanout: &mut Fanout, n: u64, channel: Channel) -> Client {
        let (outbox, inbox) = mpsc::unbounded_channel();
        let id = SubscriberId(n);
        fanout.subscribe(id, channel, outbox);
        Client { id, inbox }
    }

    mod fanout {
        use super::*;

        mod output {
            use super::*;

            #[test]
            fn should_reach_every_subscriber_in_order() {
                let mut fanout = Fanout::default();
                let mut a = attach(&mut fanout, 1, 3);
                let mut b = attach(&mut fanout, 2, 7);
                fanout.output(b"one ");
                fanout.output(b"two");
                assert_eq!(a.bytes(), b"one two");
                assert_eq!(b.bytes(), b"one two");
            }

            #[test]
            fn should_carry_each_subscribers_channel() {
                let mut fanout = Fanout::default();
                let mut client = attach(&mut fanout, 1, 9);
                fanout.output(b"x");
                assert!(matches!(
                    client.drain().as_slice(),
                    [Outgoing::Bytes { channel: 9, .. }]
                ));
            }

            #[test]
            fn should_stop_after_detach() {
                let mut fanout = Fanout::default();
                let mut client = attach(&mut fanout, 1, 3);
                fanout.unsubscribe(client.id);
                fanout.output(b"x");
                assert!(client.drain().is_empty());
            }
        }

        mod lagged {
            use super::*;

            #[test]
            fn should_keep_a_client_that_writes_what_it_gets() {
                let mut fanout = Fanout::default();
                let mut client = attach(&mut fanout, 1, 3);
                for _ in 0..4 {
                    fanout.output(&vec![b'x'; LAG_LIMIT / 2]);
                    assert_eq!(client.bytes().len(), LAG_LIMIT / 2);
                }
            }

            #[test]
            fn should_allow_exactly_the_limit_behind() {
                let mut fanout = Fanout::default();
                let mut client = attach(&mut fanout, 1, 3);
                fanout.output(&vec![b'x'; LAG_LIMIT - 1]);
                fanout.output(b"y");
                assert_eq!(client.bytes().len(), LAG_LIMIT);
            }

            #[test]
            fn should_send_lagged_once_a_client_is_more_than_the_limit_behind() {
                let mut fanout = Fanout::default();
                let mut slow = attach(&mut fanout, 1, 3);
                fanout.output(&vec![b'x'; LAG_LIMIT]);
                fanout.output(b"y");
                fanout.output(b"z");

                let queued = slow.inbox.len();
                let last = slow.drain().pop();
                assert_eq!(queued, 2, "the first chunk, then lagged and nothing more");
                assert!(matches!(
                    last,
                    Some(Outgoing::Message(ServerMessage::Lagged { channel: 3 }))
                ));
            }

            #[test]
            fn should_not_hold_back_other_clients() {
                let mut fanout = Fanout::default();
                let _slow = attach(&mut fanout, 1, 3);
                let mut fast = attach(&mut fanout, 2, 4);
                for _ in 0..3 {
                    fanout.output(&vec![b'x'; LAG_LIMIT / 2]);
                    fast.drain();
                }
                fanout.output(b"still here");
                assert_eq!(fast.bytes(), b"still here");
            }
        }

        mod resize {
            use super::*;

            #[test]
            fn should_apply_any_clients_size_while_none_is_primary() {
                let mut fanout = Fanout::default();
                let a = attach(&mut fanout, 1, 3);
                assert_eq!(
                    fanout.request_resize(a.id, BIG),
                    ResizeDecision::Apply(BIG)
                );
            }

            #[test]
            fn should_apply_only_the_primarys_size() {
                let mut fanout = Fanout::default();
                let a = attach(&mut fanout, 1, 3);
                let b = attach(&mut fanout, 2, 3);
                fanout.make_primary(a.id);
                assert_eq!(fanout.request_resize(b.id, SMALL), ResizeDecision::Refuse);
                assert_eq!(
                    fanout.request_resize(a.id, BIG),
                    ResizeDecision::Apply(BIG)
                );
            }

            #[test]
            fn should_make_the_most_recent_primary_win() {
                let mut fanout = Fanout::default();
                let a = attach(&mut fanout, 1, 3);
                let b = attach(&mut fanout, 2, 3);
                fanout.make_primary(a.id);
                fanout.make_primary(b.id);
                assert_eq!(fanout.request_resize(a.id, BIG), ResizeDecision::Refuse);
            }

            #[test]
            fn should_apply_the_size_a_new_primary_asked_for_before() {
                let mut fanout = Fanout::default();
                let a = attach(&mut fanout, 1, 3);
                let b = attach(&mut fanout, 2, 3);
                fanout.make_primary(a.id);
                fanout.request_resize(b.id, SMALL);
                assert_eq!(fanout.make_primary(b.id), Some(SMALL));
            }

            #[test]
            fn should_forget_a_primary_that_detached() {
                let mut fanout = Fanout::default();
                let a = attach(&mut fanout, 1, 3);
                let b = attach(&mut fanout, 2, 3);
                fanout.make_primary(a.id);
                fanout.unsubscribe(a.id);
                assert_eq!(
                    fanout.request_resize(b.id, SMALL),
                    ResizeDecision::Apply(SMALL)
                );
            }

            #[test]
            fn should_tell_every_other_client_the_new_size() {
                let mut fanout = Fanout::default();
                let mut a = attach(&mut fanout, 1, 3);
                let mut b = attach(&mut fanout, 2, 5);
                fanout.resized(BIG, Some(a.id));
                assert!(a.messages().is_empty());
                assert_eq!(
                    b.messages(),
                    [ServerMessage::Resize {
                        channel: 5,
                        cols: 120,
                        rows: 40
                    }]
                );
            }
        }

        mod title {
            use super::*;

            #[test]
            fn should_tell_every_client_on_its_channel() {
                let mut fanout = Fanout::default();
                let mut a = attach(&mut fanout, 1, 3);
                fanout.title("◐ agent");
                assert_eq!(
                    a.messages(),
                    [ServerMessage::Title {
                        channel: 3,
                        title: "◐ agent".into()
                    }]
                );
            }
        }
    }
}
