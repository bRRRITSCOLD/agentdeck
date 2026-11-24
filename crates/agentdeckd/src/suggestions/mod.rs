//! Suggestions (spec §3, §4): the Repositories a Workspace's Panes work in that it doesn't
//! link, offered for linking.
//!
//! For each unarchived Workspace, every Pane's current folder (an exited one's last known
//! folder) gives the Repository of its git repo ([`remotes`]); the Repositories the Workspace
//! links are left out, those other Workspaces link aren't. Nothing scans the disk beyond those
//! folders.
//!
//! Each `owner/name` is confirmed with GitHub once per run, which gives its id; the answer,
//! GitHub's Repository or that GitHub can't see one by that name, is kept in memory, so one
//! GitHub can't see is never suggested. A failed call keeps nothing, and is tried again on the
//! next read. Signed out, only a Repository agentdeck keeps by that name, and the sign-in could
//! still see when it last looked, is suggested.
//!
//! [`Suggestions::watch`] works them out again whenever something they depend on changes (a
//! Pane opening, exiting, moving or going, a Link or the Workspace changing, a Repository
//! stored or renamed, the GitHub sign-in changing) and sends `changed suggestions` for each
//! Workspace whose Suggestions differ from the last ones it worked out.

pub mod remotes;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{PaneId, RepositoryName, TabId, WorkspaceId};
use tokio::sync::{Mutex, broadcast};

use crate::db::{self, Db, DbError};
use crate::events::Events;
use crate::github_auth::GithubAuth;
use crate::model::Repository;
use crate::prsync::github::{Github, GithubError, RemoteRepository};

/// Works out each Workspace's Suggestions; clones share the GitHub answers kept.
#[derive(Clone)]
pub struct Suggestions {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    events: Events,
    auth: GithubAuth,
    github: Github,
    /// GitHub's answer for each `owner/name` asked about this run, by the name in lower case:
    /// the Repository, or `None` when GitHub can't see one by that name. Held across the call,
    /// so two reads never ask about one name twice.
    confirmed: Mutex<HashMap<String, Option<RemoteRepository>>>,
}

