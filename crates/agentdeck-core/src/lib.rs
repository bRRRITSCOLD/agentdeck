//! GPUI-free domain types for agentdeck: ids, states and the state rules engine.
//! No IO, no GPUI, no tokio.

/// What `<program> --version` prints: the program's name, then the workspace version, which
/// every agentdeck program shares.
pub fn version_line(program: &str) -> String {
    format!("{program} {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod version_line {
        use super::*;

        #[test]
        fn should_name_the_program_then_the_workspace_version() {
            assert_eq!(
                version_line("agentdeckd"),
                concat!("agentdeckd ", env!("CARGO_PKG_VERSION"))
            );
        }
    }
}
