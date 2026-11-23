//! Human-readable output. `--json` prints the API's own objects instead.

use agentdeck_client::settings;
use std::collections::BTreeMap;

use agentdeck_client::types::{
    Pane, PrCloseResult, PrSyncStage, PrSyncStatus, PullRequest, RepositoryLinks, Settings,
    Workspace,
};
use agentdeck_core::{PaneKind, PrState, WorkspaceId};
use serde::Serialize;
use serde_json::Value;

/// Left-aligned columns two spaces apart, under a header row; `empty` when there are no rows.
fn table<const N: usize>(headers: [&str; N], rows: &[[String; N]], empty: &str) -> String {
    if rows.is_empty() {
        return format!("{empty}\n");
    }
    let mut widths = headers.map(|header| header.chars().count());
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: [&str; N]| {
        let mut line = String::new();
        for (index, cell) in cells.iter().enumerate() {
            if index + 1 == N {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  ", width = widths[index]));
            }
        }
        line.truncate(line.trim_end().len());
        line.push('\n');
        line
    };
    let mut out = line(headers);
    for row in rows {
        out.push_str(&line(row.each_ref().map(String::as_str)));
    }
    out
}

/// One workspace as `ws ls` lists it: the workspace and the Repositories it links. With
/// `--json` it is the workspace's own object plus `repositories`, their names (`owner/name`).
#[derive(Debug, Serialize)]
pub struct ListedWorkspace {
    /// The workspace, as the API gives it.
    #[serde(flatten)]
    pub workspace: Workspace,
    /// The names of the Repositories it links, in `GET /v1/repositories`' order.
    pub repositories: Vec<String>,
}

/// Each of `workspaces` with the names of the `repositories` (as `GET /v1/repositories` lists
/// them) that link it, in that order.
pub fn listed_workspaces(
    workspaces: Vec<Workspace>,
    repositories: &[RepositoryLinks],
) -> Vec<ListedWorkspace> {
    (workspaces.into_iter())
        .map(|workspace| ListedWorkspace {
            repositories: (repositories.iter())
                .filter(|repository| repository.workspaces.contains(&workspace.id))
                .map(|repository| repository.name.to_string())
                .collect(),
            workspace,
        })
        .collect()
}

/// `agentdeck ws ls`: a line per workspace under `ID  SLUG  NAME  REPOSITORIES`, an archived
/// one's name marked `(archived)`, then the names of the Repositories it links separated by
/// `, `, or `-` when it links none.
pub fn workspaces(workspaces: &[ListedWorkspace]) -> String {
    let rows: Vec<[String; 4]> = workspaces
        .iter()
        .map(|listed| {
            let ws = &listed.workspace;
            let name = match ws.archived_at {
                Some(_) => format!("{} (archived)", ws.name),
                None => ws.name.clone(),
            };
            let repositories = match listed.repositories.is_empty() {
                true => "-".to_owned(),
                false => listed.repositories.join(", "),
            };
            [ws.id.to_string(), ws.slug.clone(), name, repositories]
        })
        .collect();
    table(
        ["ID", "SLUG", "NAME", "REPOSITORIES"],
        &rows,
        "no workspaces",
    )
}

/// One workspace, after `ws new`: its id and slug.
pub fn workspace(ws: &Workspace) -> String {
    format!("{}  {}\n", ws.id, ws.slug)
}

/// A pane after `agent new`: its id, agent and dir, and its session once it has one. The
/// argv is in `--json`.
pub fn pane(pane: &Pane) -> String {
    let agent = pane.agent.map_or("shell", |agent| agent.as_str());
    let mut out = format!("{}\n  agent:   {agent}\n  cwd:     {}\n", pane.id, pane.cwd);
    if let Some(session) = &pane.session_id {
        out.push_str(&format!("  session: {session}\n"));
    }
    out
}

