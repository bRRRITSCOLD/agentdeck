//! Which Repository a folder's git repo works in (spec §3 Suggestions): its `upstream` remote
//! when that is on GitHub, else its `origin`; any other remote is ignored.
//!
//! The remotes are read from git's config files as git reads them, with no `git` process: the
//! system file, the user's global files, then the repo's `config`, each with the files its
//! `include.path` and matching `includeIf` (`gitdir:`, `gitdir/i:`, `onbranch:`) name, read in
//! where they stand ([`config`]). A remote's first `url` is rewritten by the longest matching
//! `url.<base>.insteadOf` among them all, as `git remote get-url` gives it. The repo is the
//! folder or the nearest folder above it holding `.git`, a directory, or a file naming the git
//! dir (`gitdir: …`, as a worktree or a submodule has), whose `commondir` leads a worktree to the
//! main repo's config.
//!
//! Reading a few small files costs less than a `git` process, works without `git` installed and
//! is never stale, so nothing is cached: each read of a Workspace's Suggestions reads them again.

mod config;
mod wildmatch;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use agentdeck_core::RepositoryName;

use self::config::{Context, Entry};

/// The config files git reads before the repo's own, in order, and the home `~/` stands for,
/// from the environment the daemon runs in.
#[derive(Debug, PartialEq, Eq)]
pub struct GitEnv {
    home: Option<PathBuf>,
    files: Vec<PathBuf>,
}

impl GitEnv {
    /// The daemon's own environment's.
    pub fn from_env() -> GitEnv {
        GitEnv::from_vars(|name| std::env::var_os(name))
    }

    /// As git finds them from the variables `var` gives: the system file (`$GIT_CONFIG_SYSTEM`,
    /// else `/etc/gitconfig`; none under a true `$GIT_CONFIG_NOSYSTEM`), then the global ones
    /// (`$GIT_CONFIG_GLOBAL`, else `$XDG_CONFIG_HOME/git/config` or `~/.config/git/config`, and
    /// `~/.gitconfig`).
    fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> GitEnv {
        let set = |name: &str| var(name).filter(|value| !value.is_empty());
        let home = set("HOME").map(PathBuf::from);
        let mut files = Vec::new();
        if !set("GIT_CONFIG_NOSYSTEM").is_some_and(|value| is_true(&value)) {
            let system = var("GIT_CONFIG_SYSTEM").unwrap_or_else(|| "/etc/gitconfig".into());
            files.push(PathBuf::from(system));
        }
        if let Some(global) = var("GIT_CONFIG_GLOBAL") {
            files.push(PathBuf::from(global));
        } else {
            match (set("XDG_CONFIG_HOME"), &home) {
                (Some(xdg), _) => files.push(PathBuf::from(xdg).join("git/config")),
                (None, Some(home)) => files.push(home.join(".config/git/config")),
                (None, None) => {}
            }
            files.extend(home.iter().map(|home| home.join(".gitconfig")));
        }
        GitEnv { home, files }
    }
}

/// A boolean variable as git reads one: `true`, `yes`, `on` or a number other than 0.
fn is_true(value: &OsString) -> bool {
    let value = value.to_string_lossy().to_ascii_lowercase();
    matches!(value.as_str(), "true" | "yes" | "on") || value.parse::<i64>().is_ok_and(|n| n != 0)
}

/// The Repository the git repo holding `folder` works in: its `upstream` remote when that is on
/// GitHub, else its `origin` when that is. `None` outside a git repo, or when neither is.
pub fn repository_of(folder: &Path, env: &GitEnv) -> Option<RepositoryName> {
    let (git_dir, common_dir) = git_dirs(folder)?;
    let context = Context {
        git_dir: &git_dir,
        home: env.home.as_deref(),
    };
    let local = common_dir.join("config");
    let entries: Vec<Entry> = (env.files.iter().chain([&local]))
        .flat_map(|file| config::read(file, &context))
        .collect();
    pick(&entries)
}

/// The git dir of the repo holding `folder`, looking up from it (a worktree's own), and the
/// one holding its config (a worktree's main repo's).
fn git_dirs(folder: &Path) -> Option<(PathBuf, PathBuf)> {
    folder.ancestors().find_map(|dir| {
        let dot_git = dir.join(".git");
        let git_dir = if dot_git.is_dir() {
            dot_git
        } else if dot_git.is_file() {
            linked_git_dir(dir, &std::fs::read_to_string(&dot_git).ok()?)?
        } else {
            return None;
        };
        // A worktree's git dir names the main repo's, which holds the config, in `commondir`.
        let common = match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(common) => git_dir.join(common.trim()),
            Err(_) => git_dir.clone(),
        };
        Some((git_dir, common))
    })
}

