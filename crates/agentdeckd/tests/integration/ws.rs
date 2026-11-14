//! The WebSocket at `/v1/ws` (spec §5), with `sh` in real PTYs.

use std::time::Duration;

use agentdeck_client::types::NewTask;
use agentdeck_core::AgentState;
use agentdeck_core::ws::{ClientMessage, Entity, ServerMessage};
use tokio_tungstenite::tungstenite;

use crate::support::TestDaemon;
use crate::support::terminal::{Place, daemon_with_shell_rules, printed, sh};
use crate::support::ws::{Frame, Ws, request};

mod get_v1_ws {
    use super::*;

    #[tokio::test]
    async fn should_refuse_an_upgrade_without_the_token_401() {
        let daemon = TestDaemon::start().await;

        let result = tokio_tungstenite::connect_async(request(&daemon, None)).await;

        match result {
            Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 401),
            other => panic!("expected 401, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn should_refuse_a_wrong_token_401() {
        let daemon = TestDaemon::start().await;

        let result = tokio_tungstenite::connect_async(request(&daemon, Some("nope"))).await;

        assert!(matches!(result, Err(tungstenite::Error::Http(r)) if r.status() == 401));
    }

    mod attach {
        use super::*;

        #[tokio::test]
        async fn should_send_attached_then_the_screen_so_far() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place
                .spawn(
                    &daemon.client,
                    &sh("printf 'before \\033[1;31mattach'; sleep 60"),
                )
                .await;
            printed(&daemon.client, pane.id, "attach").await;
            let mut ws = Ws::connect(&daemon).await;

            ws.send(ClientMessage::Attach {
                pane: pane.id,
                channel: 3,
            })
            .await;

            let (channel, attached_pane, cols, rows, title) = ws
                .until(|frame| match frame {
                    Frame::Message(ServerMessage::Attached {
                        channel,
                        pane,
                        cols,
                        rows,
                        title,
                        ..
                    }) => Some((channel, pane, cols, rows, title)),
                    _ => None,
                })
                .await;
            assert_eq!(
                (channel, attached_pane, cols, rows, title.as_str()),
                (3, pane.id, 80, 24, "")
            );
            let Frame::Bytes(3, snapshot) = ws.next().await else {
                panic!("expected the snapshot on channel 3");
            };
            assert!(String::from_utf8_lossy(&snapshot).contains("\x1b[0;1;31ma"));
        }

        #[tokio::test]
        async fn should_end_the_snapshot_with_the_dir_the_shell_last_reported() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place
                .spawn(
                    &daemon.client,
                    &sh("printf '\\033]7;file://box/first\\007\\033]7;file://box/w/repo\\033\\\\$ '; sleep 60"),
                )
                .await;
            printed(&daemon.client, pane.id, "$ ").await;
            let mut ws = Ws::connect(&daemon).await;
            ws.attach(pane.id, 1).await;
            ws.send(ClientMessage::Detach { channel: 1 }).await;

            // A reattach, as after a `lagged` or a GUI restart, still carries it.
            ws.send(ClientMessage::Attach {
                pane: pane.id,
                channel: 2,
            })
            .await;

            let snapshot = ws
                .until(|frame| match frame {
                    Frame::Bytes(2, bytes) => Some(bytes),
                    _ => None,
                })
                .await;
            assert!(
                snapshot.ends_with(b"\x1b]7;file://box/w/repo\x1b\\"),
                "{:?}",
                String::from_utf8_lossy(&snapshot)
            );
        }

        #[tokio::test]
        async fn should_render_the_snapshot_like_the_pane() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place
                .spawn(&daemon.client, &sh("printf 'one\\ntwo\\n'; sleep 60"))
                .await;
            printed(&daemon.client, pane.id, "two").await;
            let mut ws = Ws::connect(&daemon).await;

            let screen = ws.attach(pane.id, 1).await;

