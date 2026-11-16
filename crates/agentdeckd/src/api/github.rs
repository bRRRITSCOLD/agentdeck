//! The GitHub token and Sign in with GitHub: `/v1/github` (spec §4, §8). No route answers
//! with the token or the client ID.

use dropshot::{
    ClientErrorStatusCode, ErrorStatusCode, HttpError, HttpResponseDeleted, HttpResponseOk,
    RequestContext, TypedBody, endpoint,
};

use super::{ApiContext, Authorized, client_error};
use crate::github_auth::AuthError;
use crate::model::{DeviceSignIn, GithubAuthInfo, GithubClientIdBody, GithubTokenBody};

/// Where the GitHub token comes from, whose it is and what it may do, and whether an OAuth
/// client ID is set. Never the token or the client ID.
#[endpoint { method = GET, path = "/v1/github/auth" }]
pub(crate) async fn github_auth_get(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<GithubAuthInfo>, HttpError> {
    Ok(HttpResponseOk(rqctx.context().github.info()))
}

/// Checks a pasted token with GitHub and saves it in the system keyring, replacing any saved
/// one; PR sync syncs at once. A blank token, or one GitHub answers 401 for, is 422
/// `invalid_token`; no keyring is 503 `no_keyring`; any other GitHub failure is 502
/// `github_error`.
#[endpoint { method = PUT, path = "/v1/github/token" }]
pub(crate) async fn github_token_put(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<GithubTokenBody>,
) -> Result<HttpResponseOk<GithubAuthInfo>, HttpError> {
    let github = &rqctx.context().github;
    github
        .save_token(&body.into_inner().token)
        .await
        .map_err(http_error)?;
    Ok(HttpResponseOk(github.info()))
}

/// Deletes the saved token (sign out); the token then comes from the next source, if any.
#[endpoint { method = DELETE, path = "/v1/github/token" }]
pub(crate) async fn github_token_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<GithubAuthInfo>, HttpError> {
    let github = &rqctx.context().github;
    github.delete_token().await.map_err(http_error)?;
    Ok(HttpResponseOk(github.info()))
}

/// Saves the OAuth client ID Sign in with GitHub uses in the system keyring. Write-only: no
/// route reads it back. A blank one is 400 `invalid_client_id`; no keyring is 503
/// `no_keyring`.
#[endpoint { method = PUT, path = "/v1/github/client-id" }]
pub(crate) async fn github_client_id_put(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<GithubClientIdBody>,
) -> Result<HttpResponseOk<GithubAuthInfo>, HttpError> {
    let github = &rqctx.context().github;
    github
        .save_client_id(&body.into_inner().client_id)
        .await
        .map_err(http_error)?;
    Ok(HttpResponseOk(github.info()))
}

/// Deletes the saved OAuth client ID.
#[endpoint { method = DELETE, path = "/v1/github/client-id" }]
pub(crate) async fn github_client_id_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<GithubAuthInfo>, HttpError> {
    let github = &rqctx.context().github;
    github.delete_client_id().await.map_err(http_error)?;
    Ok(HttpResponseOk(github.info()))
}

/// Starts Sign in with GitHub (the OAuth device flow, scope `repo`), cancelling one already
/// running, and answers with the code the user enters on GitHub. How it ends comes as a
/// `github_auth` WebSocket event. No client ID is 409 `no_client_id`, no keyring 503
/// `no_keyring`, and GitHub refusing 502 `github_error`.
#[endpoint { method = POST, path = "/v1/github/device" }]
pub(crate) async fn github_device_start(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<DeviceSignIn>, HttpError> {
    let sign_in = rqctx
        .context()
        .github
        .start_sign_in()
        .await
        .map_err(http_error)?;
    Ok(HttpResponseOk(sign_in))
}

/// Cancels the sign-in running now, if any. No event.
#[endpoint { method = DELETE, path = "/v1/github/device" }]
pub(crate) async fn github_device_cancel(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseDeleted, HttpError> {
    rqctx.context().github.cancel_sign_in();
    Ok(HttpResponseDeleted())
}

/// The status and `error_code` of each failure.
fn http_error(err: AuthError) -> HttpError {
    let message = err.to_string();
    match err {
        AuthError::InvalidToken => client_error(
            ClientErrorStatusCode::UNPROCESSABLE_ENTITY,
            "invalid_token",
            message,
        ),
        AuthError::InvalidClientId | AuthError::UserCodeForClientId => client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_client_id",
            message,
        ),
        AuthError::NoClientId => {
            client_error(ClientErrorStatusCode::CONFLICT, "no_client_id", message)
        }
        AuthError::NoKeyring => {
            server_error(ErrorStatusCode::SERVICE_UNAVAILABLE, "no_keyring", message)
        }
        AuthError::Github(_) => server_error(ErrorStatusCode::BAD_GATEWAY, "github_error", message),
        AuthError::Keyring(_) => HttpError::for_internal_error(message),
    }
}

/// A 5xx with a `code` and a message the client shows.
fn server_error(status: ErrorStatusCode, code: &str, message: String) -> HttpError {
    HttpError {
        status_code: status,
        error_code: Some(code.to_owned()),
        external_message: message.clone(),
        internal_message: message,
        headers: None,
    }
}
