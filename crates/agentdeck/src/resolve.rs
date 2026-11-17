//! Naming a workspace the way a person or an agent would: its id, its slug or its name; and a
//! pane or task by its id or a short id (spec §10).

use std::fmt::Display;
use std::str::FromStr;

use agentdeck_client::types::Workspace;
use agentdeck_core::WorkspaceId;

/// Finds the workspace `reference` names among `workspaces`: by id, then slug, then name.
/// A name two workspaces share is ambiguous; use the slug.
pub fn workspace(workspaces: &[Workspace], reference: &str) -> anyhow::Result<WorkspaceId> {
    let by = |matches: &dyn Fn(&Workspace) -> bool| -> Vec<&Workspace> {
        workspaces.iter().filter(|ws| matches(ws)).collect()
    };
    for found in [
        by(&|ws| ws.id.to_string() == reference),
        by(&|ws| ws.slug == reference),
        by(&|ws| ws.name == reference),
    ] {
        match found.as_slice() {
            [] => continue,
            [ws] => return Ok(ws.id),
            several => {
                let slugs: Vec<&str> = several.iter().map(|ws| ws.slug.as_str()).collect();
                anyhow::bail!(
                    "`{reference}` names {} workspaces; use a slug: {}",
                    several.len(),
                    slugs.join(", ")
                );
            }
        }
    }
    anyhow::bail!("no workspace `{reference}` (unknown_workspace)")
}

/// The fewest hex digits a short id may have.
pub const SHORT_ID_DIGITS: usize = 6;

/// What a pane or task reference is: a whole id, or the hex digits of a short one.
#[derive(Debug, PartialEq, Eq)]
pub enum IdRef<I> {
    /// A whole id, sent to the daemon as it is.
    Whole(I),
    /// At least [`SHORT_ID_DIGITS`] hex digits in a row of an id, lowercased, without dashes.
    Short(String),
}

/// Reads `reference` as a whole `kind` id, else as a short one: 6 or more of its hex digits in
/// a row, dashes ignored. A UUIDv7's leading digits are its creation time, so its random tail
/// is what tells ids made close together apart.
pub fn id_ref<I: FromStr>(kind: &str, reference: &str) -> anyhow::Result<IdRef<I>> {
    if let Ok(id) = reference.parse() {
        return Ok(IdRef::Whole(id));
    }
    let digits: String = reference
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    if digits.len() < SHORT_ID_DIGITS || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!(
            "`{reference}` isn't a {kind} id: give the whole id, or {SHORT_ID_DIGITS} or more \
             of its hex digits in a row, such as its last {SHORT_ID_DIGITS} (invalid_id)"
        );
    }
    Ok(IdRef::Short(digits))
}

