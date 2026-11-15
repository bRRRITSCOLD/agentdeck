//! Where the GUI keeps its own state (spec §11): `window.json` and its log.

use std::ffi::OsString;
use std::path::PathBuf;

/// `$XDG_STATE_HOME/agentdeck`, else `~/.local/state/agentdeck`. `None` without an absolute
/// `XDG_STATE_HOME` or `HOME`: the window then isn't remembered and the log goes to stderr
/// alone. A relative value is ignored, as the XDG spec requires.
pub fn state_dir(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let state =
        absolute("XDG_STATE_HOME").or_else(|| Some(absolute("HOME")?.join(".local/state")))?;
    Some(state.join("agentdeck"))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod state_dir {
        use super::*;

        fn vars(
            pairs: &'static [(&'static str, &'static str)],
        ) -> impl Fn(&str) -> Option<OsString> {
            move |name| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        }

        #[test]
        fn should_be_under_xdg_state_home() {
            let dir = state_dir(vars(&[("XDG_STATE_HOME", "/state"), ("HOME", "/home/me")]));
            assert_eq!(dir, Some(PathBuf::from("/state/agentdeck")));
        }

        #[test]
        fn should_default_to_the_local_state_dir() {
            let dir = state_dir(vars(&[("HOME", "/home/me")]));
            assert_eq!(dir, Some(PathBuf::from("/home/me/.local/state/agentdeck")));
        }

        #[test]
        fn should_ignore_a_relative_xdg_state_home() {
            let dir = state_dir(vars(&[("XDG_STATE_HOME", "state"), ("HOME", "/home/me")]));
            assert_eq!(dir, Some(PathBuf::from("/home/me/.local/state/agentdeck")));
        }

        #[test]
        fn should_be_none_without_a_home() {
            assert_eq!(state_dir(vars(&[])), None);
        }
    }
}
