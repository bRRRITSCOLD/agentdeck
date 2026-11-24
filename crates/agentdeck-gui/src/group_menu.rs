//! A Repository group's `…` menu (spec §11, Pull requests view → Group menu), Penpot
//! "Screens · Main" → "Pull requests · group menu", "· group menu · no workspace to link",
//! "Pull requests · linked", "Workspace · Pull requests · group menu" and "Workspace · Pull
//! requests · unlinked": **Open on GitHub**, **Link to a workspace** and, on a Workspace's Pull
//! requests, **Unlink from this workspace**.
//!
//! Link to a workspace is a submenu of the Workspaces that aren't archived and don't link the
//! Repository, in the sidebar's order, then **New workspace**, which opens New workspace with
//! the Repository already picked ([`NewWorkspaceLinking`]); with no Workspace to offer, New
//! workspace is its only item.
//!
//! Linking and unlinking apply at once, with a toast that offers Undo, as archiving does. Undo
//! links again by GitHub's id with what the view held of the Repository
//! ([`Api::link_repository`]), so it works even when the unlink deleted the Repository, signed
//! out included. Nothing here waits for the view: a toast's Undo outlives it.

use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::types::Repository;
use agentdeck_core::{PrState, RepositoryId};
use brrritscold_core_gpui::{
    Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuAlign, DropdownMenuContent,
    DropdownMenuItem, DropdownMenuSeparator, DropdownMenuSubContent, DropdownMenuSubTrigger,
    IconName,
};
use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Task,
    Window, div,
};

use crate::api::Api;
use crate::gui_state;
use crate::notice::{Notice, NoticeAction};
use crate::overview::Snapshot;
use crate::pr_table::Linker;
use crate::pull_requests::Scope;
use crate::workspace::failed;

/// A group's **New workspace** asks for New workspace with this Repository already picked.
#[derive(Debug, Clone, PartialEq)]
pub struct NewWorkspaceLinking(pub Repository);

/// One entry of a group's `…` menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupEntry {
    /// **Open on GitHub**: the Repository's page.
    OpenOnGitHub,
    /// **Link to a workspace**: a submenu of these Workspaces, then New workspace.
    LinkToWorkspace(Vec<Linker>),
    /// A line between the entries.
    Separator,
    /// **Unlink from this workspace**, on a Workspace's Pull requests.
    Unlink,
}

/// The `…` menu of `repository`'s group on a list over `scope`, top to bottom: Open on GitHub,
/// Link to a workspace with the Workspaces it offers (not archived, not linking `repository`,
/// in the sidebar's order), then, on a Workspace's Pull requests, a separator and Unlink from
/// this workspace.
pub fn entries(snapshot: &Snapshot, scope: Scope, repository: RepositoryId) -> Vec<GroupEntry> {
    let eligible = (snapshot.workspaces.iter())
        .filter(|w| w.archived_at.is_none() && !snapshot.links(w.id, repository))
        .map(|w| Linker {
            id: w.id,
            name: w.name.clone().into(),
        })
        .collect();
    let mut entries = vec![
        GroupEntry::OpenOnGitHub,
        GroupEntry::LinkToWorkspace(eligible),
    ];
    if let Scope::Workspace(_) = scope {
        entries.extend([GroupEntry::Separator, GroupEntry::Unlink]);
    }
    entries
}

/// Repository `repository` as `snapshot` holds it: what a link by GitHub's id carries, so
/// Undo can bring it back once an unlink deleted it.
pub fn held(snapshot: &Snapshot, repository: RepositoryId) -> Option<Repository> {
    let linked = (snapshot.repositories.iter())
        .find(|r| r.id == repository)
        .map(|r| Repository {
            id: r.id,
            name: r.name.clone(),
            url: r.url.clone(),
            private: r.private,
            archived: r.archived,
        });
    linked.or_else(|| {
        (snapshot.prs.iter())
            .find(|pr| pr.repository.id == repository)
            .map(|pr| pr.repository.clone())
    })
}

/// How many of `repository`'s PRs are open, counted as the sidebar counts them: drafts in,
/// Unreachable and Finished ones out.
pub fn open_prs(snapshot: &Snapshot, repository: RepositoryId) -> usize {
    (snapshot.prs.iter())
        .filter(|pr| pr.repository.id == repository)
        .filter(|pr| pr.state == PrState::Open && !pr.unreachable && !pr.finished)
        .count()
}

/// The toast after a link: `Linked owner/name to <workspace>`.
pub fn linked_title(repository: &str, workspace: &str) -> String {
    format!("Linked {repository} to {workspace}")
}

