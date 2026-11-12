//! The GitHub token, from `gh auth token` (spec §8). The daemon stores no credentials.

use std::process::{Command, Output};

/// Why there's no token: `gh` is missing, not logged in, or printed nothing.
#[derive(Debug, thiserror::Error)]
#[error("`gh auth token` failed: {0}")]
pub struct NoToken(String);

/// Runs `gh auth token` on a blocking thread and returns the token it prints.
pub async fn gh_auth_token() -> Result<String, NoToken> {
    tokio::task::spawn_blocking(|| {
        let output = Command::new("gh")
            .args(["auth", "token"])
            .output()
            .map_err(|err| NoToken(err.to_string()))?;
        token_from(&output)
    })
    .await
    .map_err(|err| NoToken(err.to_string()))?
}

/// The token in `gh auth token`'s output.
fn token_from(output: &Output) -> Result<String, NoToken> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(NoToken(if stderr.is_empty() {
            output.status.to_string()
        } else {
            stderr
        }));
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if token.is_empty() {
        return Err(NoToken("it printed no token".into()));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            // A wait status holds the exit code in its second byte.
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    mod token_from {
        use super::*;

        #[test]
        fn should_trim_the_printed_token() {
            assert_eq!(token_from(&output(0, "gho_abc\n", "")).unwrap(), "gho_abc");
        }

        #[test]
        fn should_fail_with_gh_stderr_when_logged_out() {
            let err =
                token_from(&output(1, "", "no oauth token found for github.com\n")).unwrap_err();
            assert_eq!(
                err.to_string(),
                "`gh auth token` failed: no oauth token found for github.com"
            );
        }

        #[test]
        fn should_fail_on_empty_output() {
            assert!(token_from(&output(0, "\n", "")).is_err());
        }
    }
}
