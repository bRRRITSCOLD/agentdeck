//! Task comments (spec §3, `comments.author`).

use crate::text_enum::text_enum;

text_enum! {
    /// Who wrote a task comment. Only the user's comments are delivered to the task's agent.
    pub enum CommentAuthor ("comment author") {
        /// The user, from the GUI or a shell outside any pane.
        User => "user",
        /// An agent, through the CLI or MCP from inside its pane.
        Agent => "agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod comment_author {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = CommentAuthor::ALL.iter().map(|a| a.as_str()).collect();
            assert_eq!(texts, ["user", "agent"]);
        }
    }
}
