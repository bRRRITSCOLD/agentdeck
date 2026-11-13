//! The daemon's error responses as messages a person can act on.

use std::fmt;

use crate::types;

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
                let (message, code) = response.into_inner().parts();
                ApiError { message, code }
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
                message: "no such task".into(),
                code: Some("not_found".into()),
            };

            assert_eq!(err.to_string(), "no such task (not_found)");
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