/// One pane as `agent ls` lists it: the pane, and the workspace and tab it is in. With
/// `--json` it is the pane's own object, its current `folder` included, plus `workspace_id`,
/// `workspace` (the slug) and `tab` (its title).
#[derive(Debug, Serialize)]
pub struct ListedPane {
    /// The pane, as the API gives it.
    #[serde(flatten)]
    pub pane: Pane,
    /// The workspace holding its tab.
    pub workspace_id: WorkspaceId,
    /// That workspace's slug.
    pub workspace: String,
    /// Its tab's title.
    pub tab: String,
}

/// What runs in `pane`: its agent; for a shell pane, `hosting <agent>` while it hosts one,
/// else `shell`.
fn pane_agent(pane: &Pane) -> String {
    match (pane.kind, pane.agent, pane.hosted_agent) {
        (PaneKind::Agent, Some(agent), _) => agent.to_string(),
        (PaneKind::Shell, _, Some(agent)) => format!("hosting {agent}"),
        (PaneKind::Agent, None, _) => "agent".to_owned(),
        (PaneKind::Shell, _, None) => "shell".to_owned(),
    }
}

/// `agentdeck agent ls`: a pane a line, with its current folder.
pub fn panes(panes: &[ListedPane]) -> String {
    let rows: Vec<[String; 6]> = panes
        .iter()
        .map(|listed| {
            let pane = &listed.pane;
            [
                pane.id.to_string(),
                listed.workspace.clone(),
                listed.tab.clone(),
                pane_agent(pane),
                pane.state.to_string(),
                pane.folder.clone(),
            ]
        })
        .collect();
    table(
        ["ID", "WORKSPACE", "TAB", "AGENT", "STATE", "FOLDER"],
        &rows,
        "no panes",
    )
}

/// `agentdeck pr sync` and `pr status`.
pub fn pr_status(status: &PrSyncStatus) -> String {
    let mut out = format!(
        "github:    {}\nlast sync: {}\n",
        status.login,
        status.last_synced_at.as_deref().unwrap_or("never"),
    );
    if let Some(login) = &status.synced_as {
        out.push_str(&format!("synced as: {login}\n"));
    }
    match &status.progress {
        Some(progress) if progress.stage == PrSyncStage::Details => {
            out.push_str("syncing:   PR details\n");
        }
        Some(progress) => out.push_str(&format!(
            "syncing:   {} of {} PRs\n",
            progress.prs_done, progress.prs
        )),
        None => {}
    }
    if let Some(error) = &status.last_error {
        out.push_str(&format!("last error: {error}\n"));
    }
    out
}

/// `n` PRs, as a count: "1 PR", "2 PRs".
fn prs(n: usize) -> String {
    if n == 1 {
        "1 PR".into()
    } else {
        format!("{n} PRs")
    }
}

/// What `pr close` asks before closing `n` PRs, whose repos are `repos` (one entry per PR
/// named): the question, a line per repo with its count, then `[y/N]`.
pub fn close_prompt(n: usize, repos: &[&str]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for repo in repos {
        *counts.entry(repo).or_default() += 1;
    }
    let width = counts.keys().map(|repo| repo.len()).max().unwrap_or(0);
    let mut out = format!(
        "Close {} on GitHub? Their repos' owners will see them closed. You can reopen a PR on \
         GitHub.\n",
        prs(n)
    );
    for (repo, count) in counts {
        out.push_str(&format!("  {repo:width$}  {}\n", prs(count)));
    }
    out.push_str("[y/N] ");
    out
}

/// `pr ls`: a line per pull request under `PR  STATE  TITLE`, the PR by its reference; "no
/// pull requests" when there are none.
pub fn pull_requests(prs: &[PullRequest]) -> String {
    let rows: Vec<[String; 3]> = prs
        .iter()
        .map(|pr| [pr.reference.to_string(), pr_state(pr), pr.title.clone()])
        .collect();
    table(["PR", "STATE", "TITLE"], &rows, "no pull requests")
}

/// A pull request's state as `pr ls` shows it: `open`, `draft`, `merged` or `closed`, then
/// `, unreachable` while the GitHub sign-in can't see it, as it is then as last synced.
fn pr_state(pr: &PullRequest) -> String {
    let state = match pr.state {
        PrState::Open if pr.draft => "draft",
        state => state.as_str(),
    };
    if pr.unreachable {
        format!("{state}, unreachable")
    } else {
        state.to_owned()
    }
}

