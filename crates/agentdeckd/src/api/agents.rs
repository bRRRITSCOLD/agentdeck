//! agentdeck's MCP server in the agent CLIs' own config: `/v1/agents/mcp` (spec §4, §6, §11).

use dropshot::{HttpError, HttpResponseOk, RequestContext, endpoint};

use super::{ApiContext, Authorized};
use crate::agents::{self, registration};
use crate::model::McpRegistration;

/// Whether agentdeck's MCP server is in Claude Code's and Codex's own config, so agents the
/// user starts in a shell pane have its tools too.
///
/// An agent CLI not on agentdeckd's `PATH` is `not_installed`; a config that can't be read is
/// `unreadable`, with `error` saying why. Neither is an error for the other agent.
#[endpoint { method = GET, path = "/v1/agents/mcp" }]
pub(crate) async fn agents_mcp_get(
    _rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<McpRegistration>, HttpError> {
    blocking(registration::status).await
}

/// Adds agentdeck's MCP server to Claude Code (`claude mcp add --scope user agentdeck -- <cli>
/// mcp`) and Codex (`[mcp_servers.agentdeck]` in its `config.toml`) where it isn't yet, and
/// answers with where it stands after.
///
/// Idempotent: an entry that already runs this `agentdeck mcp` is left alone, and an agent CLI
/// that isn't installed is skipped. An agent whose entry couldn't be added has `error` saying
/// why; the other is still added, and the answer is 200.
#[endpoint { method = PUT, path = "/v1/agents/mcp" }]
pub(crate) async fn agents_mcp_install(
    _rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<McpRegistration>, HttpError> {
    blocking(registration::install).await
}

/// Runs `f` on the daemon's environment and CLI off the async runtime: it reads files and may
/// run `claude`.
async fn blocking(
    f: fn(&registration::Environment, &str) -> McpRegistration,
) -> Result<HttpResponseOk<McpRegistration>, HttpError> {
    let registration = tokio::task::spawn_blocking(move || {
        let cli = agents::cli();
        f(
            &registration::Environment::from_daemon(),
            &cli.to_string_lossy(),
        )
    })
    .await
    .map_err(|err| HttpError::for_internal_error(format!("checking the agents' config: {err}")))?;
    Ok(HttpResponseOk(registration))
}