/// The git dir a `.git` file in `dir` names (`gitdir: <path>`, relative to `dir` or absolute).
fn linked_git_dir(dir: &Path, dot_git: &str) -> Option<PathBuf> {
    let path = dot_git
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?;
    Some(dir.join(path.trim()))
}

/// `upstream` when it is on GitHub, else `origin` when it is, each as `insteadOf` rewrites it.
fn pick(entries: &[Entry]) -> Option<RepositoryName> {
    let remotes = remote_urls(entries);
    ["upstream", "origin"].into_iter().find_map(|remote| {
        RepositoryName::from_remote_url(&rewrite(remotes.get(remote)?, entries))
    })
}

/// Each remote's URL by the remote's name: its first `url`, where an empty one clears those
/// before it.
fn remote_urls(entries: &[Entry]) -> HashMap<String, String> {
    let mut urls = HashMap::new();
    for entry in entries.iter().filter(|e| e.is("remote", "url")) {
        let (Some(name), Some(url)) = (&entry.subsection, &entry.value) else {
            continue;
        };
        if url.is_empty() {
            urls.remove(name);
        } else {
            urls.entry(name.clone()).or_insert_with(|| url.clone());
        }
    }
    urls
}

/// `url` with the longest prefix a `url.<base>.insteadOf` names replaced by its base; the
/// first of those as long when several are.
fn rewrite(url: &str, entries: &[Entry]) -> String {
    let rules = (entries.iter())
        .filter(|e| e.is("url", "insteadof"))
        .filter_map(|e| Some((e.subsection.as_deref()?, e.value.as_deref()?)));
    let mut best: Option<(&str, &str)> = None;
    for (base, prefix) in rules {
        if url.starts_with(prefix) && prefix.len() > best.map_or(0, |(_, p)| p.len()) {
            best = Some((base, prefix));
        }
    }
    match best {
        Some((base, prefix)) => format!("{base}{}", &url[prefix.len()..]),
        None => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<(String, String)> {
        let mut urls: Vec<_> = remote_urls(&config::parse(text)).into_iter().collect();
        urls.sort();
        urls
    }

    fn picked(text: &str) -> Option<String> {
        pick(&config::parse(text)).map(|name| name.to_string())
    }

    mod remote_urls {
        use super::*;

        #[test]
        fn should_read_each_remotes_url_by_its_name() {
            let config = "[core]\n\trepositoryformatversion = 0\n\
                          [remote \"origin\"]\n\turl = git@github.com:me/widgets.git\n\
                          \tfetch = +refs/heads/*:refs/remotes/origin/*\n\
                          [remote \"upstream\"]\n\turl = https://github.com/acme/widgets.git\n\
                          [branch \"main\"]\n\tremote = origin\n";

            assert_eq!(
                names(config),
                [
                    ("origin".into(), "git@github.com:me/widgets.git".into()),
                    (
                        "upstream".into(),
                        "https://github.com/acme/widgets.git".into()
                    ),
                ]
            );
        }

        #[test]
        fn should_keep_a_remotes_first_url() {
            let config = "[remote \"origin\"]\nurl = https://github.com/a/one\n\
                          url = https://github.com/a/two\n";

            assert_eq!(
                names(config),
                [("origin".into(), "https://github.com/a/one".into())]
            );
        }

        #[test]
        fn should_start_over_after_an_empty_url() {
            let config = "[remote \"origin\"]\nurl = https://github.com/a/one\n\
                          url =\nurl = https://github.com/a/two\n";

            assert_eq!(
                names(config),
                [("origin".into(), "https://github.com/a/two".into())]
            );
        }

        #[test]
        fn should_ignore_case_in_section_and_key_but_not_in_the_remotes_name() {
            let config = "[Remote \"Origin\"]\n  URL = https://github.com/a/b\n";

            assert_eq!(
                names(config),
                [("Origin".into(), "https://github.com/a/b".into())]
            );
        }

        #[test]
        fn should_read_the_old_dotted_header_in_lower_case() {
            let config = "[remote.Upstream]\nurl = https://github.com/a/b\n";

            assert_eq!(
                names(config),
                [("upstream".into(), "https://github.com/a/b".into())]
            );
        }

        #[test]
        fn should_not_take_a_url_from_another_section() {
            let config = "[submodule \"lib\"]\nurl = https://github.com/a/lib\n\
                          [url \"git@github.com:\"]\ninsteadOf = https://github.com/\n";

            assert_eq!(names(config), []);
        }
    }

    mod pick {
        use super::*;

        #[test]
        fn should_prefer_upstream_on_github() {
            let config = "[remote \"origin\"]\nurl = git@github.com:me/widgets.git\n\
                          [remote \"upstream\"]\nurl = https://github.com/acme/widgets.git\n";

            assert_eq!(picked(config).as_deref(), Some("acme/widgets"));
        }

        #[test]
        fn should_take_origin_when_upstream_is_not_on_github() {
            let config = "[remote \"origin\"]\nurl = git@github.com:me/widgets.git\n\
                          [remote \"upstream\"]\nurl = https://gitlab.com/acme/widgets.git\n";

            assert_eq!(picked(config).as_deref(), Some("me/widgets"));
        }

        #[test]
        fn should_ignore_every_other_remote() {
            let config = "[remote \"fork\"]\nurl = https://github.com/me/widgets\n\
                          [remote \"origin\"]\nurl = https://gitlab.com/acme/widgets\n";

            assert_eq!(picked(config), None);
        }

        mod instead_of {
            use super::*;

            #[test]
            fn should_rewrite_a_url_starting_with_an_insteadof_prefix() {
                let config = "[remote \"origin\"]\nurl = gh:acme/widgets\n\
                              [url \"https://github.com/\"]\ninsteadOf = gh:\n";

                assert_eq!(picked(config).as_deref(), Some("acme/widgets"));
            }

            #[test]
            fn should_rewrite_by_the_longest_matching_prefix() {
                let config = "[url \"https://gitlab.com/\"]\ninsteadOf = co:\n\
                              [url \"https://github.com/acme/\"]\ninsteadOf = co:acme/\n\
                              [remote \"origin\"]\nurl = co:acme/widgets\n";

                assert_eq!(picked(config).as_deref(), Some("acme/widgets"));
            }

            #[test]
            fn should_rewrite_upstream_before_choosing_it() {
                let config = "[remote \"origin\"]\nurl = https://github.com/me/widgets\n\
                              [remote \"upstream\"]\nurl = base:widgets\n\
                              [url \"git@github.com:acme/\"]\ninsteadOf = base:\n";

                assert_eq!(picked(config).as_deref(), Some("acme/widgets"));
            }

            #[test]
            fn should_not_rewrite_by_push_instead_of() {
                let config = "[remote \"origin\"]\nurl = gh:acme/widgets\n\
                              [url \"https://github.com/\"]\npushInsteadOf = gh:\n";

                assert_eq!(picked(config), None);
            }
        }
    }

    mod git_env {
        use super::*;

        fn from(vars: &[(&str, &str)]) -> GitEnv {
            GitEnv::from_vars(|name| {
                (vars.iter())
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            })
        }

        fn files(env: &GitEnv) -> Vec<&str> {
            env.files.iter().map(|f| f.to_str().unwrap()).collect()
        }

        #[test]
        fn should_read_the_system_file_then_xdg_then_home() {
            let env = from(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/x")]);

            assert_eq!(
                files(&env),
                ["/etc/gitconfig", "/x/git/config", "/h/.gitconfig"]
            );
            assert_eq!(env.home.as_deref(), Some(Path::new("/h")));
        }

        #[test]
        fn should_look_in_home_dot_config_without_xdg_config_home() {
            let env = from(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "")]);

            assert_eq!(
                files(&env),
                ["/etc/gitconfig", "/h/.config/git/config", "/h/.gitconfig"]
            );
        }

        #[test]
        fn should_read_git_config_global_and_system_in_their_place() {
            let env = from(&[
                ("HOME", "/h"),
                ("GIT_CONFIG_SYSTEM", "/s"),
                ("GIT_CONFIG_GLOBAL", "/g"),
            ]);

            assert_eq!(files(&env), ["/s", "/g"]);
        }

        #[test]
        fn should_skip_the_system_file_only_when_git_config_nosystem_is_true() {
            for (value, skipped) in [("1", true), ("yes", true), ("0", false), ("false", false)] {
                let env = from(&[("GIT_CONFIG_NOSYSTEM", value), ("GIT_CONFIG_GLOBAL", "/g")]);

                assert_eq!(
                    files(&env) == ["/g"],
                    skipped,
                    "GIT_CONFIG_NOSYSTEM={value}"
                );
            }
        }

        #[test]
        fn should_read_no_global_file_without_a_home() {
            assert_eq!(
                files(&from(&[("GIT_CONFIG_NOSYSTEM", "1")])),
                Vec::<&str>::new()
            );
        }
    }

    mod repository_of {
        use super::*;

        fn write(path: &Path, text: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        /// The env of a user whose home is `<dir>/home`, with no system file.
        fn env(dir: &Path) -> GitEnv {
            let home = dir.join("home");
            GitEnv::from_vars(|name| match name {
                "HOME" => Some(home.clone().into()),
                "GIT_CONFIG_NOSYSTEM" => Some("1".into()),
                _ => None,
            })
        }

        fn found(folder: &Path, dir: &Path) -> Option<String> {
            repository_of(folder, &env(dir)).map(|n| n.to_string())
        }

        const ORIGIN: &str = "[remote \"origin\"]\n\turl = https://github.com/acme/widgets\n";
        const SHORT: &str = "[remote \"origin\"]\n\turl = gh:acme/widgets\n";
        const GH: &str = "[url \"https://github.com/\"]\n\tinsteadOf = gh:\n";

        #[test]
        fn should_read_the_repo_holding_a_folder_deep_inside_it() {
            let dir = tempfile::tempdir().unwrap();
            write(&dir.path().join("repo/.git/config"), ORIGIN);
            let deep = dir.path().join("repo/src/bin");
            std::fs::create_dir_all(&deep).unwrap();

            assert_eq!(found(&deep, dir.path()).as_deref(), Some("acme/widgets"));
        }

        #[test]
        fn should_follow_a_worktrees_git_file_to_the_main_repos_config() {
            let dir = tempfile::tempdir().unwrap();
            write(&dir.path().join("main/.git/config"), SHORT);
            write(&dir.path().join("home/.gitconfig"), GH);
            write(
                &dir.path().join("main/.git/worktrees/wt/commondir"),
                "../..\n",
            );
            write(
                &dir.path().join("wt/.git"),
                &format!(
                    "gitdir: {}\n",
                    dir.path().join("main/.git/worktrees/wt").display()
                ),
            );

            assert_eq!(
                found(&dir.path().join("wt"), dir.path()).as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_follow_a_relative_git_file() {
            let dir = tempfile::tempdir().unwrap();
            write(&dir.path().join("store/modules/lib/config"), ORIGIN);
            write(
                &dir.path().join("lib/.git"),
                "gitdir: ../store/modules/lib\n",
            );

            assert_eq!(
                found(&dir.path().join("lib"), dir.path()).as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_find_none_outside_a_repo() {
            let dir = tempfile::tempdir().unwrap();

            assert_eq!(found(dir.path(), dir.path()), None);
        }

        #[test]
        fn should_rewrite_by_an_insteadof_in_the_repos_config() {
            let dir = tempfile::tempdir().unwrap();
            write(
                &dir.path().join("repo/.git/config"),
                &format!("{SHORT}{GH}"),
            );

            assert_eq!(
                found(&dir.path().join("repo"), dir.path()).as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_rewrite_by_an_insteadof_in_the_global_config() {
            let dir = tempfile::tempdir().unwrap();
            write(&dir.path().join("repo/.git/config"), SHORT);
            write(&dir.path().join("home/.config/git/config"), GH);

            assert_eq!(
                found(&dir.path().join("repo"), dir.path()).as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_read_a_remote_from_an_included_file() {
            let dir = tempfile::tempdir().unwrap();
            write(
                &dir.path().join("repo/.git/config"),
                "[include]\n\tpath = ../../shared/remotes.inc\n",
            );
            write(&dir.path().join("shared/remotes.inc"), ORIGIN);

            assert_eq!(
                found(&dir.path().join("repo"), dir.path()).as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_follow_a_global_include_if_only_for_a_repo_its_gitdir_matches() {
            let dir = tempfile::tempdir().unwrap();
            write(
                &dir.path().join("home/.gitconfig"),
                "[includeIf \"gitdir:~/work/\"]\n\tpath = work.inc\n",
            );
            write(&dir.path().join("home/work.inc"), GH);
            write(&dir.path().join("home/work/repo/.git/config"), SHORT);
            write(&dir.path().join("home/play/repo/.git/config"), SHORT);

            assert_eq!(
                found(&dir.path().join("home/work/repo"), dir.path()).as_deref(),
                Some("acme/widgets")
            );
            assert_eq!(found(&dir.path().join("home/play/repo"), dir.path()), None);
        }
    }
}
