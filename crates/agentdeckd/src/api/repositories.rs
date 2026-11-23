//! Repositories and their Links to Workspaces: `/v1/repositories` and
//! `/v1/workspaces/{id}/links` (spec §3, §4).

use agentdeck_core::ws::Entity;
use agentdeck_core::{RepositoryId, RepositoryName, WorkspaceId};
use dropshot::{
    ClientErrorStatusCode, ErrorStatusCode, HttpError, HttpResponseDeleted, HttpResponseOk, Path,
    Query, RequestContext, TypedBody, endpoint,
};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{ApiContext, Authorized, IdPath, client_error, server_error};
use crate::db;
use crate::db::workspaces::Linking;
use crate::github_auth::NoToken;
use crate::model::{NewLink, Repository, RepositoryFilter, RepositoryLinks};
use crate::prsync::github::{GithubError, RemoteRepository};

/// Every Repository agentdeck keeps, each with the Workspaces that link it (oldest first,
/// archived ones included) and whether the GitHub sign-in can still see it, by name ignoring
/// case. A Repository is kept while a Workspace links it or one of your kept pull requests
/// belongs to it. With `workspace`, only the Repositories that Workspace links; one that
/// doesn't exist is 400 `unknown_workspace`.
#[endpoint { method = GET, path = "/v1/repositories" }]
pub(crate) async fn repository_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    query: Query<RepositoryFilter>,
) -> Result<HttpResponseOk<Vec<RepositoryLinks>>, HttpError> {
    let workspace = query.into_inner().workspace;
    let repositories = rqctx
        .context()
        .db
        .call(move |c| db::repositories::list(c, workspace))
        .await?;
    Ok(HttpResponseOk(repositories))
}

/// `GET /v1/repositories/resolve` query.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct ResolveQuery {
    /// The Repository's `owner/name`, or its GitHub URL.
    repository: RepositoryName,
}

/// Looks a typed Repository up on GitHub (`GET /repos/{owner}/{repo}`) and answers it as
/// GitHub shows it now. Stores nothing new; a Repository agentdeck keeps takes GitHub's
/// name, URL, private and archived. One GitHub doesn't show the sign-in is 400
/// `unknown_repository`; no usable token is 409 `not_signed_in`; any other GitHub failure is
/// 502 `github_error`.
#[endpoint { method = GET, path = "/v1/repositories/resolve" }]
pub(crate) async fn repository_resolve(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    query: Query<ResolveQuery>,
) -> Result<HttpResponseOk<Repository>, HttpError> {
    let context = rqctx.context();
    let found = look_up(context, &query.into_inner().repository).await?;
    let known = found.clone();
    let updated = context
        .db
        .call(move |c| {
            if db::repositories::is_known(c, known.id)? {
                db::repositories::store(c, &known)
            } else {
                Ok(false)
            }
        })
        .await?;
    if updated {
        context.events.changed(Entity::Repository, found.id);
    }
    Ok(HttpResponseOk(Repository {
        id: found.id,
        name: found.name,
        url: found.url,
        private: found.private,
        archived: found.archived,
    }))
}

/// Links the Workspace to a Repository and answers the Repository with every Workspace that
/// links it. A Repository agentdeck doesn't know by that name is looked up on GitHub first
/// and stored, as `GET /v1/repositories/resolve` does; one GitHub doesn't show the sign-in is
/// 400 `unknown_repository` and nothing is linked. Linking a pair already linked changes
/// nothing. A Workspace that doesn't exist is 404 `not_found`. Sends `changed` for the
/// Repository, then the Workspace, when the Link is new.
#[endpoint { method = POST, path = "/v1/workspaces/{id}/links" }]
pub(crate) async fn link_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
    body: TypedBody<NewLink>,
) -> Result<HttpResponseOk<RepositoryLinks>, HttpError> {
    let context = rqctx.context();
    let workspace = path.into_inner().id;
    let name = body.into_inner().repository;
    let db = &context.db;
    let wanted = name.clone();
    let known = db
        .call(move |c| {
            if !db::exists(c, "workspaces", workspace)? {
                return Err(db::DbError::NotFound("workspace"));
            }
            db::repositories::by_name(c, &wanted)
        })
        .await?;
    let linked = match known {
        Some(id) => {
            db.call(move |c| db::repositories::link(c, workspace, id))
                .await?
        }
        None => {
            let found = look_up(context, &name).await?;
            db.call(move |c| db::repositories::store_and_link(c, workspace, &found))
                .await?
        }
    };
    if linked.changed {
        context
            .events
            .changed(Entity::Repository, linked.repository.id);
        context.events.changed(Entity::Workspace, workspace);
    }
    Ok(HttpResponseOk(linked.repository))
}

