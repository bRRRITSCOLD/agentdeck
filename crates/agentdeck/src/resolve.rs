//! Naming a workspace the way a person or an agent would: its id, its slug or its name.

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
}