/// The toast after an unlink: its title, `Unlinked owner/name from <workspace>`, and what it
/// says of the Repository's `open` PRs: they no longer show here, and, when no other Workspace
/// links it (`elsewhere` false), where they are now. With no open PR it says only the title.
pub fn unlinked(
    repository: &str,
    workspace: &str,
    open: usize,
    elsewhere: bool,
) -> (String, Option<String>) {
    let title = format!("Unlinked {repository} from {workspace}");
    let description = match open {
        0 => None,
        1 => Some("Its 1 open PR no longer shows here.".to_owned()),
        n => Some(format!("Its {n} open PRs no longer show here.")),
    };
    let description = description.map(|gone| match (elsewhere, open) {
        (true, _) => gone,
        (false, 1) => {
            format!("{gone} It's in the Pull requests view, under Not in a workspace.")
        }
        (false, _) => {
            format!("{gone} They're in the Pull requests view, under Not in a workspace.")
        }
    });
    (title, description)
}

/// What the menu acts on: the Repository, as the view holds it, the list's scope and what it
/// needs to say about an unlink.
#[derive(Clone)]
pub struct Target {
    /// The Repository.
    pub repository: Repository,
    /// The Workspace whose Pull requests it is on, if any.
    pub workspace: Option<Linker>,
    /// Its open PRs, as the sidebar counts them.
    pub open: usize,
    /// Whether a Workspace other than `workspace` links it, archived ones aside.
    pub elsewhere: bool,
}

/// What New workspace does: asks the window for New workspace with the Repository picked.
pub type OnNewWorkspace = Rc<dyn Fn(Repository, &mut Window, &mut App)>;

/// The group's `…` menu of `entries` over `target`, opening 4px below its ghost icon-xs
/// trigger with their right edges lined up, as a row's does. Tests find it as
/// `group menu <name>`.
pub fn menu(
    entries: Vec<GroupEntry>,
    target: Target,
    api: Arc<dyn Api>,
    on_new_workspace: OnNewWorkspace,
) -> AnyElement {
    let key = target.repository.name.to_string();
    let content =
        entries
            .into_iter()
            .fold(DropdownMenuContent::new(), |content, entry| match entry {
                GroupEntry::OpenOnGitHub => {
                    let url = target.repository.url.clone();
                    content.item(DropdownMenuItem::new("Open on GitHub").on_click(
                        move |_, window, cx| crate::open_pr::open_repository(&url, window, cx),
                    ))
                }
                GroupEntry::LinkToWorkspace(eligible) => {
                    let sub = eligible
                        .iter()
                        .fold(DropdownMenuSubContent::new(), |sub, w| {
                            let (api, repository, workspace) =
                                (api.clone(), target.repository.clone(), w.clone());
                            sub.item(DropdownMenuItem::new(w.name.clone()).on_click(
                                move |_, window, cx| {
                                    link(
                                        api.clone(),
                                        workspace.clone(),
                                        repository.clone(),
                                        |_, _| {},
                                        window,
                                        cx,
                                    );
                                },
                            ))
                        });
                    let sub = match eligible.is_empty() {
                        true => sub,
                        false => sub.separator(DropdownMenuSeparator::new()),
                    };
                    let (repository, on_new) =
                        (target.repository.clone(), on_new_workspace.clone());
                    let sub = sub.item(
                        DropdownMenuItem::new("New workspace")
                            .on_click(move |_, window, cx| on_new(repository.clone(), window, cx)),
                    );
                    content.sub(DropdownMenuSubTrigger::new("Link to a workspace"), sub)
                }
                GroupEntry::Separator => content.separator(DropdownMenuSeparator::new()),
                GroupEntry::Unlink => {
                    let (api, target) = (api.clone(), target.clone());
                    content.item(
                        DropdownMenuItem::new("Unlink from this workspace").on_click(
                            move |_, window, cx| unlink(api.clone(), target.clone(), window, cx),
                        ),
                    )
                }
            });
    let selector = key.clone();
    div()
        .debug_selector(move || format!("group menu {selector}"))
        .child(
            // Penpot "Pull requests · group menu" ends it on the trigger's right edge.
            DropdownMenu::new(SharedString::from(format!("group-menu-{key}")))
                .align(DropdownMenuAlign::End)
                .trigger(
                    Button::new(SharedString::from(format!("group-menu-trigger-{key}")))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::IconXs)
                        .icon(IconName::Ellipsis),
                )
                .content(content),
        )
        .into_any_element()
}

