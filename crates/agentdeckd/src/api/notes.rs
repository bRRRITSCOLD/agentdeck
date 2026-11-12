//! Notes on workspaces and tasks: `/v1/notes`.

use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, Query,
    RequestContext, TypedBody, endpoint,
};

use agentdeck_core::NoteId;

use super::{ApiContext, Authorized, IdPath};
use crate::db;
use crate::model::{NewNote, Note, NoteFilter, NotePatch};

/// The notes matching every filter given, oldest first.
#[endpoint { method = GET, path = "/v1/notes" }]
pub(crate) async fn note_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    query: Query<NoteFilter>,
) -> Result<HttpResponseOk<Vec<Note>>, HttpError> {
    let filter = query.into_inner();
    let notes = rqctx
        .context()
        .db
        .call(move |c| db::notes::list(c, &filter))
        .await?;
    Ok(HttpResponseOk(notes))
}

/// Leaves a note on a workspace or a task.
#[endpoint { method = POST, path = "/v1/notes" }]
pub(crate) async fn note_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<NewNote>,
) -> Result<HttpResponseCreated<Note>, HttpError> {
    let new = body.into_inner();
    let note = rqctx
        .context()
        .db
        .call(move |c| db::notes::create(c, &new))
        .await?;
    Ok(HttpResponseCreated(note))
}

/// Edits a note, or marks it read or unread.
#[endpoint { method = PATCH, path = "/v1/notes/{id}" }]
pub(crate) async fn note_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<NoteId>>,
    body: TypedBody<NotePatch>,
) -> Result<HttpResponseOk<Note>, HttpError> {
    let id = path.into_inner().id;
    let patch = body.into_inner();
    let note = rqctx
        .context()
        .db
        .call(move |c| db::notes::update(c, id, &patch))
        .await?;
    Ok(HttpResponseOk(note))
}

/// Deletes a note.
#[endpoint { method = DELETE, path = "/v1/notes/{id}" }]
pub(crate) async fn note_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<NoteId>>,
) -> Result<HttpResponseDeleted, HttpError> {
    let id = path.into_inner().id;
    rqctx
        .context()
        .db
        .call(move |c| db::notes::delete(c, id))
        .await?;
    Ok(HttpResponseDeleted())
}
