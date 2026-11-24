//! Suggestions: `GET /v1/workspaces/{id}/suggestions` and `changed suggestions` (spec §3, §4,
//! §5), with Panes running `sh` in temp git repos whose remotes name GitHub Repositories, and
//! httpmock as GitHub's REST API.

use std::path::{Path, PathBuf};

use agentdeck_client::Client;
use agentdeck_client::types::{
    InputWhen, NewLink, NewPane, NewTab, NewWorkspace, Pane, PaneInput, Repository, Workspace,
};
use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{AgentState, PaneKind, RepositoryId, TabId, WorkspaceId};
use httpmock::MockServer;

use crate::support::git::git;
use crate::support::github::{
    RestRepo, mock_rest_repo, mock_rest_repo_missing, mock_viewer, set_gh_token,
};
use crate::support::terminal::{eventually, in_state, sh};
use crate::support::ws::{Frame, Ws};
use crate::support::{TestDaemon, error_response, set_daemon_env};

const TOKEN: &str = "gho_test";
const WIDGETS: &str = "acme/widgets";
const GADGETS: &str = "acme/gadgets";

/// A daemon whose `gh` prints [`TOKEN`] and whose GitHub is `github`.
async fn signed_in(github: &MockServer) -> TestDaemon {
    mock_viewer(github, TOKEN, "octocat", None);
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_gh_token(dirs.path(), Some(TOKEN));
    TestDaemon::start_with_github(dirs, &github.url("/graphql")).await
}

/// A git repo `name` under the daemon's temp root with these remotes, by name and URL.
fn repo(daemon: &TestDaemon, name: &str, remotes: &[(&str, &str)]) -> PathBuf {
    let dir = daemon.new_dir(name);
    git(&dir, &["init", "--quiet"]);
    for (remote, url) in remotes {
        git(&dir, &["remote", "add", remote, url]);
    }
    dir
}

/// A Workspace `name` with one tab, for Panes to go in.
async fn workspace(client: &Client, name: &str) -> (Workspace, TabId) {
    let workspace = client
        .workspace_create()
        .body(NewWorkspace {
            name: name.into(),
            link: Vec::new(),
        })
        .send()
        .await
        .expect("create a workspace")
        .into_inner();
    let tab = client
        .tab_create()
        .id(workspace.id)
        .body(NewTab {
            title: "tab".into(),
            layout: None,
        })
        .send()
        .await
        .expect("create a tab")
        .into_inner();
    (workspace, tab.id)
}

/// A shell Pane in `tab` running `argv` in `dir`.
async fn spawn(client: &Client, tab: TabId, dir: &Path, argv: &[&str]) -> Pane {
    client
        .pane_create()
        .body(NewPane {
            tab_id: tab,
            kind: PaneKind::Shell,
            agent: None,
            argv: Some(argv.iter().map(|arg| (*arg).to_owned()).collect()),
            cwd: dir.to_str().expect("a UTF-8 path").to_owned(),
        })
        .send()
        .await
        .expect("spawn a pane")
        .into_inner()
}

/// A Pane in `tab` that starts in `dir` and stays.
async fn working_in(client: &Client, tab: TabId, dir: &Path) -> Pane {
    spawn(client, tab, dir, &["sleep", "600"]).await
}

async fn suggestions(client: &Client, workspace: WorkspaceId) -> Vec<Repository> {
    let response = client.suggestion_list().id(workspace).send().await;
    response.expect("list Suggestions").into_inner()
}

async fn names(client: &Client, workspace: WorkspaceId) -> Vec<String> {
    (suggestions(client, workspace).await.iter())
        .map(|r| r.name.to_string())
        .collect()
}

/// Waits until `workspace`'s Suggestions are `wanted`, by name.
async fn until_names(client: &Client, workspace: WorkspaceId, wanted: &[&str]) {
    eventually(&format!("Suggestions {wanted:?}"), || async {
        (names(client, workspace).await == wanted).then_some(())
    })
    .await;
}

/// The Repository GitHub shows for `repo`, as a Suggestion carries it.
fn suggested(repo: RestRepo) -> Repository {
    Repository {
        id: RepositoryId::from(repo.id),
        name: repo.name.parse().unwrap(),
        url: repo.url(),
        private: repo.private,
        archived: false,
    }
}

/// Waits for `changed suggestions` naming `workspace`.
async fn changed_suggestions(ws: &mut Ws, workspace: WorkspaceId) {
    ws.until(|frame| match frame {
        Frame::Message(ServerMessage::Changed {
            entity: Entity::Suggestions,
            id,
        }) if id == workspace.to_string() => Some(()),
        _ => None,
    })
    .await;
}