/// Links `workspace` to `repository` at once and toasts it, with an Undo that unlinks it;
/// `linked` runs once it is linked. Undo doesn't dismiss a Suggestion: it puts things back as
/// they were.
pub fn link(
    api: Arc<dyn Api>,
    workspace: Linker,
    repository: Repository,
    linked: impl FnOnce(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let linking = api.link_repository(workspace.id, repository.clone(), cx);
    after(linking, window, cx, move |done, window, cx| {
        let name = repository.name.to_string();
        match done {
            Ok(()) => {
                linked(window, cx);
                let undo = NoticeAction::new("Undo", {
                    let (workspace, id) = (workspace.clone(), repository.id);
                    move |window, cx| {
                        let unlinking = api.unlink_repository(workspace.id, id, cx);
                        let what = format!("Couldn't unlink {name} from {}", workspace.name);
                        quietly(unlinking, what, window, cx);
                    }
                });
                Notice::new(linked_title(&repository.name.to_string(), &workspace.name))
                    .action(undo)
                    .show(window, cx);
            }
            Err(err) => failed(
                format!("Couldn't link {name} to {}", workspace.name),
                &err,
                window,
                cx,
            ),
        }
    });
}

/// Unlinks the Workspace whose Pull requests `target` is on from its Repository at once, and
/// toasts it with an Undo that links it again by GitHub's id. The unlink counts as Not now:
/// the Workspace's Suggestions leave the Repository out from then on.
pub fn unlink(api: Arc<dyn Api>, target: Target, window: &mut Window, cx: &mut App) {
    let Some(workspace) = target.workspace.clone() else {
        return;
    };
    let unlinking = api.unlink_repository(workspace.id, target.repository.id, cx);
    after(unlinking, window, cx, move |done, window, cx| {
        let name = target.repository.name.to_string();
        match done {
            Ok(()) => {
                let (id, repository) = (workspace.id, target.repository.id);
                gui_state::update(cx, |state| state.dismiss(id, repository));
                let undo = NoticeAction::new("Undo", {
                    let (workspace, repository) = (workspace.clone(), target.repository.clone());
                    move |window, cx| {
                        let linking = api.link_repository(workspace.id, repository.clone(), cx);
                        let what = format!("Couldn't link {name} to {}", workspace.name);
                        quietly(linking, what, window, cx);
                    }
                });
                let (title, description) = unlinked(
                    &target.repository.name.to_string(),
                    &workspace.name,
                    target.open,
                    target.elsewhere,
                );
                let notice = Notice::new(title).action(undo);
                match description {
                    Some(description) => notice.description(description),
                    None => notice,
                }
                .show(window, cx);
            }
            Err(err) => failed(
                format!("Couldn't unlink {name} from {}", workspace.name),
                &err,
                window,
                cx,
            ),
        }
    });
}

/// Runs `then` in `window` once `task` is done.
fn after(
    task: Task<anyhow::Result<()>>,
    window: &mut Window,
    cx: &mut App,
    then: impl FnOnce(anyhow::Result<()>, &mut Window, &mut App) + 'static,
) {
    window
        .spawn(cx, async move |cx| {
            let done = task.await;
            let _ = cx.update(|window, cx| then(done, window, cx));
        })
        .detach();
}

/// An Undo's write: no toast once done, one titled `what` with why if it fails.
fn quietly(task: Task<anyhow::Result<()>>, what: String, window: &mut Window, cx: &mut App) {
    after(task, window, cx, move |done, window, cx| {
        if let Err(err) = done {
            failed(what, &err, window, cx);
        }
    });
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::{PullRequest, Workspace};

    use super::*;
    use crate::overview::fixtures::{id, links, pull_request, repository_id, workspace};

    /// Workspaces `a` (1), `b` (2) and `old` (3, archived); widgets linked by `a` and `old`,
    /// with two open PRs, a draft, an Unreachable, a Finished and a merged one.
    fn snapshot() -> Snapshot {
        let old = Workspace {
            archived_at: Some("2026-11-13T00:00:00Z".into()),
            ..workspace(3, "old")
        };
        let pr = |n, change: fn(&mut PullRequest)| {
            let mut pr = pull_request(n, "acme/widgets");
            change(&mut pr);
            pr
        };
        Snapshot {
            workspaces: vec![workspace(1, "a"), workspace(2, "b"), old],
            repositories: vec![links("acme/widgets", &[id(1), id(3)])],
            prs: vec![
                pr(1, |_| {}),
                pr(2, |pr| pr.draft = true),
                pr(3, |pr| pr.unreachable = true),
                pr(4, |pr| pr.finished = true),
                pr(5, |pr| pr.state = PrState::Merged),
                pull_request(6, "acme/gadgets"),
            ],
            ..Snapshot::default()
        }
    }

    fn widgets() -> RepositoryId {
        repository_id("acme/widgets")
    }

    fn offered(entries: &[GroupEntry]) -> Vec<String> {
        let Some(GroupEntry::LinkToWorkspace(eligible)) = entries.get(1) else {
            panic!("Link to a workspace comes second: {entries:?}");
        };
        eligible.iter().map(|w| w.name.to_string()).collect()
    }

    mod entries {
        use super::*;

        #[test]
        fn should_offer_open_on_github_and_link_to_a_workspace_on_the_pull_requests_view() {
            let entries = entries(&snapshot(), Scope::All, widgets());

            assert_eq!(entries[0], GroupEntry::OpenOnGitHub);
            assert_eq!(entries.len(), 2);
        }

        #[test]
        fn should_add_unlink_after_a_separator_on_a_workspaces_pull_requests() {
            let entries = entries(&snapshot(), Scope::Workspace(id(1)), widgets());

            assert_eq!(entries[2..], [GroupEntry::Separator, GroupEntry::Unlink]);
        }

        #[test]
        fn should_leave_out_archived_workspaces_and_those_linking_it() {
            let entries = entries(&snapshot(), Scope::All, widgets());

            assert_eq!(offered(&entries), ["b"]);
        }

        #[test]
        fn should_offer_every_live_workspace_in_the_sidebars_order_for_one_linked_nowhere() {
            let gadgets = repository_id("acme/gadgets");

            assert_eq!(
                offered(&entries(&snapshot(), Scope::All, gadgets)),
                ["a", "b"]
            );
        }

        #[test]
        fn should_offer_none_when_every_live_workspace_links_it() {
            let mut snapshot = snapshot();
            snapshot.repositories[0].workspaces.push(id(2));

            assert!(offered(&entries(&snapshot, Scope::All, widgets())).is_empty());
        }
    }

    mod held {
        use super::*;

        #[test]
        fn should_hold_a_linked_repository_as_last_read() {
            let held = held(&snapshot(), widgets()).expect("held");

            assert_eq!(held.url, "https://github.com/acme/widgets");
            assert_eq!(held.name.to_string(), "acme/widgets");
        }

        #[test]
        fn should_hold_one_known_only_by_its_prs_from_them() {
            let gadgets = repository_id("acme/gadgets");

            assert_eq!(
                held(&snapshot(), gadgets).map(|r| r.id),
                Some(repository_id("acme/gadgets"))
            );
        }
    }

    mod open_prs {
        use super::*;

        #[test]
        fn should_count_open_prs_drafts_in_unreachable_and_finished_out() {
            assert_eq!(open_prs(&snapshot(), widgets()), 2);
        }
    }

    mod unlinked {
        use super::*;

        #[test]
        fn should_say_its_prs_no_longer_show_here_while_another_workspace_links_it() {
            assert_eq!(
                unlinked("acme/widgets", "agentdeck", 3, true),
                (
                    "Unlinked acme/widgets from agentdeck".to_owned(),
                    Some("Its 3 open PRs no longer show here.".to_owned())
                )
            );
        }

        #[test]
        fn should_add_where_they_are_when_no_other_workspace_links_it() {
            assert_eq!(
                unlinked("acme/widgets", "agentdeck", 3, false).1.as_deref(),
                Some(
                    "Its 3 open PRs no longer show here. They're in the Pull requests view, \
                     under Not in a workspace."
                )
            );
        }

        #[test]
        fn should_speak_of_one_open_pr_in_the_singular() {
            assert_eq!(
                unlinked("acme/widgets", "agentdeck", 1, false).1.as_deref(),
                Some(
                    "Its 1 open PR no longer shows here. It's in the Pull requests view, \
                     under Not in a workspace."
                )
            );
        }

        #[test]
        fn should_say_only_the_title_with_no_open_pr() {
            assert_eq!(unlinked("acme/widgets", "agentdeck", 0, false).1, None);
        }
    }

    #[test]
    fn should_title_a_link_with_the_repository_and_the_workspace() {
        assert_eq!(
            linked_title("acme/widgets", "agentdeck"),
            "Linked acme/widgets to agentdeck"
        );
    }
}