/// `agentdeck repo ls`: a line per Repository under `REPOSITORY  WORKSPACES`, its name marked
/// `(unreachable)` while the GitHub sign-in can't see it, and the slugs of the Workspaces that
/// link it, oldest first, an archived one marked `(archived)`; `-` when none does.
pub fn repositories(repositories: &[RepositoryLinks], workspaces: &[Workspace]) -> String {
    let rows: Vec<[String; 2]> = repositories
        .iter()
        .map(|repository| {
            let name = if repository.unreachable {
                format!("{} (unreachable)", repository.name)
            } else {
                repository.name.to_string()
            };
            let linking: Vec<String> = repository
                .workspaces
                .iter()
                .map(|id| match workspaces.iter().find(|ws| ws.id == *id) {
                    Some(ws) if ws.archived_at.is_some() => format!("{} (archived)", ws.slug),
                    Some(ws) => ws.slug.clone(),
                    None => id.to_string(),
                })
                .collect();
            let linking = if linking.is_empty() {
                "-".to_owned()
            } else {
                linking.join(", ")
            };
            [name, linking]
        })
        .collect();
    table(["REPOSITORY", "WORKSPACES"], &rows, "no repositories")
}

/// `pr close`'s results: `closed <pr>` or `failed <pr>: <error>` per PR, by its reference,
/// then "Closed 12 of 14" when any failed.
pub fn close_results(results: &[PrCloseResult]) -> String {
    let mut out = String::new();
    for result in results {
        let name = &result.pr;
        if result.ok {
            out.push_str(&format!("closed {name}\n"));
        } else {
            let error = result.error.as_deref().unwrap_or("unknown error");
            out.push_str(&format!("failed {name}: {error}\n"));
        }
    }
    let closed = results.iter().filter(|result| result.ok).count();
    if closed < results.len() {
        out.push_str(&format!("Closed {closed} of {}\n", results.len()));
    }
    out
}

/// The settings, a key and its value per line; only `key`'s line when one is given. A value an
/// environment variable sets says so.
pub fn settings(all: &Settings, key: Option<&str>) -> String {
    let entries = settings::entries(all);
    let width = entries
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (name, value) in &entries {
        if key.is_some_and(|key| key != name) {
            continue;
        }
        let shown = if key.is_some() {
            format!("{name}  {}", setting_value(value))
        } else {
            format!("{name:<width$}  {}", setting_value(value))
        };
        out.push_str(&shown);
        if all.overridden_by_env.contains(name) {
            out.push_str("  (an environment variable overrides config.toml)");
        }
        out.push('\n');
    }
    out
}

