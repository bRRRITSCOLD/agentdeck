//! Pull request fields synced from GitHub (spec §3 `pull_requests`, §8), and the reference
//! that names a pull request (spec §10).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::repository::{RepositoryName, github_path};
use crate::text_enum::text_enum;

/// A pull request's reference, GitHub's way: `owner/name#123`. Parsed from that or the pull
/// request's GitHub URL (`https://github.com/owner/name/pull/123`, also with a tab such as
/// `/files` after it, a query or a fragment). Displays and serializes as `owner/name#123`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrRef {
    repository: RepositoryName,
    number: u32,
}

/// Text that names no pull request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("`{0}` isn't a pull request: write owner/name#123 or its GitHub URL")]
pub struct NotAPr(pub String);

impl PrRef {
    /// Pull request `number` of `repository`. GitHub numbers them from 1, well within `u32`.
    pub fn new(repository: RepositoryName, number: u32) -> PrRef {
        PrRef { repository, number }
    }

    /// The Repository it belongs to, by the name the reference gives.
    pub fn repository(&self) -> &RepositoryName {
        &self.repository
    }

    /// Its number in that Repository.
    pub fn number(&self) -> u32 {
        self.number
    }
}

/// A pull request number: digits only, from 1.
fn number(text: &str) -> Option<u32> {
    let all_digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    all_digits
        .then(|| text.parse().ok())
        .flatten()
        .filter(|n| *n > 0)
}

impl fmt::Display for PrRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repository, self.number)
    }
}

impl FromStr for PrRef {
    type Err = NotAPr;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let trimmed = text.trim();
        let parsed = match github_path(trimmed) {
            // `owner/name/pull/123`, then anything: `/files`, `/commits/<sha>`.
            Some(path) => {
                let mut parts = path.split('/');
                match (parts.next(), parts.next(), parts.next(), parts.next()) {
                    (Some(owner), Some(name), Some("pull"), Some(n)) => {
                        let repository = format!("{owner}/{name}").parse().ok();
                        repository.zip(number(n)).map(|(r, n)| PrRef::new(r, n))
                    }
                    _ => None,
                }
            }
            None => trimmed.rsplit_once('#').and_then(|(repository, n)| {
                let repository = RepositoryName::from_str(repository).ok();
                repository.zip(number(n)).map(|(r, n)| PrRef::new(r, n))
            }),
        };
        parsed.ok_or_else(|| NotAPr(trimmed.to_owned()))
    }
}

impl Serialize for PrRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PrRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for PrRef {
    fn schema_name() -> String {
        "PrRef".to_owned()
    }

    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        crate::schema::string(
            "A pull request's reference, `owner/name#123`. Given to the daemon, its GitHub URL \
             also works.",
        )
    }
}

text_enum! {
    /// A pull request's state on GitHub.
    pub enum PrState ("pull request state") {
        /// Open, draft or not.
        Open => "open",
        /// Merged.
        Merged => "merged",
        /// Closed without merging.
        Closed => "closed",
    }
}

text_enum! {
    /// The rolled-up result of a pull request's checks (`statusCheckRollup`).
    pub enum ChecksState ("checks state") {
        /// At least one check is still running.
        Pending => "pending",
        /// Every check passed.
        Success => "success",
        /// At least one check failed.
        Failure => "failure",
    }
}

text_enum! {
    /// A pull request's review decision (`reviewDecision`).
    pub enum ReviewState ("review state") {
        /// Approved.
        Approved => "approved",
        /// A reviewer asked for changes.
        ChangesRequested => "changes_requested",
        /// A review is required and none has been given.
        ReviewRequired => "review_required",
    }
}

/// What **Copy PR details** puts on the clipboard for one pull request (spec §11): three lines
/// of plain text that also read as markdown. Its reference and title, its GitHub page, then the
/// branch it merges from and the one it merges into:
///
/// ```text
/// bRRRITSCOLD/agentdeck#352: Fix pane restore
/// https://github.com/bRRRITSCOLD/agentdeck/pull/352
/// fix/pane-restore → main
/// ```
///
/// An agent the text is pasted to fetches the rest from GitHub itself.
pub fn pr_details(reference: &PrRef, title: &str, url: &str, head: &str, base: &str) -> String {
    format!("{reference}: {title}\n{url}\n{head} → {base}")
}

