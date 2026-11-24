//! git config files read as git reads them (spec §3 Suggestions): the syntax, and the files an
//! `include.path` or a matching `includeIf "<condition>".path` names, read where the include
//! stands.

use std::path::{Path, PathBuf};

use super::wildmatch::wildmatch;

/// One `key = value` of a config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// In lower case, as git compares it.
    pub section: String,
    /// As written, but in lower case under the old `[section.sub]` header.
    pub subsection: Option<String>,
    /// In lower case, as git compares it.
    pub key: String,
    /// `None` for a key with no `=`, which git reads as true.
    pub value: Option<String>,
}

impl Entry {
    /// Whether this is `<section>.<anything>.<key>` or `<section>.<key>`; both in lower case.
    pub fn is(&self, section: &str, key: &str) -> bool {
        self.section == section && self.key == key
    }
}

/// What an `includeIf` condition is tested against.
pub struct Context<'a> {
    /// The repo's git dir (a worktree's own, not the main repo's), for `gitdir:` and `onbranch:`.
    pub git_dir: &'a Path,
    /// What `~/` stands for.
    pub home: Option<&'a Path>,
}

/// How deep includes nest before git gives up; deeper ones are left unread.
const MAX_INCLUDE_DEPTH: usize = 10;

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// The entries of config `text`, in order; a line git can't read is skipped.
pub fn parse(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut section: Option<(String, Option<String>)> = None;
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '[' => {
                chars.next();
                section = header(&mut chars);
                if section.is_none() {
                    skip_line(&mut chars);
                }
            }
            c if c.is_ascii_alphabetic() => {
                let mut key = String::new();
                while let Some(&c) = chars.peek().filter(|c| c.is_ascii_alphanumeric() || **c == '-')
                {
                    key.push(c.to_ascii_lowercase());
                    chars.next();
                }
                skip_blanks(&mut chars);
                let value = match chars.peek() {
                    Some('=') => {
                        chars.next();
                        Some(value(&mut chars))
                    }
                    None | Some('\n' | '#' | ';') => None,
                    Some(_) => {
                        skip_line(&mut chars);
                        continue;
                    }
                };
                if let Some((name, sub)) = &section {
                    entries.push(Entry {
                        section: name.clone(),
                        subsection: sub.clone(),
                        key,
                        value,
                    });
                }
            }
            // A comment, or a line git would refuse.
            _ => skip_line(&mut chars),
        }
    }
    entries
}

/// The section a header names, read up to its `]` from just after its `[`: `section`,
/// `section "sub"` or the old `section.sub`. `None` for one git can't read.
fn header(chars: &mut Chars) -> Option<(String, Option<String>)> {
    // A line break is never taken here, so the caller's skip to the next line keeps the next.
    let mut name = String::new();
    loop {
        match chars.next_if(|&c| c != '\n')? {
            ']' => {
                let name = name.to_ascii_lowercase();
                if name.is_empty() {
                    return None;
                }
                return Some(match name.split_once('.') {
                    Some((section, sub)) => (section.to_owned(), Some(sub.to_owned())),
                    None => (name, None),
                });
            }
            c if c.is_ascii_alphanumeric() || c == '-' || c == '.' => name.push(c),
            ' ' | '\t' if !name.is_empty() && !name.contains('.') => break,
            _ => return None,
        }
    }
    skip_blanks(chars);
    chars.next_if_eq(&'"')?;
    let mut sub = String::new();
    loop {
        match chars.next_if(|&c| c != '\n')? {
            '"' => break,
            '\\' => sub.push(chars.next_if(|&c| c != '\n')?),
            c => sub.push(c),
        }
    }
    chars.next_if_eq(&']')?;
    Some((name.to_ascii_lowercase(), Some(sub)))
}