/// Links `workspace` to `repo` by GitHub's id, as the GUI does, so it works signed out.
async fn link_held(client: &Client, workspace: WorkspaceId, repo: RestRepo) {
    client
        .link_create()
        .id(workspace)
        .body(NewLink {
            repository: repo.name.parse().unwrap(),
            id: Some(RepositoryId::from(repo.id)),
            url: Some(repo.url()),
            private: Some(repo.private),
            archived: Some(false),
        })
        .send()
        .await
        .expect("link");
}

mod get_v1_workspaces_suggestions {
    use super::*;

    mod remotes {
        use super::*;

        #[tokio::test]
        async fn should_suggest_the_upstream_remote_when_it_is_on_github_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let dir = repo(
                &daemon,
                "fork",
                &[
                    ("origin", "git@github.com:octocat/widgets.git"),
                    ("upstream", "https://github.com/acme/widgets.git"),
                ],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(
                suggestions(&daemon.client, web.id).await,
                [suggested(RestRepo::new(WIDGETS))]
            );
        }

        #[tokio::test]
        async fn should_take_origin_when_upstream_is_not_on_github_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let dir = repo(
                &daemon,
                "mirror",
                &[
                    ("origin", "git@github.com:acme/widgets.git"),
                    ("upstream", "https://gitlab.com/acme/widgets.git"),
                ],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }

        #[tokio::test]
        async fn should_ignore_every_other_remote_200() {
            let github = MockServer::start_async().await;
            let lookups = mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let dir = repo(
                &daemon,
                "other",
                &[("fork", "https://github.com/acme/widgets.git")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, Vec::<String>::new());
            assert_eq!(lookups.calls(), 0);
        }

        #[tokio::test]
        async fn should_suggest_nothing_for_a_folder_outside_a_repo_200() {
            let daemon = TestDaemon::start().await;
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &daemon.new_dir("plain")).await;

            assert_eq!(names(&daemon.client, web.id).await, Vec::<String>::new());
        }

