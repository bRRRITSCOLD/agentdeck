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
}