/// Finds the one id among `candidates` whose hex digits hold `digits`, a short id from
/// [`id_ref`]. Each candidate carries what to call it when several match; `reference` is what
/// the caller typed, for the errors.
pub fn by_short_id<I: Copy + Display>(
    kind: &str,
    candidates: impl IntoIterator<Item = (I, String)>,
    reference: &str,
    digits: &str,
) -> anyhow::Result<I> {
    let matches: Vec<(I, String)> = candidates
        .into_iter()
        .filter(|(id, _)| id.to_string().replace('-', "").contains(digits))
        .collect();
    match matches.as_slice() {
        [] => anyhow::bail!("no {kind} id holds `{reference}` (not_found)"),
        [(id, _)] => Ok(*id),
        several => {
            let lines: Vec<String> = several
                .iter()
                .map(|(id, label)| format!("  {id}  {label}"))
                .collect();
            anyhow::bail!(
                "`{reference}` is in {} {kind} ids; give more of the id (ambiguous_id):\n{}",
                several.len(),
                lines.join("\n")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: &str, slug: &str, name: &str) -> Workspace {
        Workspace {
            archived_at: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            dir: format!("/work/{slug}"),
            dir_owned: false,
            id: id.parse().unwrap(),
            muted: false,
            name: name.into(),
            slug: slug.into(),
        }
    }

    const A: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000001";
    const B: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000002";
    const C: &str = "0190a1b2-c3d4-7e5f-8a9b-000000000003";

    fn workspaces() -> Vec<Workspace> {
        vec![
            ws(A, "web", "Web app"),
            ws(B, "api", "Shared"),
            ws(C, "api-2", "Shared"),
        ]
    }

    mod workspace {
        use super::*;

        #[test]
        fn should_find_by_id() {
            assert_eq!(workspace(&workspaces(), B).unwrap(), B.parse().unwrap());
        }

        #[test]
        fn should_find_by_slug() {
            assert_eq!(workspace(&workspaces(), "web").unwrap(), A.parse().unwrap());
        }

        #[test]
        fn should_find_by_name() {
            assert_eq!(
                workspace(&workspaces(), "Web app").unwrap(),
                A.parse().unwrap()
            );
        }

        #[test]
        fn should_prefer_a_slug_over_another_workspaces_name() {
            let list = vec![ws(A, "shared", "Other"), ws(B, "b", "shared")];

            assert_eq!(workspace(&list, "shared").unwrap(), A.parse().unwrap());
        }

        #[test]
        fn should_name_the_slugs_of_an_ambiguous_name() {
            let err = workspace(&workspaces(), "Shared").unwrap_err().to_string();

            assert!(err.contains("api, api-2"), "{err}");
        }

        #[test]
        fn should_fail_for_an_unknown_workspace() {
            let err = workspace(&workspaces(), "nope").unwrap_err().to_string();

            assert!(err.contains("unknown_workspace"), "{err}");
        }
    }

    mod id_ref {
        use super::*;
        use agentdeck_core::TaskId;

        const ID: &str = "01a1e67e-90c9-73fc-8a9b-0c1d2e3f4a5b";

        #[test]
        fn should_take_a_whole_id_as_it_is() {
            assert_eq!(
                id_ref::<TaskId>("task", ID).unwrap(),
                IdRef::Whole(ID.parse().unwrap())
            );
        }

        #[test]
        fn should_take_six_or_more_hex_digits_as_a_short_id() {
            assert_eq!(
                id_ref::<TaskId>("task", "2E3F4A5B").unwrap(),
                IdRef::Short("2e3f4a5b".into())
            );
        }

        #[test]
        fn should_ignore_dashes_in_a_short_id() {
            assert_eq!(
                id_ref::<TaskId>("task", "8a9b-0c1d").unwrap(),
                IdRef::Short("8a9b0c1d".into())
            );
        }

        #[test]
        fn should_reject_fewer_than_six_digits() {
            let err = id_ref::<TaskId>("task", "4a5b").unwrap_err().to_string();

            assert!(err.contains("invalid_id"), "{err}");
            assert!(err.contains("`4a5b` isn't a task id"), "{err}");
        }

        #[test]
        fn should_reject_what_isnt_hex() {
            let err = id_ref::<TaskId>("pane", "pane-one")
                .unwrap_err()
                .to_string();

            assert!(err.contains("invalid_id"), "{err}");
        }
    }

    mod by_short_id {
        use super::*;
        use agentdeck_core::{PaneId, TaskId};

        // Made a second apart: the same leading digits, as UUIDv7s made close together have.
        const FIRST: &str = "01a1e67e-90c9-73fc-8a9b-0c1d2e3f4a5b";
        const SECOND: &str = "01a1e67e-9102-748e-9f00-77aa2e3f4a5c";

        fn tasks() -> Vec<(TaskId, String)> {
            vec![
                (FIRST.parse().unwrap(), "Fix the build".into()),
                (SECOND.parse().unwrap(), "Ship it".into()),
            ]
        }

        #[test]
        fn should_find_the_one_id_that_ends_with_the_digits() {
            assert_eq!(
                by_short_id("task", tasks(), "2e3f4a5b", "2e3f4a5b").unwrap(),
                FIRST.parse().unwrap()
            );
        }

        #[test]
        fn should_find_digits_anywhere_in_the_id() {
            assert_eq!(
                by_short_id("task", tasks(), "9102748e", "9102748e").unwrap(),
                SECOND.parse().unwrap()
            );
        }

        #[test]
        fn should_list_every_id_when_the_digits_are_in_several() {
            let err = by_short_id("task", tasks(), "01a1e67e", "01a1e67e")
                .unwrap_err()
                .to_string();

            assert!(err.contains("ambiguous_id"), "{err}");
            assert!(err.contains(&format!("{FIRST}  Fix the build")), "{err}");
            assert!(err.contains(&format!("{SECOND}  Ship it")), "{err}");
        }

        #[test]
        fn should_say_when_no_id_holds_the_digits() {
            let err = by_short_id("task", tasks(), "abcdef", "abcdef")
                .unwrap_err()
                .to_string();

            assert!(err.contains("no task id holds `abcdef`"), "{err}");
            assert!(err.contains("not_found"), "{err}");
        }

        #[test]
        fn should_find_a_pane_by_its_short_id() {
            let panes: Vec<(PaneId, String)> = vec![
                (FIRST.parse().unwrap(), "web · tab".into()),
                (SECOND.parse().unwrap(), "web · tab".into()),
            ];

            assert_eq!(
                by_short_id("pane", panes, "a2e3f4a5c", "a2e3f4a5c").unwrap(),
                SECOND.parse().unwrap()
            );
        }
    }
}