            assert_eq!(screen.rows()[..2], ["one", "two"]);
        }

        #[tokio::test]
        async fn should_lose_and_double_nothing_between_the_snapshot_and_live_output() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place
                .spawn(
                    &daemon.client,
                    &sh(
                        "i=0; while [ $i -lt 3000 ]; do echo line$i; i=$((i+1)); done; \
                         echo end; sleep 60",
                    ),
                )
                .await;
            // Attach mid-stream.
            printed(&daemon.client, pane.id, "line200\r\n").await;
            let mut ws = Ws::connect(&daemon).await;

            let mut screen = ws.attach(pane.id, 1).await;
            screen.until(&mut ws, |screen| screen.shows("end")).await;

            let lines: Vec<String> = screen
                .all_rows()
                .into_iter()
                .filter(|row| row.starts_with("line"))
                .collect();
            let expected: Vec<String> = (0..3000).map(|i| format!("line{i}")).collect();
            assert_eq!(lines, expected);
        }

        #[tokio::test]
        async fn should_type_bytes_into_the_pane() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place.spawn(&daemon.client, &["cat"]).await;
            let mut ws = Ws::connect(&daemon).await;
            let mut screen = ws.attach(pane.id, 5).await;

            ws.type_bytes(5, b"ping\r").await;

            // The terminal's echo, then cat's copy.
            screen
                .until(&mut ws, |screen| {
                    screen.rows().iter().filter(|row| *row == "ping").count() == 2
                })
                .await;
        }

        #[tokio::test]
        async fn should_answer_not_found_for_an_unknown_pane() {
            let daemon = TestDaemon::start().await;
            let mut ws = Ws::connect(&daemon).await;

            ws.send(ClientMessage::Attach {
                pane: uuid::Uuid::now_v7().into(),
                channel: 2,
            })
            .await;

            let code = ws
                .until(|frame| match frame {
                    Frame::Message(ServerMessage::Error { channel, code, .. }) => {
                        Some((channel, code))
                    }
                    _ => None,
                })
                .await;
            assert_eq!(code, (Some(2), "not_found".to_owned()));
        }

        #[tokio::test]
        async fn should_stop_sending_after_detach() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place.spawn(&daemon.client, &["cat"]).await;
            let mut ws = Ws::connect(&daemon).await;
            ws.attach(pane.id, 1).await;

            ws.send(ClientMessage::Detach { channel: 1 }).await;
            ws.send(ClientMessage::Detach { channel: 1 }).await;

            // The second detach's error proves the first was handled; nothing else follows.
            ws.until(|frame| match frame {
                Frame::Message(ServerMessage::Error { code, .. }) if code == "not_attached" => {
                    Some(())
                }
                Frame::Bytes(..) => panic!("bytes after detach"),
                _ => None,
            })
            .await;
            daemon
                .client
                .pane_input()
                .id(pane.id)
                .body(agentdeck_client::types::PaneInput {
                    text: "after\r".into(),
                    when: agentdeck_client::types::InputWhen::Now,
                })
                .send()
                .await
                .expect("input");
            printed(&daemon.client, pane.id, "after").await;
            assert!(ws.quiet_for(Duration::from_millis(300)).await.is_none());
        }
    }

    mod resize {
        use super::*;

        /// Types `stty size` and waits for `<rows> <cols>` to show `times` times.
        async fn stty_size(
            ws: &mut Ws,
            screen: &mut crate::support::ws::Screen,
            size: &str,
            times: usize,
        ) {
            ws.type_bytes(1, b"stty size\r").await;
            screen
                .until(ws, |screen| {
                    screen.rows().iter().filter(|row| *row == size).count() == times
                })
                .await;
        }

        #[tokio::test]
        async fn should_follow_the_primary_client_and_tell_the_others() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place.spawn(&daemon.client, &["sh"]).await;
            let mut primary = Ws::connect(&daemon).await;
            let mut other = Ws::connect(&daemon).await;
            let mut screen = primary.attach(pane.id, 1).await;
            other.attach(pane.id, 7).await;

            primary.send(ClientMessage::Primary { channel: 1 }).await;
            primary
                .send(ClientMessage::Resize {
                    channel: 1,
                    cols: 100,
                    rows: 30,
                })
                .await;

            other
                .expect(ServerMessage::Resize {
                    channel: 7,
                    cols: 100,
                    rows: 30,
                })
                .await;
            // The primary's own screen follows the size it asked for.
            screen.take(&Frame::Message(ServerMessage::Resize {
                channel: 1,
                cols: 100,
                rows: 30,
            }));
            stty_size(&mut primary, &mut screen, "30 100", 1).await;
        }

        #[tokio::test]
        async fn should_refuse_a_resize_from_a_client_that_is_not_primary() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place.spawn(&daemon.client, &["sh"]).await;
            let mut primary = Ws::connect(&daemon).await;
            let mut other = Ws::connect(&daemon).await;
            let mut screen = primary.attach(pane.id, 1).await;
            other.attach(pane.id, 7).await;
            primary.send(ClientMessage::Primary { channel: 1 }).await;

            other
                .send(ClientMessage::Resize {
                    channel: 7,
                    cols: 50,
                    rows: 10,
                })
                .await;

            other
                .expect(ServerMessage::Resize {
                    channel: 7,
                    cols: 80,
                    rows: 24,
                })
                .await;
            stty_size(&mut primary, &mut screen, "24 80", 1).await;
        }

        #[tokio::test]
        async fn should_apply_the_size_a_client_asked_for_when_it_becomes_primary() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place.spawn(&daemon.client, &["sh"]).await;
            let mut first = Ws::connect(&daemon).await;
            let mut second = Ws::connect(&daemon).await;
            let mut screen = first.attach(pane.id, 1).await;
            second.attach(pane.id, 2).await;
            first.send(ClientMessage::Primary { channel: 1 }).await;
            second
                .send(ClientMessage::Resize {
                    channel: 2,
                    cols: 90,
                    rows: 20,
                })
                .await;
            second
                .expect(ServerMessage::Resize {
                    channel: 2,
                    cols: 80,
                    rows: 24,
                })
                .await;

            second.send(ClientMessage::Primary { channel: 2 }).await;

            first
                .until(|frame| {
                    screen.take(&frame);
                    matches!(
                        frame,
                        Frame::Message(ServerMessage::Resize {
                            cols: 90,
                            rows: 20,
                            ..
                        })
                    )
                    .then_some(())
                })
                .await;
            stty_size(&mut first, &mut screen, "20 90", 1).await;
        }
    }

    mod events {
        use super::*;

        #[tokio::test]
        async fn should_send_the_title_on_the_channel() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place.spawn(&daemon.client, &["sh"]).await;
            let mut ws = Ws::connect(&daemon).await;
            ws.attach(pane.id, 4).await;

            ws.type_bytes(4, b"printf '\\033]0;%s\\007' titled\r").await;

            ws.expect(ServerMessage::Title {
                channel: 4,
                title: "titled".into(),
            })
            .await;
        }

        #[tokio::test]
        async fn should_send_state_and_exit_to_clients_that_are_not_attached() {
            let daemon = daemon_with_shell_rules().await;
            let place = Place::new(&daemon).await;
            let mut ws = Ws::connect(&daemon).await;
            let mut sh_ws = Ws::connect(&daemon).await;
            let pane = place.spawn(&daemon.client, &["sh"]).await;
            sh_ws.attach(pane.id, 1).await;

            sh_ws
                .type_bytes(1, b"printf '\\033]0;idle\\007'; echo set\r")
                .await;
            ws.expect(ServerMessage::State {
                pane: pane.id,
                state: AgentState::Idle,
            })
            .await;
            sh_ws.type_bytes(1, b"exit 4\r").await;

            ws.expect(ServerMessage::State {
                pane: pane.id,
                state: AgentState::Exited,
            })
            .await;
            ws.expect(ServerMessage::Exit {
                pane: pane.id,
                code: Some(4),
            })
            .await;
        }

        #[tokio::test]
        async fn should_send_changed_when_an_entity_changes() {
            let daemon = TestDaemon::start().await;
            let mut ws = Ws::connect(&daemon).await;

            let task = daemon
                .client
                .task_create()
                .body(NewTask {
                    title: "t".into(),
                    body: None,
                    status: None,
                    workspace_id: None,
                })
                .send()
                .await
                .expect("create a task")
                .into_inner();

            ws.expect(ServerMessage::Changed {
                entity: Entity::Task,
                id: task.id.to_string(),
            })
            .await;
        }

        #[tokio::test]
        async fn should_send_an_event_published_right_after_the_upgrade() {
            let daemon = TestDaemon::start().await;
            // Warm the client's connection pool, so the change below goes out at once.
            daemon.client.task_list().send().await.expect("list tasks");

            for round in 0..20 {
                let mut ws = Ws::connect(&daemon).await;
                // No await between the 101 and the change: the daemon must already be
                // subscribed when the client sees the connection open.
                let task = daemon
                    .client
                    .task_create()
                    .body(NewTask {
                        title: format!("t{round}"),
                        body: None,
                        status: None,
                        workspace_id: None,
                    })
                    .send()
                    .await
                    .expect("create a task")
                    .into_inner();

                ws.expect(ServerMessage::Changed {
                    entity: Entity::Task,
                    id: task.id.to_string(),
                })
                .await;
            }
        }

        #[tokio::test]
        async fn should_send_changed_when_a_workspace_is_muted() {
            let daemon = TestDaemon::start().await;
            let workspace = daemon
                .client
                .workspace_create()
                .body_map(|new| new.name("w"))
                .send()
                .await
                .expect("create a workspace")
                .into_inner();
            let mut ws = Ws::connect(&daemon).await;

            daemon
                .client
                .workspace_update()
                .id(workspace.id)
                .body_map(|patch| patch.muted(Some(true)))
                .send()
                .await
                .expect("mute");

            ws.expect(ServerMessage::Changed {
                entity: Entity::Workspace,
                id: workspace.id.to_string(),
            })
            .await;
        }

        #[tokio::test]
        async fn should_send_changed_when_a_pane_is_created() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let mut ws = Ws::connect(&daemon).await;

            let pane = place.spawn(&daemon.client, &["cat"]).await;

            ws.expect(ServerMessage::Changed {
                entity: Entity::Pane,
                id: pane.id.to_string(),
            })
            .await;
        }
    }

    mod lagged {
        use super::*;

        #[tokio::test]
        async fn should_tell_a_client_that_fell_more_than_1_mib_behind() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let pane = place.spawn(&daemon.client, &["sh"]).await;
            let mut ws = Ws::connect_with_small_buffer(&daemon).await;
            ws.attach(pane.id, 1).await;

            // Stop reading while the pane prints far more than the socket buffers hold. The
            // quotes keep the terminal's echo of the command from matching `printed-it-all`.
            ws.type_bytes(
                1,
                b"head -c 24000000 /dev/zero | tr '\\0' x; echo; echo printed-''it-all\r",
            )
            .await;
            printed(&daemon.client, pane.id, "printed-it-all").await;

            ws.until(|frame| match frame {
                Frame::Message(ServerMessage::Lagged { channel: 1 }) => Some(()),
                _ => None,
            })
            .await;
        }
    }
}
