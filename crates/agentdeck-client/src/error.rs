//! The daemon's error responses as messages a person can act on.

use std::fmt;

use crate::types;

/// What a bare 404 or 405 says: the daemon has no such route or method, so it predates the
/// client that called it (spec §2). dropshot answers an unknown route with no `error_code`,
/// and every domain failure carries one, so the two can't be confused.
pub const OLDER_DAEMON: &str = "agentdeck's background service is older than this app and \
    can't do this yet. Restart it to use new features; your agents resume after the restart.";

/// A failed API call: what the daemon said, with its `error_code` when it gave one.
#[derive(Debug)]
pub struct ApiError {
    /// The daemon's message, or the transport failure.
    pub message: String,
    /// The `error_code`, such as `not_found` or `dirty`.
    pub code: Option<String>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{} ({code})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ApiError {}

/// An error body the daemon sends: dropshot's, or one that extends it.
pub trait ErrorBody: fmt::Debug {
    /// The message and the `error_code`.
    fn parts(self) -> (String, Option<String>);
}

impl ErrorBody for types::Error {
    fn parts(self) -> (String, Option<String>) {
        (self.message, self.error_code)
    }
}

impl ErrorBody for types::DeleteWorkspaceError {
    fn parts(self) -> (String, Option<String>) {
        (self.message, self.error_code)
    }
}

impl<E: ErrorBody> From<crate::Error<E>> for ApiError {
    fn from(err: crate::Error<E>) -> ApiError {
        match err {
            crate::Error::ErrorResponse(response) => {
                let unrouted = matches!(response.status().as_u16(), 404 | 405);
                let (message, code) = response.into_inner().parts();
                match code {
                    None if unrouted => ApiError {
                        message: OLDER_DAEMON.to_owned(),
                        code: None,
                    },
                    code => ApiError { message, code },
                }
            }
            other => ApiError {
                message: format!("talking to agentdeckd: {other}"),
                code: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod api_error {
        use super::*;

        #[test]
        fn should_end_with_the_error_code() {
            let err = ApiError {
                message: "no such pane".into(),
                code: Some("not_found".into()),
            };

            assert_eq!(err.to_string(), "no such pane (not_found)");
        }

        fn error_response(status: u16, code: Option<&str>, message: &str) -> ApiError {
            let body = types::Error {
                error_code: code.map(str::to_owned),
                message: message.to_owned(),
                request_id: "r".into(),
            };
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            let response = progenitor_client::ResponseValue::new(body, status, Default::default());
            ApiError::from(crate::Error::ErrorResponse(response))
        }

        #[test]
        fn should_say_the_daemon_is_older_for_a_route_it_doesnt_have_404() {
            let err = error_response(404, None, "Not Found");

            assert_eq!(err.message, OLDER_DAEMON);
            assert_eq!(err.code, None);
        }

        #[test]
        fn should_say_the_daemon_is_older_for_a_method_it_doesnt_have_405() {
            let err = error_response(405, None, "Method Not Allowed");

            assert_eq!(err.message, OLDER_DAEMON);
        }

        #[test]
        fn should_keep_the_daemons_words_for_a_missing_entity_404() {
            let err = error_response(404, Some("not_found"), "no pane p");

            assert_eq!(err.to_string(), "no pane p (not_found)");
        }

        #[test]
        fn should_be_the_message_alone_without_a_code() {
            let err = ApiError {
                message: "connection refused".into(),
                code: None,
            };

            assert_eq!(err.to_string(), "connection refused");
        }
    }
}
