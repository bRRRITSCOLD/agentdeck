//! `agentdeck agent new`: launch a `claude` or `codex` pane in a workspace (spec §6, §10).
//! The daemon builds the argv and the launch prompt; `agentdeck_client::panes::launch_agent`
//! picks the tab and places the pane in its layout, as the GUI's new agent dialog does too.

use std::path::Path;

use agentdeck_client::Client;
use agentdeck_client::types::Pane;
use agentdeck_core::{Agent, TaskId};

use crate::commands;
use agentdeck_client::ApiError;

/// What `agent new` was asked for.
pub struct Launch<'a> {
    /// The workspace: an id, slug or name.
    pub ws: &'a str,
    /// `claude` or `codex`.
    pub agent: Agent,
    /// Where it runs; the workspace's dir when absent.
    pub cwd: Option<&'a Path>,
    /// The task to link.
    pub task: Option<TaskId>,
}

/// Launches the agent and returns its pane, placed in the workspace's first empty tab or in a
/// new tab of its own (`agentdeck_client::panes::launch_agent`).
pub async fn agent_new(client: &Client, launch: Launch<'_>) -> anyhow::Result<Pane> {
    let ws_id = commands::workspace_id(client, launch.ws).await?;
    let ws = client
        .workspace_get()
        .id(ws_id)
        .send()
        .await
        .map_err(ApiError::from)?;
    let cwd = match launch.cwd {
        Some(cwd) => commands::absolute(cwd)?,
        None => ws.dir.clone(),
    };
    agentdeck_client::panes::launch_agent(client, ws_id, launch.agent, cwd, launch.task)
        .await
        .map_err(|err| launch_error(err, launch.task))
}

/// The daemon's refusal, with what to do about the ones a person can fix. An agent that
/// can't start is the daemon's message alone, which says what to do when it isn't on the
/// daemon's `PATH`.
fn launch_error(err: ApiError, task: Option<TaskId>) -> anyhow::Error {
    let hint = match err.code.as_deref() {
        Some("invalid_argv") => return anyhow::anyhow!(err.message),
        Some("unknown_task") => task.map(|task| format!("no task {task}; see `agentdeck task ls`")),
        Some("unknown_tab") => Some("the tab was deleted while launching; try again".to_owned()),
        Some("invalid_dir") => Some("--cwd must be an existing directory".to_owned()),
        _ => None,
    };
    match hint {
        Some(hint) => anyhow::anyhow!("{err}: {hint}"),
        None => err.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod launch_error {
        use super::*;

        fn api(code: &str) -> ApiError {
            ApiError {
                message: "refused".into(),
                code: Some(code.into()),
            }
        }

        #[test]
        fn should_say_only_the_daemons_message_when_the_agent_cant_start() {
            let refused = ApiError {
                message: "`codex` isn't on agentdeckd's PATH. Install it, or start agentdeckd \
                          from a shell where it's found."
                    .into(),
                code: Some("invalid_argv".into()),
            };

            let err = launch_error(refused, None).to_string();

            assert_eq!(
                err,
                "`codex` isn't on agentdeckd's PATH. Install it, or start agentdeckd from a \
                 shell where it's found."
            );
        }

        #[test]
        fn should_name_the_unknown_task() {
            let task: TaskId = "0190a1b2-c3d4-7e5f-8a9b-000000000005".parse().unwrap();

            let err = launch_error(api("unknown_task"), Some(task)).to_string();

            assert!(err.contains(&format!("no task {task}")), "{err}");
        }

        #[test]
        fn should_pass_other_errors_through() {
            let err = launch_error(api("unsupported_kind"), None).to_string();

            assert_eq!(err, "refused (unsupported_kind)");
        }
    }
}