/// A value read from after its `=` to the end of its line, as git reads it: trimmed, quotes
/// taken off, an unquoted `#` or `;` starting a comment, `\` escapes undone and a `\` at the end
/// of a line joining the next.
fn value(chars: &mut Chars) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut spaces = 0;
    while let Some(c) = chars.next() {
        match c {
            '\n' => break,
            c if c.is_whitespace() && !quoted => {
                if !out.is_empty() {
                    spaces += 1;
                }
            }
            '#' | ';' if !quoted => {
                skip_line(chars);
                break;
            }
            c => {
                out.extend(std::iter::repeat_n(' ', spaces));
                spaces = 0;
                match c {
                    '"' => quoted = !quoted,
                    '\\' => match chars.next() {
                        Some('\n') | None => {}
                        Some('\r') if chars.peek() == Some(&'\n') => {
                            chars.next();
                        }
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('b') => out.push('\u{8}'),
                        Some(c) => out.push(c),
                    },
                    c => out.push(c),
                }
            }
        }
    }
    out
}

fn skip_blanks(chars: &mut Chars) {
    while chars.next_if(|c| matches!(c, ' ' | '\t' | '\r')).is_some() {}
}

fn skip_line(chars: &mut Chars) {
    for c in chars.by_ref() {
        if c == '\n' {
            return;
        }
    }
}

/// The entries of the config `file`, each file it includes read in where the include stands;
/// none when it can't be read.
pub fn read(file: &Path, context: &Context) -> Vec<Entry> {
    let mut entries = Vec::new();
    read_into(file, context, 0, &mut entries);
    entries
}

fn read_into(file: &Path, context: &Context, depth: usize, entries: &mut Vec<Entry>) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    for entry in parse(&text) {
        let included = entry.key == "path"
            && match (entry.section.as_str(), entry.subsection.as_deref()) {
                ("include", None) => true,
                ("includeif", Some(condition)) => context.holds(condition, file),
                _ => false,
            };
        let target = (entry.value.as_deref())
            .filter(|_| included && depth < MAX_INCLUDE_DEPTH)
            .and_then(|path| context.include_path(path, file));
        entries.push(entry);
        if let Some(target) = target {
            read_into(&target, context, depth + 1, entries);
        }
    }
}

