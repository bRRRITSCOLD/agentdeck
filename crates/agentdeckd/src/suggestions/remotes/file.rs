//! The fallback for when `git` can't answer (spec §3 Suggestions): the remotes read straight
//! from the repo's `config` file. The repo is the folder or the nearest folder above it holding
//! `.git`, a directory, or a file naming the git dir (`gitdir: …`, as a worktree or a submodule
//! has), whose `commondir` leads a worktree to the main repo's config. Only that file is read:
//! `include` and `includeIf` files, the user's global config and `url.<base>.insteadOf`
//! rewrites aren't, so a remote that only they make a GitHub URL suggests nothing here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Each remote's URL by its name, from the `config` file of the git repo holding `folder`.
/// `None` outside a git repo, or when the file can't be read.
pub fn remote_urls_of(folder: &Path) -> Option<HashMap<String, String>> {
    let config = std::fs::read_to_string(config_file(folder)?).ok()?;
    Some(remote_urls(&config))
}
/// The `config` file of the git repo holding `folder`, looking up from it.
fn config_file(folder: &Path) -> Option<PathBuf> {
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
            Err(_) => git_dir,
        };
        Some(common.join("config"))
    })
}

/// The git dir a `.git` file in `dir` names (`gitdir: <path>`, relative to `dir` or absolute).
fn linked_git_dir(dir: &Path, dot_git: &str) -> Option<PathBuf> {
    let path = dot_git
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?;
    Some(dir.join(path.trim()))
}

/// Each remote's URL in git config `text`, by the remote's name: its first `url`.
pub fn remote_urls(text: &str) -> HashMap<String, String> {
    let mut urls = HashMap::new();
    let mut remote: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            remote = remote_section(header);
            // `[remote "origin"] url = …`: a key may follow its header on the line.
            let Some((_, rest)) = header.split_once(']') else {
                continue;
            };
            if rest.trim().is_empty() {
                continue;
            }
            if let (Some(name), Some(url)) = (&remote, url_value(rest.trim())) {
                urls.entry(name.clone()).or_insert(url);
            }
            continue;
        }
        if let (Some(name), Some(url)) = (&remote, url_value(line)) {
            urls.entry(name.clone()).or_insert(url);
        }
    }
    urls
}

/// The remote a section header names (the text after its `[`): `remote "origin"]`, or the old
/// `remote.origin]`, whose name git reads in lower case. `None` for any other section.
fn remote_section(header: &str) -> Option<String> {
    let (header, _) = header.split_once(']')?;
    let header = header.trim();
    if let Some((section, sub)) = header.split_once(char::is_whitespace) {
        let sub = sub.trim().strip_prefix('"')?.strip_suffix('"')?;
        return section
            .eq_ignore_ascii_case("remote")
            .then(|| unescape(sub));
    }
    let (section, sub) = header.split_once('.')?;
    section
        .eq_ignore_ascii_case("remote")
        .then(|| sub.to_lowercase())
}

/// The value of a `url = …` line; `None` for another key. Quotes are taken off and an
/// unquoted `#` or `;` starts a comment.
fn url_value(line: &str) -> Option<String> {
    let (key, value) = line.split_once('=')?;
    if !key.trim().eq_ignore_ascii_case("url") {
        return None;
    }
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = value.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => out.extend(chars.next()),
            '#' | ';' if !quoted => break,
            c => out.push(c),
        }
    }
    let url = out.trim().to_owned();
    (!url.is_empty()).then_some(url)
}

/// A subsection name with git's `\"` and `\\` escapes undone.
fn unescape(sub: &str) -> String {
    let mut out = String::new();
    let mut chars = sub.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<(String, String)> {
        let mut urls: Vec<_> = remote_urls(text).into_iter().collect();
        urls.sort();
        urls
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
        fn should_skip_comments_and_take_off_quotes() {
            let config = "# a comment\n; another\n[remote \"origin\"]\n\
                          url = \"https://github.com/a/b\" # trailing\n";

            assert_eq!(
                names(config),
                [("origin".into(), "https://github.com/a/b".into())]
            );
        }

        #[test]
        fn should_read_a_url_on_its_headers_line() {
            let config = "[remote \"origin\"] url = https://github.com/a/b\n";

            assert_eq!(
                names(config),
                [("origin".into(), "https://github.com/a/b".into())]
            );
        }

        #[test]
        fn should_not_take_a_url_from_another_section() {
            let config = "[submodule \"lib\"]\nurl = https://github.com/a/lib\n\
                          [url \"git@github.com:\"]\ninsteadOf = https://github.com/\n";

            assert_eq!(names(config), []);
        }
    }

    mod remote_urls_of {
        use super::*;

        fn write(path: &Path, text: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        fn origin(folder: &Path) -> Option<String> {
            remote_urls_of(folder)?.remove("origin")
        }

        const ORIGIN: &str = "[remote \"origin\"]\n\turl = https://github.com/acme/widgets\n";

        #[test]
        fn should_read_the_repo_holding_a_folder_deep_inside_it() {
            let dir = tempfile::tempdir().unwrap();
            write(&dir.path().join("repo/.git/config"), ORIGIN);
            let deep = dir.path().join("repo/src/bin");
            std::fs::create_dir_all(&deep).unwrap();

            assert_eq!(
                origin(&deep).as_deref(),
                Some("https://github.com/acme/widgets")
            );
        }

        #[test]
        fn should_follow_a_worktrees_git_file_to_the_main_repos_config() {
            let dir = tempfile::tempdir().unwrap();
            write(&dir.path().join("main/.git/config"), ORIGIN);
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
                origin(&dir.path().join("wt")).as_deref(),
                Some("https://github.com/acme/widgets")
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
                origin(&dir.path().join("lib")).as_deref(),
                Some("https://github.com/acme/widgets")
            );
        }

        #[test]
        fn should_find_none_outside_a_repo() {
            let dir = tempfile::tempdir().unwrap();

            assert_eq!(remote_urls_of(dir.path()), None);
        }
    }
}