        #[tokio::test]
        async fn should_suggest_each_repository_once_by_name_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            mock_rest_repo(&github, TOKEN, GADGETS, RestRepo::new(GADGETS));
            let daemon = signed_in(&github).await;
            let widgets = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let gadgets = repo(
                &daemon,
                "gadgets",
                &[("origin", "https://github.com/acme/gadgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &widgets).await;
            working_in(&daemon.client, tab, &gadgets).await;
            working_in(&daemon.client, tab, &widgets.join(".git")).await;

            assert_eq!(names(&daemon.client, web.id).await, [GADGETS, WIDGETS]);
        }
    }

    /// Remotes git sees only through its config files beyond the repo's own: includes,
    /// `includeIf` and `insteadOf`. Each fixture is checked with `git remote get-url` too, so
    /// it means to git what the test says it does. Without a `git` that answers, the repo's
    /// own `config` file still does.
    mod git_config {
        use super::*;

        fn write(path: &Path, text: &str) {
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("making its dir");
            std::fs::write(path, text).expect("writing a config file");
        }

        /// The daemon's global git config, `$XDG_CONFIG_HOME/git/config`.
        fn global_config(daemon: &TestDaemon) -> PathBuf {
            daemon.root().join("config/git/config")
        }

        /// The URL git gives `remote` of the repo at `dir`, with the daemon's home and config.
        fn git_url(daemon: &TestDaemon, dir: &Path, remote: &str) -> String {
            let output = std::process::Command::new("git")
                .args(["remote", "get-url", remote])
                .current_dir(dir)
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", daemon.home())
                .env("XDG_CONFIG_HOME", daemon.root().join("config"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("running git");
            String::from_utf8(output.stdout)
                .expect("a UTF-8 URL")
                .trim()
                .to_owned()
        }

        /// [`signed_in`], with the `PATH` that `path` makes from the daemon's temp root.
        async fn signed_in_on_path(
            github: &MockServer,
            path: impl FnOnce(&Path) -> String,
        ) -> TestDaemon {
            mock_viewer(github, TOKEN, "octocat", None);
            let dirs = tempfile::tempdir().expect("a temp dir");
            set_gh_token(dirs.path(), Some(TOKEN));
            let path = path(dirs.path());
            set_daemon_env(dirs.path(), &[("PATH", &path)]);
            TestDaemon::start_with_github(dirs, &github.url("/graphql")).await
        }

        /// `dir` holding links to the programs a Pane and the stub `gh` run, and no `git`.
        fn without_git(dir: &Path) -> PathBuf {
            std::fs::create_dir_all(dir).expect("making the dir");
            for program in ["sh", "sleep", "cat", "dirname"] {
                let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                    .map(|entry| entry.join(program))
                    .find(|path| path.is_file())
                    .unwrap_or_else(|| panic!("{program} on PATH"));
                std::os::unix::fs::symlink(found, dir.join(program)).expect("linking it");
            }
            dir.to_owned()
        }

        #[tokio::test]
        async fn should_read_the_repos_own_config_without_git_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in_on_path(&github, |root| {
                let no_git = without_git(&root.join("no-git"));
                format!("{}:{}", root.join("bin").display(), no_git.display())
            })
            .await;
            assert!(!daemon.root().join("no-git/git").exists());
            let dir = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }

        #[tokio::test]
        async fn should_read_the_repos_own_config_when_git_hangs_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in_on_path(&github, |root| {
                let hang = root.join("hang");
                write(&hang.join("git"), "#!/bin/sh\nexec sleep 30\n");
                std::fs::set_permissions(
                    hang.join("git"),
                    std::os::unix::fs::PermissionsExt::from_mode(0o755),
                )
                .expect("making the stub git executable");
                format!(
                    "{}:{}:{}",
                    root.join("bin").display(),
                    hang.display(),
                    std::env::var("PATH").unwrap_or_default()
                )
            })
            .await;
            let dir = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }

        #[tokio::test]
        async fn should_follow_an_insteadof_rewrite_in_the_global_config_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            write(
                &global_config(&daemon),
                "[url \"https://github.com/\"]\n\tinsteadOf = gh:\n",
            );
            let dir = repo(&daemon, "widgets", &[("origin", "gh:acme/widgets")]);
            assert_eq!(
                git_url(&daemon, &dir, "origin"),
                "https://github.com/acme/widgets"
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }

        #[tokio::test]
        async fn should_follow_an_insteadof_rewrite_in_the_repos_config_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let dir = repo(&daemon, "widgets", &[("upstream", "work:acme/widgets")]);
            git(&dir, &["config", "url.git@github.com:.insteadOf", "work:"]);
            assert_eq!(
                git_url(&daemon, &dir, "upstream"),
                "git@github.com:acme/widgets"
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }

        #[tokio::test]
        async fn should_read_a_remote_from_an_included_file_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let dir = repo(&daemon, "widgets", &[]);
            write(
                &dir.join(".git/remotes.inc"),
                "[remote \"origin\"]\n\turl = https://github.com/acme/widgets\n",
            );
            git(&dir, &["config", "include.path", "remotes.inc"]);
            assert_eq!(
                git_url(&daemon, &dir, "origin"),
                "https://github.com/acme/widgets"
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }

        #[tokio::test]
        async fn should_follow_an_include_if_only_for_a_repo_its_gitdir_matches_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let work = daemon.root().join("dirs/work");
            write(
                &global_config(&daemon),
                &format!(
                    "[includeIf \"gitdir:{}/\"]\n\tpath = work.inc\n",
                    work.display()
                ),
            );
            write(
                &daemon.root().join("config/git/work.inc"),
                "[url \"https://github.com/acme/\"]\n\tinsteadOf = acme:\n",
            );
            let inside = repo(&daemon, "work/widgets", &[("origin", "acme:widgets")]);
            let outside = repo(&daemon, "play/widgets", &[("origin", "acme:widgets")]);
            assert_eq!(
                git_url(&daemon, &inside, "origin"),
                "https://github.com/acme/widgets"
            );
            assert_eq!(git_url(&daemon, &outside, "origin"), "acme:widgets");
            let (web, web_tab) = workspace(&daemon.client, "web").await;
            let (play, play_tab) = workspace(&daemon.client, "play").await;

            working_in(&daemon.client, web_tab, &inside).await;
            working_in(&daemon.client, play_tab, &outside).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
            assert_eq!(names(&daemon.client, play.id).await, Vec::<String>::new());
        }

        #[tokio::test]
        async fn should_follow_a_worktree_to_the_main_repos_rewritten_remote_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            write(
                &global_config(&daemon),
                "[url \"https://github.com/\"]\n\tinsteadOf = gh:\n",
            );
            let main = daemon.new_dir("main");
            crate::support::git::init_with_commit(&main);
            git(&main, &["remote", "add", "origin", "gh:acme/widgets"]);
            let tree = daemon.root().join("dirs/tree");
            let tree_arg = tree.to_str().expect("a UTF-8 path");
            git(&main, &["worktree", "add", "--quiet", "-b", "wt", tree_arg]);
            assert_eq!(
                git_url(&daemon, &tree, "origin"),
                "https://github.com/acme/widgets"
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &tree).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }
    }

