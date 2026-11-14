//! `agentdeck_client::notes`: the note operations that take more than one call, against a
//! real daemon.

use agentdeck_client::Client;
use agentdeck_client::notes::send_note;
use agentdeck_client::types::{NewNote, Note};
use agentdeck_core::NoteId;

use crate::support::TestDaemon;
use crate::support::terminal::{Place, printed, sh};

async fn note_on(client: &Client, place: &Place, body: &str) -> Note {
    client
        .note_create()
        .body(NewNote {
            body: body.into(),
            workspace_id: Some(place.workspace.id),
            task_id: None,
        })
        .send()
        .await
        .expect("create a note")
        .into_inner()
}

async fn read_back(client: &Client, note: NoteId) -> Note {
    let notes = client.note_list().send().await.expect("list notes");
    (notes.into_inner().into_iter())
        .find(|n| n.id == note)
        .expect("the note is still there")
}

mod send_note {
    use super::*;

    #[tokio::test]
    async fn should_type_the_note_with_enter_now_and_mark_it_read_by_the_pane() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = (place.spawn(
            &daemon.client,
            &sh("read line; echo \"got:$line\"; sleep 60"),
        ))
        .await;
        let note = note_on(&daemon.client, &place, "Keep the arrow keys in TabsList").await;

        send_note(&daemon.client, note.id, &note.body, pane.id)
            .await
            .expect("send the note");

        printed(
            &daemon.client,
            pane.id,
            "got:Keep the arrow keys in TabsList",
        )
        .await;
        let note = read_back(&daemon.client, note.id).await;
        assert!(note.read_at.is_some());
        assert_eq!(note.read_by_pane, Some(pane.id));
    }

    #[tokio::test]
    async fn should_leave_the_note_unread_when_the_pane_is_gone() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        let note = note_on(&daemon.client, &place, "too late").await;
        (daemon.client.pane_delete().id(pane.id).send())
            .await
            .expect("delete the pane");

        let sent = send_note(&daemon.client, note.id, &note.body, pane.id).await;

        assert!(sent.is_err());
        let note = read_back(&daemon.client, note.id).await;
        assert_eq!((note.read_at, note.read_by_pane), (None, None));
    }
}
