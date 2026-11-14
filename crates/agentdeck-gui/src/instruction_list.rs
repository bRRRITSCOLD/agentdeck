//! The instructions the right panel lists (spec §9, §11), from the sidebar's [`Snapshot`]:
//! those for a workspace, newest first, each with the line under it, and the agent panes each
//! can be sent to.

use agentdeck_client::types::Instruction;
use agentdeck_core::{AgentState, InstructionId, PaneId, WorkspaceId};
use chrono::{DateTime, Utc};
use gpui::SharedString;

use crate::new_agent::AGENTS;
use crate::overview::{Snapshot, ago};

/// One instruction, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionRow {
    /// The instruction.
    pub id: InstructionId,
    /// Its text.
    pub body: SharedString,
    /// How old it is: `added 2 h ago`.
    pub meta: SharedString,
}

/// `workspace`'s instructions, newest first, at `now`.
pub fn instructions(
    snapshot: &Snapshot,
    workspace: WorkspaceId,
    now: DateTime<Utc>,
) -> Vec<InstructionRow> {
    let mut picked: Vec<&Instruction> = (snapshot.instructions.iter())
        .filter(|instruction| instruction.workspace_id == workspace)
        .collect();
    // RFC 3339 UTC stamps sort as text.
    picked.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    picked
        .into_iter()
        .map(|instruction| InstructionRow {
            id: instruction.id,
            body: instruction.body.clone().into(),
            meta: meta(instruction, now).into(),
        })
        .collect()
}

/// A pane an instruction can be sent to, from its "Send to agent" menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendTarget {
    /// The pane.
    pub pane: PaneId,
    /// What its item reads: the agent, ` · `, then the linked task's title, else the tab's.
    pub label: SharedString,
}

/// The live agent panes in `instruction`'s workspace, in the snapshot's order: what its
/// "Send to agent" menu offers.
pub fn send_targets(snapshot: &Snapshot, instruction: InstructionId) -> Vec<SendTarget> {
    let Some(instruction) = (snapshot.instructions.iter()).find(|i| i.id == instruction) else {
        return Vec::new();
    };
    let task_title = |task| {
        (snapshot.tasks.iter())
            .find(|t| t.id == task)
            .map(|t| t.title.as_str())
    };
    (snapshot.panes.iter())
        .filter(|p| p.workspace == instruction.workspace_id && p.state != AgentState::Exited)
        .filter_map(|p| {
            let agent = AGENTS.iter().find(|(a, _)| Some(*a) == p.agent)?.1;
            let on = p.task.and_then(task_title).unwrap_or(&p.tab);
            Some(SendTarget {
                pane: p.pane,
                label: format!("{agent} · {on}").into(),
            })
        })
        .collect()
}

/// `added 2 h ago`, or `added` when the time doesn't parse.
fn meta(instruction: &Instruction, now: DateTime<Utc>) -> String {
    match instruction.created_at.parse::<DateTime<Utc>>() {
        Ok(created) => format!("added {}", ago(now - created)),
        Err(_) => "added".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::Task;
    use agentdeck_core::{Agent, TaskKind};

    use super::*;
    use crate::overview::PaneState;
    use crate::overview::fixtures::{id, instruction, pane, task, workspace};

    const NOW: &str = "2026-11-13T12:00:00Z";

    fn now() -> DateTime<Utc> {
        NOW.parse().unwrap()
    }

    fn at(instruction: Instruction, created_at: &str) -> Instruction {
        Instruction {
            created_at: created_at.into(),
            ..instruction
        }
    }

    #[test]
    fn should_list_only_the_workspaces_instructions_newest_first() {
        let (a, b) = (workspace(1, "a"), workspace(2, "b"));
        let snapshot = Snapshot {
            instructions: vec![
                at(instruction(20, a.id), "2026-11-13T09:00:00Z"),
                at(instruction(21, a.id), "2026-11-13T11:00:00Z"),
                instruction(22, b.id),
            ],
            workspaces: vec![a.clone(), b],
            ..Snapshot::default()
        };

        let ids: Vec<InstructionId> = instructions(&snapshot, a.id, now())
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, [id(21), id(20)]);
    }

    #[test]
    fn should_say_how_long_ago_it_was_added() {
        let a = workspace(1, "a");
        let snapshot = Snapshot {
            instructions: vec![at(instruction(20, a.id), "2026-11-13T10:00:00Z")],
            workspaces: vec![a.clone()],
            ..Snapshot::default()
        };

        assert_eq!(
            instructions(&snapshot, a.id, now())[0].meta,
            "added 2 h ago"
        );
    }

    mod send_targets {
        use super::*;

        fn agent(n: u32, ws: WorkspaceId, agent: Agent, task: Option<u32>) -> PaneState {
            PaneState {
                agent: Some(agent),
                task: task.map(id),
                tab: format!("tab {n}"),
                ..pane(n, ws, AgentState::Working)
            }
        }

        fn labels(snapshot: &Snapshot, instruction: u32) -> Vec<(PaneId, String)> {
            (send_targets(snapshot, id(instruction)).into_iter())
                .map(|t| (t.pane, t.label.to_string()))
                .collect()
        }

        #[test]
        fn should_offer_each_live_agent_in_the_workspace_by_task_or_tab() {
            let (a, b) = (workspace(1, "a"), workspace(2, "b"));
            let linked = Task {
                title: "Tabs: arrow-key navigation".into(),
                ..task(10, TaskKind::Plain, Some(a.id))
            };
            let exited = PaneState {
                state: AgentState::Exited,
                ..agent(33, a.id, Agent::Claude, None)
            };
            let snapshot = Snapshot {
                instructions: vec![instruction(20, a.id)],
                panes: vec![
                    agent(30, a.id, Agent::Claude, Some(10)),
                    pane(31, a.id, AgentState::Idle),
                    agent(32, a.id, Agent::Codex, None),
                    exited,
                    agent(34, b.id, Agent::Claude, None),
                ],
                tasks: vec![linked],
                workspaces: vec![a, b],
                ..Snapshot::default()
            };

            assert_eq!(
                labels(&snapshot, 20),
                [
                    (
                        id(30),
                        "Claude Code · Tabs: arrow-key navigation".to_owned()
                    ),
                    (id(32), "Codex · tab 32".to_owned()),
                ]
            );
        }

        #[test]
        fn should_offer_none_for_an_instruction_it_doesnt_hold() {
            let a = workspace(1, "a");
            let snapshot = Snapshot {
                panes: vec![agent(30, a.id, Agent::Claude, None)],
                workspaces: vec![a],
                ..Snapshot::default()
            };

            assert_eq!(labels(&snapshot, 20), []);
        }
    }
}
