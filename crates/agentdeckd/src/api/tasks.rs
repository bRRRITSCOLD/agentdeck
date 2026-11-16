//! The global task list: `/v1/tasks`.

use agentdeck_core::ws::Entity;
use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    Path, Query, RequestContext, TypedBody, endpoint,
};

use agentdeck_core::TaskId;

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::db;
use crate::model::{NewTask, Task, TaskFilter, TaskPatch};

fn check_title(title: Option<&str>) -> Result<(), HttpError> {
    match title {
        Some(title) if title.trim().is_empty() => Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_title",
            "a task's title can't be empty".into(),
        )),
        _ => Ok(()),
    }
}

/// The tasks matching every filter given, oldest first.
#[endpoint { method = GET, path = "/v1/tasks" }]
pub(crate) async fn task_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    query: Query<TaskFilter>,
) -> Result<HttpResponseOk<Vec<Task>>, HttpError> {
    let filter = query.into_inner();
    let tasks = rqctx
        .context()
        .db
        .call(move |c| db::tasks::list(c, &filter))
        .await?;
    Ok(HttpResponseOk(tasks))
}

/// Creates a plain task.
#[endpoint { method = POST, path = "/v1/tasks" }]
pub(crate) async fn task_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<NewTask>,
) -> Result<HttpResponseCreated<Task>, HttpError> {
    let new = body.into_inner();
    check_title(Some(&new.title))?;
    let task = rqctx
        .context()
        .db
        .call(move |c| db::tasks::create(c, &new))
        .await?;
    rqctx.context().events.changed(Entity::Task, task.id);
    Ok(HttpResponseCreated(task))
}

/// One task.
#[endpoint { method = GET, path = "/v1/tasks/{id}" }]
pub(crate) async fn task_get(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<TaskId>>,
) -> Result<HttpResponseOk<Task>, HttpError> {
    let id = path.into_inner().id;
    let task = rqctx
        .context()
        .db
        .call(move |c| db::tasks::get(c, id))
        .await?;
    Ok(HttpResponseOk(task))
}

/// Changes a task's title, body, status or workspace.
///
/// `"workspace_id": null` moves the task to the inbox. Refuses a blank title, or another title for a PR task, whose title is its PR's
/// (`invalid_title`), and a workspace that doesn't exist or is archived
/// (`unknown_workspace`). A PR task's workspace set here sticks: PR sync never moves it again.
/// Moving a task out of a linked pane's workspace unlinks the pane.
#[endpoint { method = PATCH, path = "/v1/tasks/{id}" }]
pub(crate) async fn task_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<TaskId>>,
    body: TypedBody<TaskPatch>,
) -> Result<HttpResponseOk<Task>, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    let patch = body.into_inner();
    check_title(patch.title.as_deref())?;
    let edited = context
        .db
        .call(move |c| db::tasks::edit(c, id, &patch))
        .await?;
    for pane in edited.unlinked {
        context.terminals.link(pane, None);
        context.events.changed(Entity::Pane, pane);
    }
    context.events.changed(Entity::Task, edited.task.id);
    Ok(HttpResponseOk(edited.task))
}

/// Deletes a task and its comments.
#[endpoint { method = DELETE, path = "/v1/tasks/{id}" }]
pub(crate) async fn task_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<TaskId>>,
) -> Result<HttpResponseDeleted, HttpError> {
    let id = path.into_inner().id;
    let context = rqctx.context();
    context.db.call(move |c| db::tasks::delete(c, id)).await?;
    context.terminals.unlink_task(id);
    context.events.changed(Entity::Task, id);
    Ok(HttpResponseDeleted())
}