/// A Link's path: the Workspace and the Repository it ties.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct LinkPath {
    /// The Workspace's id.
    id: WorkspaceId,
    /// The Repository's id, GitHub's.
    repository: RepositoryId,
}

/// Unlinks the Workspace from a Repository. The Repository goes too when no other Workspace
/// links it and none of your kept pull requests belongs to it. A Workspace that doesn't
/// exist, or a Repository it doesn't link, is 404 `not_found`. Sends `changed` for the
/// Repository, then the Workspace.
#[endpoint { method = DELETE, path = "/v1/workspaces/{id}/links/{repository}" }]
pub(crate) async fn link_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<LinkPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let context = rqctx.context();
    let LinkPath { id, repository } = path.into_inner();
    context
        .db
        .call(move |c| db::repositories::unlink(c, id, repository))
        .await?;
    context.events.changed(Entity::Repository, repository);
    context.events.changed(Entity::Workspace, id);
    Ok(HttpResponseDeleted())
}

/// What linking each of `names` takes, in order: a Repository agentdeck keeps by that name
/// now, ignoring case, or else the one GitHub shows by it, looked up as
/// `GET /v1/repositories/resolve` does. The first that fails fails them all, before anything is
/// linked.
pub(crate) async fn linkings(
    context: &ApiContext,
    names: Vec<RepositoryName>,
) -> Result<Vec<Linking>, HttpError> {
    let wanted = names.clone();
    let known = context
        .db
        .call(move |c| {
            (wanted.iter())
                .map(|name| db::repositories::by_name(c, name))
                .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    let mut links = Vec::with_capacity(names.len());
    for (name, known) in names.iter().zip(known) {
        links.push(match known {
            Some(id) => Linking::Known(id),
            None => Linking::Found(look_up(context, name).await?),
        });
    }
    Ok(links)
}

/// The Repository GitHub calls `name` now, read with the sign-in's token. A 401 reads the
/// token's sources again and tries once more, as PR sync does.
async fn look_up(
    context: &ApiContext,
    name: &RepositoryName,
) -> Result<RemoteRepository, HttpError> {
    let auth = &context.github;
    let github = &context.github_api;
    let token = auth
        .current_token()
        .await
        .map_err(|err| not_signed_in(name, &err))?;
    let found = match github.repository(&token, name).await {
        Err(GithubError::Unauthorized) => {
            let fresh = auth
                .reread()
                .await
                .map_err(|err| not_signed_in(name, &err))?;
            let again = github.repository(&fresh, name).await;
            if matches!(again, Err(GithubError::Unauthorized)) {
                auth.rejected();
                return Err(not_signed_in(name, &NoToken::Rejected));
            }
            again
        }
        result => result,
    };
    match found {
        Ok(Some(repository)) => Ok(repository),
        Ok(None) => Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "unknown_repository",
            format!("GitHub can't see {name}. Check the name or your token's access."),
        )),
        Err(err) => Err(server_error(
            ErrorStatusCode::BAD_GATEWAY,
            "github_error",
            err.to_string(),
        )),
    }
}

/// 409 `not_signed_in`: there's no token to ask GitHub about `name` with.
fn not_signed_in(name: &RepositoryName, why: &NoToken) -> HttpError {
    let message = match why {
        NoToken::None => format!("Sign in to GitHub to look up {name}."),
        NoToken::Rejected => {
            format!("GitHub rejected the token, so agentdeck can't look up {name}.")
        }
    };
    client_error(ClientErrorStatusCode::CONFLICT, "not_signed_in", message)
}
