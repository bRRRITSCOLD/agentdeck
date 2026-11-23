//! Repositories (spec §3 `repositories`): GitHub's numeric id, which names a Repository for
//! good, and the `owner/name` it goes by now, which a rename or transfer changes.
//!
//! A Repository reference (spec §10) is `owner/name` or its GitHub URL; both parse as a
//! [`RepositoryName`].

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// GitHub's numeric id of a Repository (`fullDatabaseId`, the REST API's `id`): what keeps it
/// the same Repository through a rename or a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepositoryId(u64);

impl RepositoryId {
    /// The id as GitHub numbers it.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for RepositoryId {
    fn from(id: u64) -> Self {
        RepositoryId(id)
    }
}

impl fmt::Display for RepositoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for RepositoryId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(RepositoryId)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for RepositoryId {
    fn schema_name() -> String {
        "RepositoryId".to_owned()
    }

    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        crate::schema::unsigned_integer()
    }
}

/// What a Repository is called now: `owner/name`. Parsed from `owner/name` or the
/// Repository's GitHub URL (`https://github.com/owner/name`, with or without the scheme, a
/// trailing `/` or `.git`). Displays and serializes as `owner/name`.
///
/// GitHub matches names ignoring case; so does the daemon, while this type compares them as
/// written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryName {
    owner: String,
    name: String,
}

/// Text that names no Repository.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("`{0}` isn't a Repository: write owner/name or its GitHub URL")]
pub struct NotARepository(pub String);

impl RepositoryName {
    /// The account or organisation that owns it.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Its name within the owner's.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `owner/name` from its two parts, each as GitHub allows: the owner letters, digits, `-`
    /// and `_` (a managed user's); the name letters, digits, `-`, `_` and `.`, but not `.` or
    /// `..`.
    fn from_parts(owner: &str, name: &str) -> Option<RepositoryName> {
        let owner_ok = !owner.is_empty()
            && (owner.chars()).all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        let name_ok = !matches!(name, "" | "." | "..")
            && (name.chars())
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        (owner_ok && name_ok).then(|| RepositoryName {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// `owner/name` exactly: two parts, one `/` apart.
    fn plain(text: &str) -> Option<RepositoryName> {
        let (owner, name) = text.split_once('/')?;
        RepositoryName::from_parts(owner, name)
    }
}

impl fmt::Display for RepositoryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl FromStr for RepositoryName {
    type Err = NotARepository;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let trimmed = text.trim();
        let parsed = match github_path(trimmed) {
            Some(path) => {
                let path = path.strip_suffix('/').unwrap_or(path);
                RepositoryName::plain(path.strip_suffix(".git").unwrap_or(path))
            }
            None => RepositoryName::plain(trimmed),
        };
        parsed.ok_or_else(|| NotARepository(trimmed.to_owned()))
    }
}

impl Serialize for RepositoryName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RepositoryName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for RepositoryName {
    fn schema_name() -> String {
        "RepositoryName".to_owned()
    }

    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        crate::schema::string(
            "A Repository's `owner/name`. Given to the daemon, its GitHub URL also works.",
        )
    }
}

/// The path of a GitHub web URL (`https://github.com/<path>`; `http`, `www.` and no scheme at
/// all work too), without its query or fragment; `None` when `text` isn't one.
pub(crate) fn github_path(text: &str) -> Option<&str> {
    let rest = (text.strip_prefix("https://"))
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    let (host, path) = rest.split_once('/')?;
    if !(host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com")) {
        return None;
    }
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(&path[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> Result<RepositoryName, NotARepository> {
        text.parse()
    }

    mod repository_name {
        use super::*;

        #[test]
        fn should_parse_owner_slash_name() {
            let parsed = name("bRRRITSCOLD/agentdeck").expect("a Repository");

            assert_eq!(parsed.owner(), "bRRRITSCOLD");
            assert_eq!(parsed.name(), "agentdeck");
            assert_eq!(parsed.to_string(), "bRRRITSCOLD/agentdeck");
        }

        #[test]
        fn should_take_the_punctuation_github_allows() {
            assert_eq!(
                name("octo-org_emu/my.repo-name_2").unwrap().to_string(),
                "octo-org_emu/my.repo-name_2"
            );
        }

        #[test]
        fn should_ignore_surrounding_whitespace() {
            assert_eq!(
                name("  acme/widgets\n").unwrap().to_string(),
                "acme/widgets"
            );
        }

        #[test]
        fn should_parse_the_repositorys_url() {
            for url in [
                "https://github.com/acme/widgets",
                "http://github.com/acme/widgets",
                "https://www.github.com/acme/widgets",
                "github.com/acme/widgets",
                "https://GitHub.com/acme/widgets",
                "https://github.com/acme/widgets/",
                "https://github.com/acme/widgets.git",
                "https://github.com/acme/widgets?tab=readme#install",
            ] {
                assert_eq!(
                    name(url).map(|n| n.to_string()),
                    Ok("acme/widgets".into()),
                    "{url}"
                );
            }
        }

        #[test]
        fn should_reject_text_that_names_no_repository() {
            for text in [
                "",
                "acme",
                "acme/",
                "/widgets",
                "acme/widgets/extra",
                "acme/wid gets",
                "acme/..",
                "ac.me/widgets",
                "acme/widgets#7",
                "https://gitlab.com/acme/widgets",
                "https://github.com/acme",
                "https://github.com/acme/widgets/pull/7",
            ] {
                assert!(name(text).is_err(), "{text:?} parsed");
            }
        }

        #[test]
        fn should_say_what_to_write_instead() {
            assert_eq!(
                name("widgets").unwrap_err().to_string(),
                "`widgets` isn't a Repository: write owner/name or its GitHub URL"
            );
        }

        #[test]
        fn should_serialize_as_owner_slash_name() {
            let parsed = name("https://github.com/acme/widgets").unwrap();

            let json = serde_json::to_string(&parsed).unwrap();

            assert_eq!(json, r#""acme/widgets""#);
            assert_eq!(
                serde_json::from_str::<RepositoryName>(&json).unwrap(),
                parsed
            );
        }

        #[test]
        fn should_fail_to_deserialize_text_that_names_no_repository() {
            assert!(serde_json::from_str::<RepositoryName>(r#""widgets""#).is_err());
        }
    }

    mod repository_id {
        use super::*;

        #[test]
        fn should_serialize_as_a_number() {
            let id = RepositoryId::from(1_296_269);

            let json = serde_json::to_string(&id).unwrap();

            assert_eq!(json, "1296269");
            assert_eq!(serde_json::from_str::<RepositoryId>(&json).unwrap(), id);
        }
    }
}
