//! `agentdeck agent new`: launch a `claude` or `codex` pane in a workspace (spec §6, §10).
//! The daemon builds the argv and the launch prompt; the CLI picks the tab and places the pane
//! in its layout, as every client does.

use std::path::Path;

use agentdeck_client::Client;
use agentdeck_client::types::{NewPane, NewTab, Pane, Tab, TabPatch};
use agentdeck_core::{Agent, Layout, PaneKind, TabId, TaskId};

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
/// new tab of its own.
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
    let tabs = client
        .tab_list()
        .id(ws_id)
        .send()
        .await
        .map_err(ApiError::from)?;
    let (tab, created) = match empty_tab(&tabs) {
        Some(tab) => (tab, false),
        None => {
            let tab = client
                .tab_create()
                .id(ws_id)
                .body(NewTab {
                    layout: None,
                    title: launch.agent.to_string(),
                })
                .send()
                .await
                .map_err(ApiError::from)?;
            (tab.id, true)
        }
    };
    let spawned = client
        .pane_create()
        .body(NewPane {
            agent: Some(launch.agent),
            argv: None,
            cwd,
            kind: PaneKind::Agent,
            tab_id: tab,
            task_id: launch.task,
        })
        .send()
        .await;
    let pane = match spawned {
        Ok(pane) => pane.into_inner(),
        Err(err) => {
            if created {
                // Don't leave the tab made for this agent behind, empty.
                let _ = client.tab_delete().id(tab).send().await;
            }
            return Err(launch_error(ApiError::from(err), launch.agent, launch.task));
        }
    };
    client
        .tab_update()
        .id(tab)
        .body(TabPatch {
            layout: Some(Layout::single(pane.id)),
            ..TabPatch::default()
        })
        .send()
        .await
        .map_err(ApiError::from)?;
    Ok(pane)
}

/// The first tab with no panes, which a new agent can fill.
fn empty_tab(tabs: &[Tab]) -> Option<TabId> {
    tabs.iter()
        .find(|tab| tab.layout.is_none())
        .map(|tab| tab.id)
}

/// The daemon's refusal, with what to do about the ones a person can fix.
fn launch_error(err: ApiError, agent: Agent, task: Option<TaskId>) -> anyhow::Error {
    let hint = match err.code.as_deref() {
        Some("invalid_argv") => Some(format!("is `{agent}` installed and on agentdeckd's PATH?")),
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
    use agentdeck_core::PaneId;

    use super::*;

    fn tab(id: u8, layout: Option<Layout>) -> Tab {
        Tab {
            id: format!("0190a1b2-c3d4-7e5f-8a9b-0000000000{id:02}")
                .parse()
                .unwrap(),
            layout,
            position: i64::from(id),
            title: "t".into(),
            workspace_id: "0190a1b2-c3d4-7e5f-8a9b-000000000099".parse().unwrap(),
        }
    }

    fn pane() -> Layout {
        let pane: PaneId = "0190a1b2-c3d4-7e5f-8a9b-000000000077".parse().unwrap();
        Layout::single(pane)
    }

    mod empty_tab {
        use super::*;

        #[test]
        fn should_pick_the_first_tab_without_panes() {
            let tabs = [tab(1, Some(pane())), tab(2, None), tab(3, None)];

            assert_eq!(empty_tab(&tabs), Some(tabs[1].id));
        }

        #[test]
        fn should_be_none_when_every_tab_has_panes() {
            assert_eq!(empty_tab(&[tab(1, Some(pane()))]), None);
            assert_eq!(empty_tab(&[]), None);
        }
    }

    mod launch_error {
        use super::*;

        fn api(code: &str) -> ApiError {
            ApiError {
                message: "refused".into(),
                code: Some(code.into()),
            }
        }

        #[test]
        fn should_ask_whether_the_agent_is_installed_when_it_cant_start() {
            let err = launch_error(api("invalid_argv"), Agent::Codex, None).to_string();

            assert_eq!(
                err,
                "refused (invalid_argv): is `codex` installed and on agentdeckd's PATH?"
            );
        }

        #[test]
        fn should_name_the_unknown_task() {
            let task: TaskId = "0190a1b2-c3d4-7e5f-8a9b-000000000005".parse().unwrap();

            let err = launch_error(api("unknown_task"), Agent::Claude, Some(task)).to_string();

            assert!(err.contains(&format!("no task {task}")), "{err}");
        }

        #[test]
        fn should_pass_other_errors_through() {
            let err = launch_error(api("unsupported_kind"), Agent::Claude, None).to_string();

            assert_eq!(err, "refused (unsupported_kind)");
        }
    }
}
