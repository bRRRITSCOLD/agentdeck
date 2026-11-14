//! Panes: `/v1/panes` and `/v1/panes/{id}` (spec §4, §6), with `sh` in real PTYs.

use std::collections::BTreeMap;
use std::time::Duration;

use agentdeck_client::types::{InputWhen, PaneInput};
use agentdeck_core::{Agent, AgentState, PaneKind};

use crate::support::terminal::{
    Place, alive, daemon_with_shell_rules, in_state, printed, sh, tail,
};
use crate::support::{TestDaemon, error_response, set_daemon_env};

/// Types `text` into the pane.
async fn input(
    daemon: &TestDaemon,
    pane: agentdeck_core::PaneId,
    text: &str,
    when: InputWhen,
) -> bool {
    daemon
        .client
        .pane_input()
        .id(pane)
        .body(PaneInput {
            text: text.into(),
            when,
        })
        .send()
        .await
        .expect("send input")
        .into_inner()
        .queued
}

/// The pid a pane printed as `pid:<n>`.
fn pid(text: &str) -> u32 {
    let start = text.find("pid:").expect("a printed pid") + 4;
    text[start..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse().ok())
        .expect("a pid")
}

mod post_v1_panes {
    use super::*;

    #[tokio::test]
    async fn should_start_a_shell_in_the_tab_201() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;

        let response = daemon
            .client
            .pane_create()
            .body(place.new_pane(&sh("pwd; sleep 60")))
            .send()
            .await
            .expect("spawn");

