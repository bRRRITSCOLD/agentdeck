//! Task comments: `/v1/tasks/{id}/comments` and `/v1/comments/{id}` (spec §9).

use agentdeck_core::ws::Entity;
use agentdeck_core::{CommentAuthor, CommentId, TaskId};
use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    HttpResponseUpdatedNoContent, Path, RequestContext, TypedBody, endpoint,
};

use super::{ApiContext, Authorized, IdPath};
use crate::agents::delivery;
use crate::db;
use crate::model::{Comment, NewComment};

/// A task's comments, oldest first.
#[endpoint { method = GET, path = "/v1/tasks/{id}/comments" }]
pub(crate) async fn comment_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<TaskId>>,
) -> Result<HttpResponseOk<Vec<Comment>>, HttpError> {
    let task = path.into_inner().id;
    let comments = rqctx
        .context()
        .db
        .call(move |c| db::comments::list(c, task))
        .await?;
    Ok(HttpResponseOk(comments))
}

/// Adds a comment to a task. With `pane`, it is that pane's agent's and is never delivered.
/// Without it, it is the user's: typed into the task's newest live agent pane once that pane
/// is idle or done, or else carried by the task's next launch prompt (spec §9).
#[endpoint { method = POST, path = "/v1/tasks/{id}/comments" }]
pub(crate) async fn comment_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<TaskId>>,
    body: TypedBody<NewComment>,
) -> Result<HttpResponseCreated<Comment>, HttpError> {
    let task = path.into_inner().id;
    let new = body.into_inner();
    let context = rqctx.context();
    let comment = context
        .db
        .call(move |c| db::comments::create(c, task, &new))
        .await?;
    context.events.changed(Entity::Comment, comment.id);
    if comment.author == CommentAuthor::Agent {
        // The task's count of unseen agent comments went up.
        context.events.changed(Entity::Task, task);
    }
    delivery::deliver_to_task_pane(&context.db, &context.terminals, &context.events, &comment)
        .await;
    Ok(HttpResponseCreated(comment))
}

/// Marks every agent comment on a task seen by the user: the GUI does this when the user
/// opens the task, which clears its "new from agent" dot.
#[endpoint { method = POST, path = "/v1/tasks/{id}/comments/seen" }]
pub(crate) async fn comment_seen(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<TaskId>>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let task = path.into_inner().id;
    let context = rqctx.context();
    let seen = context
        .db
        .call(move |c| db::comments::mark_seen(c, task))
        .await?;
    if !seen.is_empty() {
        for id in seen {
            context.events.changed(Entity::Comment, id);
        }
        context.events.changed(Entity::Task, task);
    }
    Ok(HttpResponseUpdatedNoContent())
}

/// Deletes a comment.
#[endpoint { method = DELETE, path = "/v1/comments/{id}" }]
pub(crate) async fn comment_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<CommentId>>,
) -> Result<HttpResponseDeleted, HttpError> {
    let id = path.into_inner().id;
    rqctx
        .context()
        .db
        .call(move |c| db::comments::delete(c, id))
        .await?;
    rqctx.context().events.changed(Entity::Comment, id);
    Ok(HttpResponseDeleted())
}