impl Suggestions {
    /// Suggestions read from `db`, confirmed with `github` as `auth` signs in, announced on
    /// `events`.
    pub fn new(db: Db, events: Events, auth: GithubAuth, github: Github) -> Suggestions {
        Suggestions {
            inner: Arc::new(Inner {
                db,
                events,
                auth,
                github,
                confirmed: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Workspace `workspace`'s Suggestions, by name ignoring case, then id; none while it is
    /// archived. One that doesn't exist is [`DbError::NotFound`].
    pub async fn list(&self, workspace: WorkspaceId) -> Result<Vec<Repository>, DbError> {
        let read = self
            .inner
            .db
            .call(move |c| {
                let archived = db::workspaces::get(c, workspace)?.archived_at.is_some();
                if archived {
                    return Ok(None);
                }
                let folders = db::panes::folders_in(c, workspace)?;
                let linked = db::repositories::linked_by(c, workspace)?;
                Ok(Some((folders, linked)))
            })
            .await?;
        let Some((folders, linked)) = read else {
            return Ok(Vec::new());
        };
        let names = tokio::task::spawn_blocking(move || worked_in(&folders))
            .await
            .unwrap_or_default();
        let linked: HashSet<_> = linked.into_iter().collect();
        let mut found: BTreeMap<_, Repository> = BTreeMap::new();
        for name in names.into_values() {
            if let Some(repository) = self.confirm(&name).await?
                && !linked.contains(&repository.id)
            {
                let key = (repository.name.to_string().to_lowercase(), repository.id);
                found.insert(key, repository);
            }
        }
        Ok(found.into_values().collect())
    }

    /// The Repository GitHub calls `name`, as this run first heard; signed out, the one
    /// agentdeck keeps by that name while the sign-in could still see it.
    async fn confirm(&self, name: &RepositoryName) -> Result<Option<Repository>, DbError> {
        let key = name.to_string().to_lowercase();
        let mut confirmed = self.inner.confirmed.lock().await;
        if let Some(answer) = confirmed.get(&key) {
            return Ok(answer.clone().map(repository));
        }
        let Some(token) = self.inner.auth.token() else {
            drop(confirmed);
            return self.known(name).await;
        };
        let github = &self.inner.github;
        let answer = match github.repository(&token, name).await {
            Err(GithubError::Unauthorized) => match self.inner.auth.reread().await {
                Ok(fresh) => github.repository(&fresh, name).await,
                Err(_) => Err(GithubError::Unauthorized),
            },
            answer => answer,
        };
        match answer {
            Ok(answer) => {
                confirmed.insert(key, answer.clone());
                Ok(answer.map(repository))
            }
            Err(err) => {
                if matches!(err, GithubError::Unauthorized) {
                    self.inner.auth.rejected();
                }
                tracing::warn!("confirming {name} for Suggestions: {err}");
                drop(confirmed);
                self.known(name).await
            }
        }
    }

    /// The Repository agentdeck keeps by `name`, ignoring case, unless the sign-in can't see
    /// it any more.
    async fn known(&self, name: &RepositoryName) -> Result<Option<Repository>, DbError> {
        let name = name.clone();
        self.inner
            .db
            .call(move |c| {
                let Some(id) = db::repositories::by_name(c, &name)? else {
                    return Ok(None);
                };
                let kept = db::repositories::get(c, id)?;
                Ok((!kept.unreachable).then_some(Repository {
                    id: kept.id,
                    name: kept.name,
                    url: kept.url,
                    private: kept.private,
                    archived: kept.archived,
                }))
            })
            .await
    }

    /// Works Suggestions out again whenever `events` says something they depend on changed,
    /// and sends `changed suggestions` for each Workspace whose Suggestions differ from the last
    /// worked out. Runs until the runtime stops.
    pub fn watch(&self) {
        let this = self.clone();
        let mut feed = self.inner.events.subscribe();
        tokio::spawn(async move {
            let mut last: HashMap<WorkspaceId, Vec<Repository>> = HashMap::new();
            loop {
                let mut due = Due::default();
                match feed.recv().await {
                    Ok(event) => due.add(&event),
                    Err(broadcast::error::RecvError::Lagged(_)) => due.all = true,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
                // A burst (a PR sync's pages, a tab's panes going) is worked out once.
                loop {
                    match feed.try_recv() {
                        Ok(event) => due.add(&event),
                        Err(broadcast::error::TryRecvError::Lagged(_)) => due.all = true,
                        Err(_) => break,
                    }
                }
                if due.is_empty() {
                    continue;
                }
                if let Err(err) = this.recompute(due, &mut last).await {
                    tracing::warn!("working out Suggestions: {err}");
                }
            }
        });
    }

    /// Works out the Suggestions of the Workspaces `due` names and announces those that
    /// changed since `last`.
    async fn recompute(
        &self,
        due: Due,
        last: &mut HashMap<WorkspaceId, Vec<Repository>>,
    ) -> Result<(), DbError> {
        let Due {
            all,
            workspaces,
            panes,
            tabs,
        } = due;
        let workspaces: Vec<WorkspaceId> = self
            .inner
            .db
            .call(move |c| {
                if all {
                    return Ok(db::workspaces::list(c)?.into_iter().map(|w| w.id).collect());
                }
                let mut ids: Vec<WorkspaceId> = workspaces.into_iter().collect();
                let mut gone = false;
                for pane in panes {
                    match db::panes::workspace_of(c, pane) {
                        Ok(id) => ids.push(id),
                        Err(DbError::NotFound(_)) => gone = true,
                        Err(err) => return Err(err),
                    }
                }
                for tab in tabs {
                    match db::panes::workspace_of_tab(c, tab)? {
                        Some(id) => ids.push(id),
                        None => gone = true,
                    }
                }
                // A Pane or tab deleted names no Workspace any more: look at them all.
                if gone {
                    return Ok(db::workspaces::list(c)?.into_iter().map(|w| w.id).collect());
                }
                ids.sort();
                ids.dedup();
                Ok(ids)
            })
            .await?;
        for workspace in workspaces {
            let now = match self.list(workspace).await {
                Ok(now) => now,
                Err(DbError::NotFound(_)) => {
                    last.remove(&workspace);
                    continue;
                }
                Err(err) => return Err(err),
            };
            if last
                .get(&workspace)
                .map_or(!now.is_empty(), |was| *was != now)
            {
                last.insert(workspace, now);
                self.inner.events.changed(Entity::Suggestions, workspace);
            }
        }
        Ok(())
    }
}

/// The Repository a git repo's remote names, by the name in lower case, for each of `folders`
/// that is in one.
fn worked_in(folders: &[String]) -> BTreeMap<String, RepositoryName> {
    let git = remotes::Git::default();
    let folders: BTreeSet<&String> = folders.iter().collect();
    (folders.into_iter())
        .filter_map(|folder| remotes::repository_of(&PathBuf::from(folder), &git))
        .map(|name| (name.to_string().to_lowercase(), name))
        .collect()
}

fn repository(found: RemoteRepository) -> Repository {
    Repository {
        id: found.id,
        name: found.name,
        url: found.url,
        private: found.private,
        archived: found.archived,
    }
}

/// Which Workspaces' Suggestions to work out again.
#[derive(Default)]
struct Due {
    /// Every Workspace's.
    all: bool,
    workspaces: HashSet<WorkspaceId>,
    /// Those of the Workspaces these Panes are in.
    panes: HashSet<PaneId>,
    /// Those of the Workspaces these tabs are in.
    tabs: HashSet<TabId>,
}

impl Due {
    fn is_empty(&self) -> bool {
        !self.all && self.workspaces.is_empty() && self.panes.is_empty() && self.tabs.is_empty()
    }

    /// What `event` makes due: a Pane's Workspace when it opened, moved, exited or went; a
    /// tab's when its layout changed; a Workspace when its Links or itself changed; every one
    /// when a Repository was stored, renamed or deleted, or the sign-in changed.
    fn add(&mut self, event: &ServerMessage) {
        match event {
            ServerMessage::Exit { pane, .. } => {
                self.panes.insert(*pane);
            }
            ServerMessage::Changed { entity, id } => match entity {
                Entity::Pane => match id.parse() {
                    Ok(pane) => {
                        self.panes.insert(pane);
                    }
                    Err(_) => self.all = true,
                },
                Entity::Tab => match id.parse() {
                    Ok(tab) => {
                        self.tabs.insert(tab);
                    }
                    Err(_) => self.all = true,
                },
                Entity::Workspace => match id.parse() {
                    Ok(workspace) => {
                        self.workspaces.insert(workspace);
                    }
                    Err(_) => self.all = true,
                },
                Entity::Repository => self.all = true,
                Entity::PrSync
                | Entity::Settings
                | Entity::PullRequest
                | Entity::Suggestions
                | Entity::GithubRepositories => {}
            },
            ServerMessage::GithubAuth { .. } => self.all = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod due {
        use super::*;

        fn changed(entity: Entity, id: &str) -> ServerMessage {
            ServerMessage::Changed {
                entity,
                id: id.into(),
            }
        }

        const PANE: &str = "019a0000-0000-7000-8000-000000000001";

        #[test]
        fn should_follow_a_panes_changes_and_its_exit() {
            let mut due = Due::default();

            due.add(&changed(Entity::Pane, PANE));
            due.add(&ServerMessage::Exit {
                pane: PANE.parse().unwrap(),
                code: Some(0),
            });

            assert_eq!(due.panes.len(), 1);
            assert!(!due.all);
        }

        #[test]
        fn should_work_out_every_workspace_after_a_repository_or_sign_in_change() {
            for event in [
                changed(Entity::Repository, "1296269"),
                ServerMessage::GithubAuth {
                    device: None,
                    message: None,
                },
            ] {
                let mut due = Due::default();
                due.add(&event);
                assert!(due.all);
            }
        }

        #[test]
        fn should_ignore_its_own_announcements_and_pr_changes() {
            let mut due = Due::default();

            due.add(&changed(Entity::Suggestions, PANE));
            due.add(&changed(Entity::PullRequest, "1296269#7"));
            due.add(&changed(Entity::PrSync, "status"));

            assert!(due.is_empty());
        }
    }
}
