//! `terminal.file_opener` (spec §2, §11): the command a `secondary`-click on a file path in a
//! pane runs, such as `code -g {path}:{line}`.
//!
//! The command is split into words as a shell would split it (`shell-words`), once, and the
//! placeholders are filled into the words afterwards, so a path holding spaces or quotes stays
//! one argument and can't inject anything. It is never run through `sh -c`.

/// The opener when nothing sets one: the desktop's handler for the file.
pub const DEFAULT: &str = "xdg-open {path}";

/// The placeholder for the file's absolute path, which every opener needs.
pub const PATH: &str = "{path}";

/// The command a click on a file path runs, split into words with its placeholders unfilled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOpener {
    words: Vec<String>,
}

/// Why a command can't be a file opener; each reads as the end of a sentence about it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenerError {
    /// Nothing but spaces.
    #[error("must name a command")]
    Empty,
    /// It doesn't split as a shell would split it: an unclosed quote, or a trailing `\`.
    #[error("can't be split into words as a shell would: {0}")]
    Unsplittable(String),
    /// No argument after the command holds `{path}`.
    #[error("must pass {{path}} to the command, as in `code -g {{path}}:{{line}}`")]
    NoPath,
}

impl FileOpener {
    /// Splits `command` into words: the program, then its arguments. One argument at least
    /// must hold `{path}`; the program itself may not, so a click never runs the file clicked.
    pub fn parse(command: &str) -> Result<FileOpener, OpenerError> {
        let words = shell_words::split(command)
            .map_err(|err| OpenerError::Unsplittable(err.to_string()))?;
        let Some((_program, args)) = words.split_first() else {
            return Err(OpenerError::Empty);
        };
        if !args.iter().any(|arg| arg.contains(PATH)) {
            return Err(OpenerError::NoPath);
        }
        Ok(FileOpener { words })
    }

    /// The words, placeholders unfilled.
    pub fn words(&self) -> &[String] {
        &self.words
    }

    /// The program and its arguments for opening `path` (absolute) at `line` and `col`, each
    /// `1` when the path names none. Each placeholder is filled into its word as is, so a
    /// path holding spaces, quotes or `{line}` stays one argument, unchanged.
    pub fn argv(&self, path: &str, line: Option<u32>, col: Option<u32>) -> Vec<String> {
        let line = line.unwrap_or(1).to_string();
        let col = col.unwrap_or(1).to_string();
        let values = [(PATH, path), ("{line}", &line), ("{col}", &col)];
        self.words.iter().map(|word| fill(word, &values)).collect()
    }
}

/// `word` with each placeholder in `values` replaced, in one pass, so what a value holds is
/// never read as a placeholder.
fn fill(word: &str, values: &[(&str, &str)]) -> String {
    let mut filled = String::with_capacity(word.len());
    let mut rest = word;
    while !rest.is_empty() {
        match values
            .iter()
            .find(|(placeholder, _)| rest.starts_with(placeholder))
        {
            Some((placeholder, value)) => {
                filled.push_str(value);
                rest = &rest[placeholder.len()..];
            }
            None => {
                let next = rest.chars().next().expect("not empty");
                filled.push(next);
                rest = &rest[next.len_utf8()..];
            }
        }
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse {
        use super::*;

        fn words(command: &str) -> Vec<String> {
            FileOpener::parse(command)
                .expect("a valid opener")
                .words()
                .to_vec()
        }

        #[test]
        fn should_take_the_default() {
            assert_eq!(words(DEFAULT), ["xdg-open", "{path}"]);
        }

        #[test]
        fn should_split_as_a_shell_would() {
            assert_eq!(
                words(r#"kitty --title 'an editor' nvim "+{line}" {path}"#),
                ["kitty", "--title", "an editor", "nvim", "+{line}", "{path}"]
            );
            assert_eq!(
                words("code -g {path}:{line}:{col}"),
                ["code", "-g", "{path}:{line}:{col}"]
            );
        }

        #[test]
        fn should_refuse_a_command_of_nothing_but_spaces() {
            assert_eq!(FileOpener::parse(""), Err(OpenerError::Empty));
            assert_eq!(FileOpener::parse(" \t "), Err(OpenerError::Empty));
        }

        #[test]
        fn should_refuse_a_command_that_does_not_split() {
            assert!(matches!(
                FileOpener::parse("code 'oops {path}"),
                Err(OpenerError::Unsplittable(_))
            ));
        }

        #[test]
        fn should_refuse_a_command_without_path() {
            assert_eq!(FileOpener::parse("code -g"), Err(OpenerError::NoPath));
            assert_eq!(FileOpener::parse("code {line}"), Err(OpenerError::NoPath));
        }

        #[test]
        fn should_refuse_path_as_the_program() {
            assert_eq!(FileOpener::parse("{path}"), Err(OpenerError::NoPath));
            assert_eq!(FileOpener::parse("{path} --x"), Err(OpenerError::NoPath));
        }

        #[test]
        fn should_say_what_an_opener_needs() {
            assert_eq!(
                OpenerError::NoPath.to_string(),
                "must pass {path} to the command, as in `code -g {path}:{line}`"
            );
        }
    }

    mod argv {
        use super::*;

        fn argv(command: &str, path: &str, line: Option<u32>, col: Option<u32>) -> Vec<String> {
            FileOpener::parse(command)
                .expect("a valid opener")
                .argv(path, line, col)
        }

        #[test]
        fn should_fill_in_the_path_line_and_column() {
            assert_eq!(
                argv(
                    "code -g {path}:{line}:{col}",
                    "/w/src/main.rs",
                    Some(42),
                    Some(7)
                ),
                ["code", "-g", "/w/src/main.rs:42:7"]
            );
        }

        #[test]
        fn should_fill_in_1_for_a_line_or_column_the_path_does_not_name() {
            assert_eq!(
                argv("kitty nvim +{line} {path}", "/w/a.txt", None, None),
                ["kitty", "nvim", "+1", "/w/a.txt"]
            );
            assert_eq!(
                argv("code -g {path}:{line}:{col}", "/w/a.txt", Some(3), None),
                ["code", "-g", "/w/a.txt:3:1"]
            );
        }

        #[test]
        fn should_keep_a_path_with_spaces_and_quotes_one_argument() {
            assert_eq!(
                argv("xdg-open {path}", "/w/my notes/it's \"x\".md", None, None),
                ["xdg-open", "/w/my notes/it's \"x\".md"]
            );
        }

        #[test]
        fn should_not_fill_placeholders_inside_the_path() {
            assert_eq!(
                argv("code -g {path}:{line}", "/w/{line}/{col}.rs", Some(2), None),
                ["code", "-g", "/w/{line}/{col}.rs:2"]
            );
        }

        #[test]
        fn should_keep_quoted_words_and_other_braces_as_written() {
            assert_eq!(
                argv("ed --title 'a {b}' {path}", "/w/x", None, None),
                ["ed", "--title", "a {b}", "/w/x"]
            );
        }
    }
}