        assert_eq!(response.status().as_u16(), 201);
        let pane = response.into_inner();
        assert_eq!(pane.tab_id, place.tab.id);
        assert_eq!(pane.kind, PaneKind::Shell);
        assert_eq!(pane.argv, ["sh", "-c", "pwd; sleep 60"]);
        assert_eq!(pane.exit_code, None);
        printed(&daemon.client, pane.id, &place.workspace.dir).await;
    }

    #[tokio::test]
    async fn should_reject_an_unknown_tab_400() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let mut new = place.new_pane(&["sh"]);
        new.tab_id = uuid::Uuid::now_v7().into();

        let (status, body) = error_response(daemon.client.pane_create().body(new).send().await);

        assert_eq!(status, 400);
        assert_eq!(body.error_code.as_deref(), Some("unknown_tab"));
    }

    #[tokio::test]
    async fn should_reject_a_cwd_that_is_not_a_dir_400() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let mut new = place.new_pane(&["sh"]);
        new.cwd = "/no/such/dir".into();

        let (status, body) = error_response(daemon.client.pane_create().body(new).send().await);

        assert_eq!(status, 400);
        assert_eq!(body.error_code.as_deref(), Some("invalid_dir"));
    }

    #[tokio::test]
    async fn should_reject_a_program_that_does_not_exist_400() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;

        let (status, body) = error_response(
            daemon
                .client
                .pane_create()
                .body(place.new_pane(&["agentdeck-no-such-program"]))
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(body.error_code.as_deref(), Some("invalid_argv"));
        assert_eq!(
            body.message,
            "`agentdeck-no-such-program` isn't on agentdeckd's PATH. Install it, or start \
             agentdeckd from a shell where it's found."
        );
    }

    #[tokio::test]
    async fn should_keep_the_spawn_error_for_a_program_that_is_not_executable_400() {
        use std::os::unix::fs::PermissionsExt as _;

        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let program = std::path::Path::new(&place.workspace.dir).join("not-executable");
        std::fs::write(&program, "#!/bin/sh\n").expect("writing the program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o644))
            .expect("making it not executable");

        let (status, body) = error_response(
            daemon
                .client
                .pane_create()
                .body(place.new_pane(&["./not-executable"]))
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(body.error_code.as_deref(), Some("invalid_argv"));
        assert!(
            body.message
                .contains("not-executable because it is not executable"),
            "{}",
            body.message
        );
    }

    #[tokio::test]
    async fn should_run_an_other_agents_argv_201() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let mut new = place.new_pane(&sh("echo other-$((2+2)); sleep 60"));
        new.kind = PaneKind::Agent;
        new.agent = Some(Agent::Other);

        let pane = daemon
            .client
            .pane_create()
            .body(new)
            .send()
            .await
            .expect("spawn")
            .into_inner();

        assert_eq!(pane.agent, Some(Agent::Other));
        assert_eq!(pane.session_id, None);
        printed(&daemon.client, pane.id, "other-4").await;
    }

    mod environment {
        use super::*;

        /// What `sh` itself adds to the environment it runs `env` in.
        const SHELL_ADDS: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

        fn allowlisted(name: &str) -> bool {
            [
                "PATH",
                "HOME",
                "USER",
                "SHELL",
                "LANG",
                "TERM",
                "COLORTERM",
                "SSH_AUTH_SOCK",
                "EDITOR",
                "CODEX_HOME",
                "AGENTDECK_PANE",
                "AGENTDECK_WORKSPACE",
            ]
            .contains(&name)
                || ["LC_", "XDG_", "GH_"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        }

        #[tokio::test]
        async fn should_pass_on_only_the_allowlist_201() {
            let dirs = tempfile::tempdir().expect("a temp dir");
            set_daemon_env(
                dirs.path(),
                &[
                    ("CLAUDECODE", "1"),
                    ("CLAUDE_CODE_ENTRYPOINT", "cli"),
                    ("AWS_SECRET_ACCESS_KEY", "secret"),
                    ("LC_ALL", "C"),
                    ("GH_HOST", "github.com"),
                    ("EDITOR", "vi"),
                    ("TERM", "dumb"),
                ],
            );
            let daemon = TestDaemon::start_in(dirs).await;
            let place = Place::new(&daemon).await;

            let pane = place.spawn(&daemon.client, &sh("env; echo env-done")).await;
            let text = printed(&daemon.client, pane.id, "env-done").await;

            let env: BTreeMap<&str, &str> = text
                .lines()
                .filter_map(|line| line.trim_end_matches('\r').split_once('='))
                .collect();
            let leaked: Vec<&str> = env
                .keys()
                .copied()
                .filter(|name| !allowlisted(name) && !SHELL_ADDS.contains(name))
                .collect();
            assert_eq!(
                leaked,
                Vec::<&str>::new(),
                "variables outside the allowlist"
            );
            assert_eq!(env.get("LC_ALL"), Some(&"C"));
            assert_eq!(env.get("GH_HOST"), Some(&"github.com"));
            assert_eq!(env.get("EDITOR"), Some(&"vi"));
            assert_eq!(env.get("TERM"), Some(&"xterm-256color"));
            assert_eq!(env.get("COLORTERM"), Some(&"truecolor"));
            assert_eq!(
                env.get("AGENTDECK_PANE"),
                Some(&pane.id.to_string().as_str())
            );
            let workspace = place.workspace.id.to_string();
            assert_eq!(env.get("AGENTDECK_WORKSPACE"), Some(&workspace.as_str()));
        }
    }
}

mod get_v1_panes_id {
    use super::*;

    #[tokio::test]
    async fn should_show_the_live_title_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place
            .spawn(&daemon.client, &sh("printf '\\033]0;hello\\007'; sleep 60"))
            .await;

        crate::support::terminal::eventually("the title", || async {
            let pane = daemon.client.pane_get().id(pane.id).send().await.ok()?;
            (pane.into_inner().title == "hello").then_some(())
        })
        .await;
    }

    #[tokio::test]
    async fn should_record_the_exit_code_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("exit 3")).await;

        let exited = in_state(&daemon.client, pane.id, AgentState::Exited).await;

        assert_eq!(exited.exit_code, Some(3));
    }

    #[tokio::test]
    async fn should_answer_404_for_an_unknown_pane_404() {
        let daemon = TestDaemon::start().await;

        let (status, body) = error_response(
            daemon
                .client
                .pane_get()
                .id(agentdeck_core::PaneId::from(uuid::Uuid::now_v7()))
                .send()
                .await,
        );

        assert_eq!(status, 404);
        assert_eq!(body.error_code.as_deref(), Some("not_found"));
    }
}

mod patch_v1_panes_id {
    use agentdeck_client::types::{NewTask, NewWorkspace, PanePatch, Task, TaskPatch};
    use agentdeck_core::ws::{Entity, ServerMessage};
    use agentdeck_core::{PaneId, TaskId, TaskStatus, WorkspaceId};

    use super::*;
    use crate::support::ws::Ws;

    async fn task(daemon: &TestDaemon, workspace: Option<WorkspaceId>) -> Task {
        daemon
            .client
            .task_create()
            .body(NewTask {
                title: "t".into(),
                body: None,
                status: None,
                workspace_id: workspace,
            })
            .send()
            .await
            .expect("create a task")
            .into_inner()
    }