impl Context<'_> {
    /// The file an include in `file` names by `path`: under home for `~/`, beside `file` when
    /// relative.
    fn include_path(&self, path: &str, file: &Path) -> Option<PathBuf> {
        if let Some(rest) = path.strip_prefix("~/") {
            return Some(self.home?.join(rest));
        }
        let path = Path::new(path);
        if path.is_absolute() {
            return Some(path.to_owned());
        }
        Some(file.parent()?.join(path))
    }

    /// Whether the `includeIf` `condition` in `file` holds for the repo.
    fn holds(&self, condition: &str, file: &Path) -> bool {
        if let Some(pattern) = condition.strip_prefix("gitdir:") {
            self.in_git_dir(pattern, file, false)
        } else if let Some(pattern) = condition.strip_prefix("gitdir/i:") {
            self.in_git_dir(pattern, file, true)
        } else if let Some(pattern) = condition.strip_prefix("onbranch:") {
            self.on_branch(pattern)
        } else {
            false
        }
    }

    /// `gitdir:`: whether the git dir, its real path or as found, matches `pattern`, where
    /// `~/` is home, `./` the dir of `file`, a pattern not starting with `/` may match at any
    /// depth and one ending in `/` matches everything under it.
    fn in_git_dir(&self, pattern: &str, file: &Path, fold: bool) -> bool {
        // `./`'s dir is matched as it is written, not as a pattern.
        let (literal, mut glob) = if let Some(rest) = pattern.strip_prefix("~/") {
            let Some(home) = self.home else {
                return false;
            };
            (String::new(), format!("{}/{rest}", home.display()))
        } else if let Some(rest) = pattern.strip_prefix("./") {
            let real = std::fs::canonicalize(file).ok();
            let Some(dir) = real.as_deref().and_then(Path::parent) else {
                return false;
            };
            (format!("{}/", dir.display()), rest.to_owned())
        } else if pattern.starts_with('/') {
            (String::new(), pattern.to_owned())
        } else {
            (String::new(), format!("**/{pattern}"))
        };
        if format!("{literal}{glob}").ends_with('/') {
            glob.push_str("**");
        }
        let found = std::path::absolute(self.git_dir).ok();
        let real = std::fs::canonicalize(self.git_dir).ok();
        [real, found].into_iter().flatten().any(|dir| {
            let dir = dir.to_string_lossy();
            dir.get(..literal.len()).is_some_and(|head| {
                if fold {
                    head.eq_ignore_ascii_case(&literal)
                } else {
                    head == literal
                }
            }) && wildmatch(&glob, &dir[literal.len()..], fold)
        })
    }

    /// `onbranch:`: whether the branch checked out matches `pattern`, where one ending in `/`
    /// matches every branch under it. A detached HEAD is on none.
    fn on_branch(&self, pattern: &str) -> bool {
        let Ok(head) = std::fs::read_to_string(self.git_dir.join("HEAD")) else {
            return false;
        };
        let Some(branch) = head.trim().strip_prefix("ref: refs/heads/") else {
            return false;
        };
        let mut pattern = pattern.to_owned();
        if pattern.ends_with('/') {
            pattern.push_str("**");
        }
        wildmatch(&pattern, branch, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(section: &str, subsection: Option<&str>, key: &str, value: Option<&str>) -> Entry {
        Entry {
            section: section.into(),
            subsection: subsection.map(Into::into),
            key: key.into(),
            value: value.map(Into::into),
        }
    }

    /// Each entry's `key` and value, the way `git config --list` shows them.
    fn listed(entries: &[Entry]) -> Vec<String> {
        (entries.iter())
            .map(|e| {
                let name = match &e.subsection {
                    Some(sub) => format!("{}.{sub}.{}", e.section, e.key),
                    None => format!("{}.{}", e.section, e.key),
                };
                match &e.value {
                    Some(value) => format!("{name}={value}"),
                    None => name,
                }
            })
            .collect()
    }

    mod parse {
        use super::*;

        #[test]
        fn should_read_sections_and_keys_in_lower_case_and_subsections_as_written() {
            let config = "[Core]\n\tBare = false\n[Remote \"Origin\"]\n\tURL = https://x/y\n";

            assert_eq!(
                parse(config),
                [
                    entry("core", None, "bare", Some("false")),
                    entry("remote", Some("Origin"), "url", Some("https://x/y")),
                ]
            );
        }

        #[test]
        fn should_read_the_old_dotted_header_with_its_subsection_in_lower_case() {
            assert_eq!(
                listed(&parse("[remote.Upstream]\nurl = a\n")),
                ["remote.upstream.url=a"]
            );
        }

        #[test]
        fn should_undo_a_quoted_subsections_escapes() {
            assert_eq!(
                listed(&parse("[url \"a\\\"b\\\\c\"]\ninsteadOf = d\n")),
                ["url.a\"b\\c.insteadof=d"]
            );
        }

        #[test]
        fn should_read_a_key_without_a_value_as_none() {
            assert_eq!(
                parse("[core]\n\tbare\n"),
                [entry("core", None, "bare", None)]
            );
        }

        #[test]
        fn should_read_a_key_on_its_headers_line() {
            assert_eq!(
                listed(&parse("[remote \"origin\"] url = a\n")),
                ["remote.origin.url=a"]
            );
        }

        #[test]
        fn should_skip_comments_and_take_off_quotes() {
            let config = "# a comment\n; another\n[remote \"origin\"] # here too\n\
                          url = \"https://x/#y\" # trailing\n";

            assert_eq!(listed(&parse(config)), ["remote.origin.url=https://x/#y"]);
        }

        #[test]
        fn should_trim_a_value_and_keep_its_inner_spaces() {
            assert_eq!(
                listed(&parse("[a]\nb =   one two  \r\n")),
                ["a.b=one two"]
            );
        }

        #[test]
        fn should_undo_escapes_and_join_a_continued_line() {
            assert_eq!(
                listed(&parse("[a]\nb = one\\\n two\\t\\\"\\\\\n")),
                ["a.b=one two\t\"\\"]
            );
        }

        #[test]
        fn should_skip_a_line_it_cannot_read() {
            assert_eq!(
                listed(&parse("orphan = 1\n[a]\n= 2\nb = 3\n[broken\n[c]\nd = 4\n")),
                ["a.b=3", "c.d=4"]
            );
        }
    }

    mod read {
        use std::path::PathBuf;

        use super::*;

        fn write(path: &Path, text: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        /// A temp dir with a git dir at `repo/.git` and a home at `home`.
        struct Fixture {
            dir: tempfile::TempDir,
        }

        impl Fixture {
            fn new() -> Fixture {
                let dir = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(dir.path().join("work/repo/.git")).unwrap();
                std::fs::create_dir_all(dir.path().join("home")).unwrap();
                Fixture { dir }
            }

            fn path(&self, rel: &str) -> PathBuf {
                self.dir.path().join(rel)
            }

            fn git_dir(&self) -> PathBuf {
                self.path("work/repo/.git")
            }

            /// Reads `file` for the repo at `work/repo`.
            fn read(&self, file: &str) -> Vec<String> {
                let git_dir = self.git_dir();
                let home = self.path("home");
                let context = Context {
                    git_dir: &git_dir,
                    home: Some(&home),
                };
                listed(&read(&self.path(file), &context))
            }
        }

        #[test]
        fn should_read_an_included_file_where_the_include_stands() {
            let f = Fixture::new();
            write(
                &f.path("config"),
                "[a]\none = 1\n[include]\npath = more.inc\n[a]\nthree = 3\n",
            );
            write(&f.path("more.inc"), "[a]\ntwo = 2\n");

            assert_eq!(
                f.read("config"),
                ["a.one=1", "include.path=more.inc", "a.two=2", "a.three=3"]
            );
        }

        #[test]
        fn should_find_a_relative_include_beside_the_including_file() {
            let f = Fixture::new();
            write(&f.path("config"), "[include]\npath = nested/one.inc\n");
            write(&f.path("nested/one.inc"), "[include]\npath = two.inc\n");
            write(&f.path("nested/two.inc"), "[a]\nb = 2\n");

            assert_eq!(f.read("config").last().unwrap(), "a.b=2");
        }

        #[test]
        fn should_find_an_include_under_home_or_at_an_absolute_path() {
            let f = Fixture::new();
            let absolute = f.path("elsewhere/abs.inc");
            write(
                &f.path("config"),
                &format!(
                    "[include]\npath = ~/home.inc\npath = {}\n",
                    absolute.display()
                ),
            );
            write(&f.path("home/home.inc"), "[a]\nhome = 1\n");
            write(&absolute, "[a]\nabsolute = 1\n");

            let read = f.read("config");

            assert!(read.contains(&"a.home=1".to_owned()), "{read:?}");
            assert!(read.contains(&"a.absolute=1".to_owned()), "{read:?}");
        }

        #[test]
        fn should_skip_an_include_that_cannot_be_read() {
            let f = Fixture::new();
            write(&f.path("config"), "[include]\npath = missing.inc\n[a]\nb = 1\n");

            assert_eq!(f.read("config"), ["include.path=missing.inc", "a.b=1"]);
        }

        #[test]
        fn should_stop_following_includes_ten_deep() {
            let f = Fixture::new();
            write(&f.path("config"), "[a]\nb = 1\n[include]\npath = config\n");

            assert_eq!(
                (f.read("config").iter())
                    .filter(|line| *line == "a.b=1")
                    .count(),
                11
            );
        }

        #[test]
        fn should_read_nothing_from_a_file_that_cannot_be_read() {
            let f = Fixture::new();

            assert_eq!(f.read("missing"), Vec::<String>::new());
        }

        mod include_if {
            use super::*;

            /// Whether `config` with `[includeIf "<condition>"] path = cond.inc` reads it in.
            fn follows(f: &Fixture, condition: &str) -> bool {
                write(
                    &f.path("config"),
                    &format!("[includeIf \"{condition}\"]\npath = cond.inc\n"),
                );
                write(&f.path("cond.inc"), "[a]\nb = 1\n");
                f.read("config").contains(&"a.b=1".to_owned())
            }

            #[test]
            fn should_follow_gitdir_when_the_repos_git_dir_is_under_it() {
                let f = Fixture::new();

                assert!(follows(
                    &f,
                    &format!("gitdir:{}/", f.path("work").display())
                ));
                assert!(follows(
                    &f,
                    &format!("gitdir:{}", f.git_dir().display())
                ));
            }

            #[test]
            fn should_not_follow_gitdir_when_the_repo_is_elsewhere() {
                let f = Fixture::new();

                assert!(!follows(
                    &f,
                    &format!("gitdir:{}/", f.path("play").display())
                ));
                assert!(!follows(
                    &f,
                    &format!("gitdir:{}", f.path("work").display())
                ));
            }

            #[test]
            fn should_match_a_relative_gitdir_pattern_at_any_depth() {
                let f = Fixture::new();

                assert!(follows(&f, "gitdir:work/"));
                assert!(follows(&f, "gitdir:repo/.git"));
                assert!(!follows(&f, "gitdir:play/"));
            }

            #[test]
            fn should_expand_a_path_beside_the_config_file() {
                let f = Fixture::new();

                assert!(follows(&f, "gitdir:./work/"));
                assert!(!follows(&f, "gitdir:./play/"));
            }

            #[test]
            fn should_expand_home_and_match_the_git_dir_before_or_after_links() {
                let f = Fixture::new();
                let home = f.path("home");
                std::fs::create_dir_all(home.join("code")).unwrap();
                std::os::unix::fs::symlink(f.path("work"), home.join("code/work")).unwrap();
                let linked = home.join("code/work/repo/.git");
                write(&f.path("cond.inc"), "[a]\nb = 1\n");
                let follows_from = |condition: &str, git_dir: &Path| {
                    write(
                        &f.path("config"),
                        &format!("[includeIf \"{condition}\"]\npath = cond.inc\n"),
                    );
                    let context = Context {
                        git_dir,
                        home: Some(&home),
                    };
                    listed(&read(&f.path("config"), &context)).contains(&"a.b=1".to_owned())
                };

                assert!(follows_from("gitdir:~/code/work/", &linked));
                assert!(follows_from(
                    &format!("gitdir:{}/", f.path("work").display()),
                    &linked
                ));
                assert!(!follows_from("gitdir:~/code/work/", &f.git_dir()));
            }

            #[test]
            fn should_ignore_case_only_under_gitdir_i() {
                let f = Fixture::new();
                let upper = format!("{}/", f.path("WORK").display());

                assert!(follows(&f, &format!("gitdir/i:{upper}")));
                assert!(!follows(&f, &format!("gitdir:{upper}")));
            }

            #[test]
            fn should_follow_onbranch_when_the_checked_out_branch_matches() {
                let f = Fixture::new();
                write(
                    &f.git_dir().join("HEAD"),
                    "ref: refs/heads/feature/widgets\n",
                );

                assert!(follows(&f, "onbranch:feature/"));
                assert!(follows(&f, "onbranch:feature/w*"));
                assert!(follows(&f, "onbranch:feature/widgets"));
                assert!(!follows(&f, "onbranch:main"));
            }

            #[test]
            fn should_not_follow_onbranch_on_a_detached_head() {
                let f = Fixture::new();
                write(&f.git_dir().join("HEAD"), "0123456789abcdef\n");

                assert!(!follows(&f, "onbranch:**"));
            }

            #[test]
            fn should_not_follow_a_condition_it_does_not_know() {
                let f = Fixture::new();

                assert!(!follows(&f, "hasconfig:remote.*.url:https://github.com/**"));
            }
        }
    }
}