    mod folders {
        use super::*;

        #[tokio::test]
        async fn should_follow_a_pane_that_moves_into_a_repo_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let dir = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;
            let script = format!("cd '{}' && echo moved && exec sleep 600", dir.display());

            spawn(&daemon.client, tab, &daemon.new_dir("plain"), &sh(&script)).await;

            until_names(&daemon.client, web.id, &[WIDGETS]).await;
        }

        #[tokio::test]
        async fn should_keep_an_exited_panes_last_known_folder_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let dir = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;
            let script = format!("cd '{}' && echo moved && read line", dir.display());
            let pane = spawn(&daemon.client, tab, &daemon.new_dir("plain"), &sh(&script)).await;
            until_names(&daemon.client, web.id, &[WIDGETS]).await;

            daemon
                .client
                .pane_input()
                .id(pane.id)
                .body(PaneInput {
                    text: "\r".into(),
                    when: InputWhen::Now,
                    submit: false,
                })
                .send()
                .await
                .expect("type into the pane");
            in_state(&daemon.client, pane.id, AgentState::Exited).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }
    }

    mod github {
        use super::*;

        #[tokio::test]
        async fn should_never_suggest_a_repository_github_cannot_see_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo_missing(&github, WIDGETS);
            let daemon = signed_in(&github).await;
            let dir = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, Vec::<String>::new());
        }

        #[tokio::test]
        async fn should_suggest_a_renamed_repository_under_the_name_github_gives_200() {
            let github = MockServer::start_async().await;
            let renamed = RestRepo::new("acme/widgets-v2");
            mock_rest_repo(&github, TOKEN, WIDGETS, renamed);
            let daemon = signed_in(&github).await;
            let dir = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(
                suggestions(&daemon.client, web.id).await,
                [suggested(renamed)]
            );
        }

        #[tokio::test]
        async fn should_ask_github_about_each_name_once_per_run_200() {
            let github = MockServer::start_async().await;
            let lookups = mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let missing = mock_rest_repo_missing(&github, GADGETS);
            let daemon = signed_in(&github).await;
            let widgets = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let gadgets = repo(
                &daemon,
                "gadgets",
                &[("origin", "https://github.com/acme/gadgets")],
            );
            let (web, web_tab) = workspace(&daemon.client, "web").await;
            let (api, api_tab) = workspace(&daemon.client, "api").await;
            working_in(&daemon.client, web_tab, &widgets).await;
            working_in(&daemon.client, web_tab, &gadgets).await;
            working_in(&daemon.client, api_tab, &widgets).await;

            for _ in 0..3 {
                assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
                assert_eq!(names(&daemon.client, api.id).await, [WIDGETS]);
            }

            assert_eq!(lookups.calls(), 1, "acme/widgets asked about once");
            assert_eq!(missing.calls(), 1, "acme/gadgets asked about once");
        }
    }

    mod signed_out {
        use super::*;

        #[tokio::test]
        async fn should_suggest_only_repositories_agentdeck_keeps_200() {
            let daemon = TestDaemon::start().await;
            let kept = RestRepo::new(WIDGETS);
            let (other, _) = workspace(&daemon.client, "other").await;
            link_held(&daemon.client, other.id, kept).await;
            let widgets = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let gadgets = repo(
                &daemon,
                "gadgets",
                &[("origin", "https://github.com/acme/gadgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &widgets).await;
            working_in(&daemon.client, tab, &gadgets).await;

            assert_eq!(suggestions(&daemon.client, web.id).await, [suggested(kept)]);
        }
    }

    mod links {
        use super::*;

        #[tokio::test]
        async fn should_suggest_a_repository_other_workspaces_link_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let (api, _) = workspace(&daemon.client, "api").await;
            link_held(&daemon.client, api.id, RestRepo::new(WIDGETS)).await;
            let dir = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;

            working_in(&daemon.client, tab, &dir).await;

            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
        }

        #[tokio::test]
        async fn should_leave_out_what_the_workspace_links_200() {
            let github = MockServer::start_async().await;
            mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
            let daemon = signed_in(&github).await;
            let dir = repo(
                &daemon,
                "widgets",
                &[("origin", "https://github.com/acme/widgets")],
            );
            let (web, tab) = workspace(&daemon.client, "web").await;
            working_in(&daemon.client, tab, &dir).await;
            assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);

            link_held(&daemon.client, web.id, RestRepo::new(WIDGETS)).await;

            assert_eq!(names(&daemon.client, web.id).await, Vec::<String>::new());
        }
    }

    #[tokio::test]
    async fn should_give_an_archived_workspace_none_200() {
        let daemon = TestDaemon::start().await;
        let kept = RestRepo::new(WIDGETS);
        let (other, _) = workspace(&daemon.client, "other").await;
        link_held(&daemon.client, other.id, kept).await;
        let dir = repo(
            &daemon,
            "widgets",
            &[("origin", "https://github.com/acme/widgets")],
        );
        let (web, tab) = workspace(&daemon.client, "web").await;
        let pane = spawn(&daemon.client, tab, &dir, &["true"]).await;
        in_state(&daemon.client, pane.id, AgentState::Exited).await;
        assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);

        daemon
            .client
            .workspace_archive()
            .id(web.id)
            .send()
            .await
            .expect("archive");

        assert_eq!(names(&daemon.client, web.id).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn should_not_find_a_workspace_that_does_not_exist_404() {
        let daemon = TestDaemon::start().await;
        let missing: WorkspaceId = "019a0000-0000-7000-8000-000000000001".parse().unwrap();

        let (status, body) =
            error_response(daemon.client.suggestion_list().id(missing).send().await);

        assert_eq!(status, 404);
        assert_eq!(body.error_code.as_deref(), Some("not_found"));
    }
}

mod changed_suggestions {
    use super::*;

    #[tokio::test]
    async fn should_be_sent_when_a_pane_opens_in_a_repo() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = signed_in(&github).await;
        let dir = repo(
            &daemon,
            "widgets",
            &[("origin", "https://github.com/acme/widgets")],
        );
        let (web, tab) = workspace(&daemon.client, "web").await;
        let mut ws = Ws::connect(&daemon).await;

        working_in(&daemon.client, tab, &dir).await;

        changed_suggestions(&mut ws, web.id).await;
    }

    #[tokio::test]
    async fn should_be_sent_when_a_pane_moves_into_a_repo() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = signed_in(&github).await;
        let dir = repo(
            &daemon,
            "widgets",
            &[("origin", "https://github.com/acme/widgets")],
        );
        let (web, tab) = workspace(&daemon.client, "web").await;
        let mut ws = Ws::connect(&daemon).await;
        let script = format!(
            "sleep 1 && cd '{}' && echo moved && exec sleep 600",
            dir.display()
        );

        spawn(&daemon.client, tab, &daemon.new_dir("plain"), &sh(&script)).await;

        changed_suggestions(&mut ws, web.id).await;
        assert_eq!(names(&daemon.client, web.id).await, [WIDGETS]);
    }

    #[tokio::test]
    async fn should_be_sent_when_a_link_takes_a_suggestion_away() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = signed_in(&github).await;
        let dir = repo(
            &daemon,
            "widgets",
            &[("origin", "https://github.com/acme/widgets")],
        );
        let (web, tab) = workspace(&daemon.client, "web").await;
        let mut ws = Ws::connect(&daemon).await;
        working_in(&daemon.client, tab, &dir).await;
        changed_suggestions(&mut ws, web.id).await;

        link_held(&daemon.client, web.id, RestRepo::new(WIDGETS)).await;

        changed_suggestions(&mut ws, web.id).await;
        assert_eq!(names(&daemon.client, web.id).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn should_be_sent_when_the_last_pane_in_a_repo_is_deleted() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = signed_in(&github).await;
        let dir = repo(
            &daemon,
            "widgets",
            &[("origin", "https://github.com/acme/widgets")],
        );
        let (web, tab) = workspace(&daemon.client, "web").await;
        let mut ws = Ws::connect(&daemon).await;
        let pane = working_in(&daemon.client, tab, &dir).await;
        changed_suggestions(&mut ws, web.id).await;

        daemon
            .client
            .pane_delete()
            .id(pane.id)
            .send()
            .await
            .expect("delete the pane");

        changed_suggestions(&mut ws, web.id).await;
        assert_eq!(names(&daemon.client, web.id).await, Vec::<String>::new());
    }
}
