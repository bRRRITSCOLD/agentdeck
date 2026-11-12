//! The global task list: `/v1/tasks`.

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    Path, Query, RequestContext, TypedBody, endpoint,
};

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
    Ok(HttpResponseCreated(task))
}

/// One task.
#[endpoint { method = GET, path = "/v1/tasks/{id}" }]
pub(crate) async fn task_get(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
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
#[endpoint { method = PATCH, path = "/v1/tasks/{id}" }]
pub(crate) async fn task_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
    body: TypedBody<TaskPatch>,
) -> Result<HttpResponseOk<Task>, HttpError> {
    let id = path.into_inner().id;
    let patch = body.into_inner();
    check_title(patch.title.as_deref())?;
    let task = rqctx
        .context()
        .db
        .call(move |c| db::tasks::update(c, id, &patch))
        .await?;
    Ok(HttpResponseOk(task))
}

/// Deletes a task and its notes.
#[endpoint { method = DELETE, path = "/v1/tasks/{id}" }]
pub(crate) async fn task_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let id = path.into_inner().id;
    rqctx
        .context()
        .db
        .call(move |c| db::tasks::delete(c, id))
        .await?;
    Ok(HttpResponseDeleted())
}