    async fn link(
        daemon: &TestDaemon,
        pane: PaneId,
        task: Option<TaskId>,
    ) -> Result<
        agentdeck_client::ResponseValue<agentdeck_client::types::Pane>,
        agentdeck_client::Error<agentdeck_client::types::Error>,
    > {
        daemon
            .client
            .pane_update()
            .id(pane)
            .body(PanePatch { task_id: task })
            .send()
            .await
    }

    #[tokio::test]
    async fn should_link_the_pane_to_a_task_in_its_workspace_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        let task = task(&daemon, Some(place.workspace.id)).await;

        let response = link(&daemon, pane.id, Some(task.id)).await.expect("link");

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.into_inner().task_id, Some(task.id));
        let read = daemon
            .client
            .pane_get()
            .id(pane.id)
            .send()
            .await
            .expect("read");
        assert_eq!(read.into_inner().task_id, Some(task.id));
    }

    #[tokio::test]
    async fn should_unlink_it_given_null_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon, Some(place.workspace.id)).await;
        let mut new = place.new_pane(&sh("sleep 60"));
        new.task_id = Some(task.id);
        let pane = daemon
            .client
            .pane_create()
            .body(new)
            .send()
            .await
            .expect("spawn");

        let unlinked = link(&daemon, pane.id, None).await.expect("unlink");

        assert_eq!(unlinked.into_inner().task_id, None);
    }

    #[tokio::test]
    async fn should_send_changed_for_the_pane_and_its_old_and_new_task_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        let (old, new) = (
            task(&daemon, Some(place.workspace.id)).await,
            task(&daemon, Some(place.workspace.id)).await,
        );
        link(&daemon, pane.id, Some(old.id)).await.expect("link");
        let mut ws = Ws::connect(&daemon).await;

        link(&daemon, pane.id, Some(new.id)).await.expect("relink");

        for (entity, id) in [
            (Entity::Pane, pane.id.to_string()),
            (Entity::Task, old.id.to_string()),
            (Entity::Task, new.id.to_string()),
        ] {
            ws.expect(ServerMessage::Changed { entity, id }).await;
        }
    }

    #[tokio::test]
    async fn should_reject_an_unknown_task_400() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;

        let (status, body) =
            error_response(link(&daemon, pane.id, Some(uuid::Uuid::now_v7().into())).await);

        assert_eq!(status, 400);
        assert_eq!(body.error_code.as_deref(), Some("unknown_task"));
    }

    #[tokio::test]
    async fn should_reject_a_task_in_another_workspace_or_the_inbox_400() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        let other = daemon
            .client
            .workspace_create()
            .body(NewWorkspace {
                name: "other".into(),
                dir: None,
            })
            .send()
            .await
            .expect("create a workspace");

        for workspace in [Some(other.id), None] {
            let task = task(&daemon, workspace).await;

            let (status, body) = error_response(link(&daemon, pane.id, Some(task.id)).await);

            assert_eq!(status, 400);
            assert_eq!(body.error_code.as_deref(), Some("invalid_task"));
        }
    }

    #[tokio::test]
    async fn should_reject_a_done_task_400() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        let task = task(&daemon, Some(place.workspace.id)).await;
        daemon
            .client
            .task_update()
            .id(task.id)
            .body(TaskPatch {
                status: Some(TaskStatus::Done),
                ..TaskPatch::default()
            })
            .send()
            .await
            .expect("finish the task");

        let (status, body) = error_response(link(&daemon, pane.id, Some(task.id)).await);

        assert_eq!(status, 400);
        assert_eq!(body.error_code.as_deref(), Some("invalid_task"));
    }

    #[tokio::test]
    async fn should_answer_404_for_an_unknown_pane_404() {
        let daemon = TestDaemon::start().await;

        let (status, body) = error_response(link(&daemon, uuid::Uuid::now_v7().into(), None).await);

        assert_eq!(status, 404);
        assert_eq!(body.error_code.as_deref(), Some("not_found"));
    }
}

mod delete_v1_panes_id {
    use super::*;

    #[tokio::test]
    async fn should_kill_the_process_and_delete_the_pane_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place
            .spawn(&daemon.client, &sh("echo pid:$$; exec sleep 600"))
            .await;
        let pid = pid(&printed(&daemon.client, pane.id, "pid:").await);
        assert!(alive(pid));