/// A setting's value as typed on the command line: text without quotes.
pub fn setting_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::GhLogin;
    use agentdeck_core::Agent;

    use super::*;

    fn workspace_named(slug: &str, name: &str, archived: bool) -> Workspace {
        Workspace {
            archived_at: archived.then(|| "2026-02-01T00:00:00Z".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            id: "0190a1b2-c3d4-7e5f-8a9b-000000000001".parse().unwrap(),
            muted: false,
            name: name.into(),
            slug: slug.into(),
        }
    }

    mod table {
        use super::*;

        #[test]
        fn should_align_columns_under_their_headers() {
            let out = table(
                ["A", "LONGER"],
                &[["wide cell".into(), "x".into()], ["b".into(), "y".into()]],
                "none",
            );

            assert_eq!(out, "A          LONGER\nwide cell  x\nb          y\n");
        }

        #[test]
        fn should_say_so_when_empty() {
            assert_eq!(table(["A"], &[], "no things"), "no things\n");
        }
    }

    mod workspaces {
        use super::*;

        fn listed(slug: &str, repositories: &[&str]) -> ListedWorkspace {
            ListedWorkspace {
                workspace: workspace_named(slug, slug, false),
                repositories: repositories.iter().map(|r| (*r).to_owned()).collect(),
            }
        }

        #[test]
        fn should_mark_archived_workspaces() {
            let old = ListedWorkspace {
                workspace: workspace_named("old", "Old", true),
                repositories: Vec::new(),
            };

            let out = workspaces(&[old]);

            assert!(out.contains("Old (archived)"), "{out}");
        }

        #[test]
        fn should_list_each_workspaces_repositories_separated_by_commas() {
            let out = workspaces(&[
                listed("web", &["acme/gadgets", "acme/widgets"]),
                listed("api", &[]),
            ]);

            let id = "0190a1b2-c3d4-7e5f-8a9b-000000000001";
            let pad = " ".repeat(id.len() - 2);
            assert_eq!(
                out,
                format!(
                    "ID{pad}  SLUG  NAME  REPOSITORIES\n\
                     {id}  web   web   acme/gadgets, acme/widgets\n\
                     {id}  api   api   -\n"
                )
            );
        }

        #[test]
        fn should_add_the_repositories_to_the_workspaces_json() {
            let json = serde_json::to_value(listed("web", &["acme/widgets"])).unwrap();

            assert_eq!(json["slug"], "web");
            assert_eq!(json["repositories"], serde_json::json!(["acme/widgets"]));
            assert_eq!(json.get("dir"), None);
        }
    }

    mod listed_workspaces {
        use super::*;
        use agentdeck_client::types::RepositoryLinks;

        fn links(name: &str, workspaces: &[WorkspaceId]) -> RepositoryLinks {
            RepositoryLinks {
                id: agentdeck_core::RepositoryId::from(1),
                name: name.parse().unwrap(),
                url: format!("https://github.com/{name}"),
                private: false,
                archived: false,
                unreachable: false,
                workspaces: workspaces.to_vec(),
            }
        }

        #[test]
        fn should_give_each_workspace_the_names_of_the_repositories_it_links_in_their_order() {
            let web = workspace_named("web", "web", false);
            let mut api = workspace_named("api", "api", false);
            api.id = "0190a1b2-c3d4-7e5f-8a9b-000000000002".parse().unwrap();
            let repositories = [
                links("acme/gadgets", &[web.id]),
                links("acme/tools", &[]),
                links("acme/widgets", &[api.id, web.id]),
            ];

            let listed = listed_workspaces(vec![web, api], &repositories);

            assert_eq!(listed[0].repositories, ["acme/gadgets", "acme/widgets"]);
            assert_eq!(listed[1].repositories, ["acme/widgets"]);
        }
    }

    mod panes {
        use agentdeck_core::{AgentState, PaneKind};

        use super::*;

        const PANE_1: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000011";
        const PANE_2: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000012";
        const PANE_3: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000013";

        fn listed(id: &str, kind: PaneKind, tab: &str) -> ListedPane {
            ListedPane {
                pane: Pane {
                    agent: None,
                    argv: vec!["sh".into()],
                    created_at: "2026-01-01T00:00:00Z".into(),
                    cwd: "/work/web".into(),
                    exit_code: None,
                    folder: "/work/web".into(),
                    hosted_agent: None,
                    id: id.parse().unwrap(),
                    kind,
                    session_id: None,
                    state: AgentState::Idle,
                    tab_id: "0190a1b2-c3d4-7e5f-8a9b-000000000030".parse().unwrap(),
                    title: String::new(),
                },
                workspace_id: "0190a1b2-c3d4-7e5f-8a9b-000000000001".parse().unwrap(),
                workspace: "web".into(),
                tab: tab.into(),
            }
        }

        #[test]
        fn should_list_id_workspace_tab_agent_state_and_folder() {
            let mut agent = listed(PANE_1, PaneKind::Agent, "agents");
            agent.pane.agent = Some(Agent::Claude);
            agent.pane.state = AgentState::Blocked;
            let mut hosting = listed(PANE_2, PaneKind::Shell, "shell");
            hosting.pane.folder = "/work/web/app".into();
            hosting.pane.hosted_agent = Some(Agent::Codex);
            hosting.pane.state = AgentState::Working;
            let mut exited = listed(PANE_3, PaneKind::Shell, "shell");
            exited.pane.state = AgentState::Exited;

            let out = panes(&[agent, hosting, exited]);

            assert_eq!(
                out,
                format!(
                    "ID{pad}  WORKSPACE  TAB     AGENT          STATE    FOLDER\n\
                     {PANE_1}  web        agents  claude         blocked  /work/web\n\
                     {PANE_2}  web        shell   hosting codex  working  /work/web/app\n\
                     {PANE_3}  web        shell   shell          exited   /work/web\n",
                    pad = " ".repeat(PANE_1.len() - 2)
                )
            );
        }

        #[test]
        fn should_say_so_when_no_pane_runs() {
            assert_eq!(panes(&[]), "no panes\n");
        }
    }

    mod pr_status {
        use super::*;

        #[test]
        fn should_say_never_before_the_first_sync() {
            let out = pr_status(&PrSyncStatus {
                last_error: Some("gh is not logged in".into()),
                last_synced_at: None,
                login: GhLogin::LoggedOut,
                max_age_days: 90,
                progress: None,
                synced_as: None,
            });

            assert_eq!(
                out,
                "github:    logged_out\nlast sync: never\nlast error: gh is not logged in\n"
            );
        }

        #[test]
        fn should_name_the_login_prs_last_synced_as() {
            let out = pr_status(&PrSyncStatus {
                last_error: None,
                last_synced_at: Some("2026-11-14T10:00:00.000Z".into()),
                login: GhLogin::LoggedOut,
                max_age_days: 90,
                progress: None,
                synced_as: Some("octocat".into()),
            });

            assert_eq!(
                out,
                "github:    logged_out\nlast sync: 2026-11-14T10:00:00.000Z\nsynced as: octocat\n"
            );
        }
    }

    mod pr_status_syncing {
        use agentdeck_client::types::{PrSyncProgress, PrSyncStage};

        use super::*;

        #[test]
        fn should_say_how_far_a_running_sync_is() {
            let out = pr_status(&PrSyncStatus {
                last_error: None,
                last_synced_at: Some("2026-11-14T10:00:00.000Z".into()),
                login: GhLogin::LoggedIn,
                max_age_days: 90,
                progress: Some(PrSyncProgress {
                    prs_done: 100,
                    prs: 271,
                    stage: PrSyncStage::Listing,
                }),
                synced_as: None,
            });

            assert_eq!(
                out,
                "github:    logged_in\nlast sync: 2026-11-14T10:00:00.000Z\nsyncing:   100 of 271 PRs\n"
            );
        }
    }

    mod pr_status_details {
        use agentdeck_client::types::{PrSyncProgress, PrSyncStage};

        use super::*;

        #[test]
        fn should_say_it_is_fetching_details() {
            let out = pr_status(&PrSyncStatus {
                last_error: None,
                last_synced_at: None,
                login: GhLogin::LoggedIn,
                max_age_days: 90,
                progress: Some(PrSyncProgress {
                    prs_done: 271,
                    prs: 271,
                    stage: PrSyncStage::Details,
                }),
                synced_as: None,
            });

            assert!(out.contains("syncing:   PR details\n"), "{out}");
        }
    }

    mod close_prompt {
        use super::*;

        #[test]
        fn should_ask_with_a_line_per_repo_and_its_count() {
            assert_eq!(
                close_prompt(3, &["acme/widgets", "acme/api", "acme/widgets"]),
                "Close 3 PRs on GitHub? Their repos' owners will see them closed. You can \
                 reopen a PR on GitHub.\n  acme/api      1 PR\n  acme/widgets  2 PRs\n[y/N] "
            );
        }

        #[test]
        fn should_say_1_pr() {
            assert!(close_prompt(1, &["acme/api"]).starts_with("Close 1 PR on GitHub?"));
        }
    }

    mod close_results {
        use super::*;

        fn result(pr: &str, error: Option<&str>) -> PrCloseResult {
            PrCloseResult {
                pr: pr.parse().expect("a PR reference"),
                ok: error.is_none(),
                error: error.map(str::to_owned),
            }
        }

        #[test]
        fn should_say_what_closed_and_what_failed_then_how_many_closed() {
            let results = [
                result("acme/api#7", None),
                result("acme/api#8", Some("Not allowed")),
                result("acme/web#3", Some("Not an open PR")),
            ];

            assert_eq!(
                close_results(&results),
                "closed acme/api#7\nfailed acme/api#8: Not allowed\nfailed acme/web#3: Not an \
                 open PR\nClosed 1 of 3\n"
            );
        }

        #[test]
        fn should_not_count_when_every_pr_closed() {
            let results = [result("acme/api#7", None)];

            assert_eq!(close_results(&results), "closed acme/api#7\n");
        }
    }

    mod pull_requests {
        use agentdeck_client::types::Repository;

        use super::*;

        fn pr(reference: &str, title: &str) -> PullRequest {
            let reference: agentdeck_core::PrRef = reference.parse().expect("a PR reference");
            PullRequest {
                author: Some("octocat".into()),
                base_ref: "main".into(),
                checks: None,
                draft: false,
                head_ref: "feat".into(),
                number: reference.number(),
                reference: reference.clone(),
                repository: Repository {
                    archived: false,
                    id: 1_296_269.into(),
                    name: reference.repository().clone(),
                    private: false,
                    url: format!("https://github.com/{}", reference.repository()),
                },
                review: None,
                state: PrState::Open,
                title: title.into(),
                unreachable: false,
                finished: false,
                updated_at: String::new(),
                url: format!(
                    "https://github.com/{}/pull/{}",
                    reference.repository(),
                    reference.number()
                ),
            }
        }

        #[test]
        fn should_list_each_pr_by_its_reference_with_its_state_and_title() {
            let draft = PullRequest {
                draft: true,
                ..pr("acme/api#12", "Add the gateway")
            };
            let merged = PullRequest {
                state: PrState::Merged,
                ..pr("acme/web#3", "Fix the build")
            };

            assert_eq!(
                pull_requests(&[pr("acme/api#7", "Sync PRs"), draft, merged]),
                "PR           STATE   TITLE\n\
                 acme/api#7   open    Sync PRs\n\
                 acme/api#12  draft   Add the gateway\n\
                 acme/web#3   merged  Fix the build\n"
            );
        }

        #[test]
        fn should_mark_a_pr_the_sign_in_cannot_see() {
            let hidden = PullRequest {
                unreachable: true,
                ..pr("acme/secret#8", "Rotate keys")
            };

            assert!(
                pull_requests(&[hidden]).contains("acme/secret#8  open, unreachable  Rotate keys"),
            );
        }

        #[test]
        fn should_say_when_there_are_none() {
            assert_eq!(pull_requests(&[]), "no pull requests\n");
        }
    }

    mod repositories {
        use super::*;

        fn repository(name: &str, workspaces: &[WorkspaceId]) -> RepositoryLinks {
            RepositoryLinks {
                archived: false,
                id: 1_296_269.into(),
                name: name.parse().expect("a Repository"),
                private: false,
                unreachable: false,
                url: format!("https://github.com/{name}"),
                workspaces: workspaces.to_vec(),
            }
        }

        fn ws(slug: &str, id: &str, archived: bool) -> Workspace {
            Workspace {
                id: id.parse().unwrap(),
                ..workspace_named(slug, slug, archived)
            }
        }

        const WEB: &str = "0190a1b2-c3d4-7e5f-8a9b-00000000000a";
        const API: &str = "0190a1b2-c3d4-7e5f-8a9b-00000000000b";

        #[test]
        fn should_list_each_repository_with_the_slugs_of_the_workspaces_linking_it() {
            let workspaces = [ws("web", WEB, false), ws("api", API, true)];
            let both = [WEB.parse().unwrap(), API.parse().unwrap()];
            let hidden = RepositoryLinks {
                unreachable: true,
                ..repository("acme/secret", &[])
            };

            assert_eq!(
                repositories(&[repository("acme/widgets", &both), hidden], &workspaces),
                "REPOSITORY                 WORKSPACES\n\
                 acme/widgets               web, api (archived)\n\
                 acme/secret (unreachable)  -\n"
            );
        }

        #[test]
        fn should_say_when_there_are_none() {
            assert_eq!(repositories(&[], &[]), "no repositories\n");
        }
    }
}
