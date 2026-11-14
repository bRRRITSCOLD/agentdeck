//! Workspace instructions: `/v1/workspaces/{id}/instructions` and `/v1/instructions/{id}`
//! (spec §9).

use agentdeck_core::ws::Entity;
use agentdeck_core::{InstructionId, WorkspaceId};
use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, RequestContext,
    TypedBody, endpoint,
};

use super::{ApiContext, Authorized, IdPath};
use crate::db;
use crate::model::{Instruction, InstructionPatch, NewInstruction};

/// A workspace's instructions, oldest first.
#[endpoint { method = GET, path = "/v1/workspaces/{id}/instructions" }]
pub(crate) async fn instruction_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
) -> Result<HttpResponseOk<Vec<Instruction>>, HttpError> {
    let workspace = path.into_inner().id;
    let instructions = rqctx
        .context()
        .db
        .call(move |c| db::instructions::list(c, workspace))
        .await?;
    Ok(HttpResponseOk(instructions))
}

/// Adds an instruction to a workspace. Every launch prompt there carries it from now on.
#[endpoint { method = POST, path = "/v1/workspaces/{id}/instructions" }]
pub(crate) async fn instruction_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
    body: TypedBody<NewInstruction>,
) -> Result<HttpResponseCreated<Instruction>, HttpError> {
    let workspace = path.into_inner().id;
    let new = body.into_inner();
    let instruction = rqctx
        .context()
        .db
        .call(move |c| db::instructions::create(c, workspace, &new))
        .await?;
    rqctx
        .context()
        .events
        .changed(Entity::Instruction, instruction.id);
    Ok(HttpResponseCreated(instruction))
}

/// Edits an instruction.
#[endpoint { method = PATCH, path = "/v1/instructions/{id}" }]
pub(crate) async fn instruction_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<InstructionId>>,
    body: TypedBody<InstructionPatch>,
) -> Result<HttpResponseOk<Instruction>, HttpError> {
    let id = path.into_inner().id;
    let patch = body.into_inner();
    let instruction = rqctx
        .context()
        .db
        .call(move |c| db::instructions::update(c, id, &patch))
        .await?;
    rqctx.context().events.changed(Entity::Instruction, id);
    Ok(HttpResponseOk(instruction))
}

/// Deletes an instruction.
#[endpoint { method = DELETE, path = "/v1/instructions/{id}" }]
pub(crate) async fn instruction_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<InstructionId>>,
) -> Result<HttpResponseDeleted, HttpError> {
    let id = path.into_inner().id;
    rqctx
        .context()
        .db
        .call(move |c| db::instructions::delete(c, id))
        .await?;
    rqctx.context().events.changed(Entity::Instruction, id);
    Ok(HttpResponseDeleted())
}