        let response = daemon
            .client
            .pane_delete()
            .id(pane.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(response.status().as_u16(), 204);
        crate::support::terminal::eventually("the process to die", || async {
            (!alive(pid)).then_some(())
        })
        .await;
        let (status, _) = error_response(daemon.client.pane_get().id(pane.id).send().await);
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn should_kill_a_process_that_ignores_sighup_after_3_s_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place
            .spawn(
                &daemon.client,
                &sh("trap '' HUP; echo pid:$$; while :; do sleep 0.1; done"),
            )
            .await;
        let pid = pid(&printed(&daemon.client, pane.id, "pid:").await);

        daemon
            .client
            .pane_delete()
            .id(pane.id)
            .send()
            .await
            .expect("delete");

        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(alive(pid), "SIGHUP alone shouldn't stop it");
        crate::support::terminal::eventually("SIGKILL", || async { (!alive(pid)).then_some(()) })
            .await;
    }

    #[tokio::test]
    async fn should_kill_the_panes_of_a_deleted_tab_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place
            .spawn(&daemon.client, &sh("echo pid:$$; exec sleep 600"))
            .await;
        let pid = pid(&printed(&daemon.client, pane.id, "pid:").await);

        daemon
            .client
            .tab_delete()
            .id(place.tab.id)
            .send()
            .await
            .expect("delete the tab");

        crate::support::terminal::eventually("the process to die", || async {
            (!alive(pid)).then_some(())
        })
        .await;
    }
}

mod post_v1_panes_id_input {
    use super::*;

    #[tokio::test]
    async fn should_type_the_text_now_202() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &["cat"]).await;

        let queued = input(&daemon, pane.id, "typed text\r", InputWhen::Now).await;

        assert!(!queued);
        printed(&daemon.client, pane.id, "typed text").await;
    }

    #[tokio::test]
    async fn should_hold_idle_input_until_the_pane_is_idle_202() {
        let daemon = daemon_with_shell_rules().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &["sh"]).await;

        let queued = input(&daemon, pane.id, "echo queued-$((6*7))\r", InputWhen::Idle).await;
        assert!(queued);
        // Something typed after it shows first: the queued text is still held.
        input(&daemon, pane.id, "echo now-$((1+1))\r", InputWhen::Now).await;
        let before = printed(&daemon.client, pane.id, "now-2").await;
        assert!(!before.contains("queued-"));

        input(
            &daemon,
            pane.id,
            "printf '\\033]0;idle\\007'\r",
            InputWhen::Now,
        )
        .await;

        printed(&daemon.client, pane.id, "queued-42").await;
    }

    #[tokio::test]
    async fn should_type_idle_input_at_once_into_an_idle_pane_202() {
        let daemon = daemon_with_shell_rules().await;
        let place = Place::new(&daemon).await;
        let pane = place
            .spawn(&daemon.client, &sh("printf '\\033]0;idle\\007'; exec cat"))
            .await;
        in_state(&daemon.client, pane.id, AgentState::Idle).await;

        let queued = input(&daemon, pane.id, "straight in\r", InputWhen::Idle).await;

        assert!(!queued);
        printed(&daemon.client, pane.id, "straight in").await;
    }

    #[tokio::test]
    async fn should_refuse_input_once_the_process_exited_409() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &["true"]).await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;

        let (status, body) = error_response(
            daemon
                .client
                .pane_input()
                .id(pane.id)
                .body(PaneInput {
                    text: "x".into(),
                    when: InputWhen::Now,
                })
                .send()
                .await,
        );

        assert_eq!(status, 409);
        assert_eq!(body.error_code.as_deref(), Some("exited"));
    }
}

mod get_v1_panes_id_tail {
    use super::*;

    #[tokio::test]
    async fn should_return_the_last_bytes_of_output_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("printf 0123456789")).await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;

        assert_eq!(tail(&daemon.client, pane.id, Some(4)).await, b"6789");
        assert_eq!(tail(&daemon.client, pane.id, None).await, b"0123456789");
    }
}

mod terminal_queries {
    use super::*;

    #[tokio::test]
    async fn should_be_answered_by_the_daemon_with_no_client_attached() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;

        // Asks for the cursor position (DSR 6) and prints the answer with ESC spelled `E`.
        let pane = place
            .spawn(
                &daemon.client,
                &sh(
                    "stty raw -echo; printf '\\033[6n'; r=$(dd bs=1 count=6 2>/dev/null); \
                     stty sane; printf '\\nanswer:%s\\n' \"$r\" | tr '\\033' E",
                ),
            )
            .await;

        printed(&daemon.client, pane.id, "answer:E[1;1R").await;
    }
}