/// What **Copy PR details** puts on the clipboard for several pull requests at once, from Select
/// mode (spec §11): each one's [`pr_details`], in the order given, a blank line between them.
pub fn prs_details<S: AsRef<str>>(blocks: impl IntoIterator<Item = S>) -> String {
    let blocks: Vec<S> = blocks.into_iter().collect();
    let blocks: Vec<&str> = blocks.iter().map(AsRef::as_ref).collect();
    blocks.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    mod pr_ref {
        use super::*;

        fn pr(text: &str) -> Result<PrRef, NotAPr> {
            text.parse()
        }

        #[test]
        fn should_parse_owner_slash_name_hash_number() {
            let parsed = pr("bRRRITSCOLD/agentdeck#357").expect("a PR");

            assert_eq!(parsed.repository().to_string(), "bRRRITSCOLD/agentdeck");
            assert_eq!(parsed.number(), 357);
            assert_eq!(parsed.to_string(), "bRRRITSCOLD/agentdeck#357");
        }

        #[test]
        fn should_parse_the_pull_requests_url() {
            for url in [
                "https://github.com/acme/widgets/pull/7",
                "http://github.com/acme/widgets/pull/7",
                "github.com/acme/widgets/pull/7",
                "https://www.github.com/acme/widgets/pull/7/",
                "https://github.com/acme/widgets/pull/7/files",
                "https://github.com/acme/widgets/pull/7/commits/0a1b2c3",
                "https://github.com/acme/widgets/pull/7#issuecomment-123",
                "https://github.com/acme/widgets/pull/7?w=1",
                "  https://github.com/acme/widgets/pull/7\n",
            ] {
                assert_eq!(
                    pr(url).map(|p| p.to_string()),
                    Ok("acme/widgets#7".into()),
                    "{url}"
                );
            }
        }

        #[test]
        fn should_reject_text_that_names_no_pull_request() {
            for text in [
                "",
                "acme/widgets",
                "acme/widgets#",
                "acme/widgets#0",
                "acme/widgets#+7",
                "acme/widgets#7a",
                "acme/widgets#99999999999",
                "#7",
                "widgets#7",
                "019a1dd1-5b85-75ae-af0b-6d1a8b211cc0",
                "https://github.com/acme/widgets",
                "https://github.com/acme/widgets/issues/7",
                "https://github.com/acme/widgets/pull/",
                "https://github.com/acme/widgets/pull/seven",
                "https://github.com/acme/widgets#7",
                "github.com/acme/widgets#7",
                "https://gitlab.com/acme/widgets/pull/7",
            ] {
                assert!(pr(text).is_err(), "{text:?} parsed");
            }
        }

        #[test]
        fn should_say_what_to_write_instead() {
            assert_eq!(
                pr("acme/widgets").unwrap_err().to_string(),
                "`acme/widgets` isn't a pull request: write owner/name#123 or its GitHub URL"
            );
        }

        #[test]
        fn should_serialize_as_its_reference() {
            let parsed = pr("https://github.com/acme/widgets/pull/7/files").unwrap();

            let json = serde_json::to_string(&parsed).unwrap();

            assert_eq!(json, r#""acme/widgets#7""#);
            assert_eq!(serde_json::from_str::<PrRef>(&json).unwrap(), parsed);
        }
    }

    mod pr_details {
        use super::*;

        #[test]
        fn should_give_the_reference_and_title_the_url_then_head_into_base() {
            let pr: PrRef = "bRRRITSCOLD/agentdeck#352".parse().unwrap();

            let details = pr_details(
                &pr,
                "Fix pane restore",
                "https://github.com/bRRRITSCOLD/agentdeck/pull/352",
                "fix/pane-restore",
                "main",
            );

            assert_eq!(
                details,
                "bRRRITSCOLD/agentdeck#352: Fix pane restore\n\
                 https://github.com/bRRRITSCOLD/agentdeck/pull/352\n\
                 fix/pane-restore → main"
            );
        }
    }

    mod prs_details {
        use super::*;

        #[test]
        fn should_join_each_prs_three_lines_with_a_blank_line() {
            let blocks = [
                "a#1: One\nhttps://x/1\nb → main",
                "a#2: Two\nhttps://x/2\nc → main",
            ];

            assert_eq!(
                prs_details(blocks),
                "a#1: One\nhttps://x/1\nb → main\n\na#2: Two\nhttps://x/2\nc → main"
            );
        }

        #[test]
        fn should_give_one_prs_lines_alone() {
            assert_eq!(prs_details(["a#1: One"]), "a#1: One");
        }
    }

    mod pr_state {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = PrState::ALL.iter().map(|s| s.as_str()).collect();
            assert_eq!(texts, ["open", "merged", "closed"]);
        }
    }

    mod checks_state {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = ChecksState::ALL.iter().map(|s| s.as_str()).collect();
            assert_eq!(texts, ["pending", "success", "failure"]);
        }
    }

    mod review_state {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = ReviewState::ALL.iter().map(|s| s.as_str()).collect();
            assert_eq!(texts, ["approved", "changes_requested", "review_required"]);
        }
    }
}
