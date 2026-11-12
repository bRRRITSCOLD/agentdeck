//! The bearer token every request carries (spec §2).

/// A fresh token: 122 random bits from a v4 UUID, as 32 hex digits.
pub fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Whether an `Authorization` header value is `Bearer <token>`.
pub fn bearer_matches(header: Option<&[u8]>, token: &str) -> bool {
    let Some(given) = header.and_then(|value| value.strip_prefix(b"Bearer ")) else {
        return false;
    };
    !token.is_empty() && constant_time_eq(given, token.as_bytes())
}

/// Compares without stopping at the first differing byte, so timing doesn't leak a prefix.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    mod bearer_matches {
        use super::*;

        #[test]
        fn should_accept_the_token_after_bearer() {
            assert!(bearer_matches(Some(b"Bearer abc123"), "abc123"));
        }

        #[test]
        fn should_reject_a_missing_header() {
            assert!(!bearer_matches(None, "abc123"));
        }

        #[test]
        fn should_reject_another_token() {
            assert!(!bearer_matches(Some(b"Bearer abc124"), "abc123"));
        }

        #[test]
        fn should_reject_a_prefix_of_the_token() {
            assert!(!bearer_matches(Some(b"Bearer abc"), "abc123"));
        }

        #[test]
        fn should_reject_the_token_without_the_bearer_scheme() {
            assert!(!bearer_matches(Some(b"abc123"), "abc123"));
        }

        #[test]
        fn should_reject_an_empty_token() {
            assert!(!bearer_matches(Some(b"Bearer "), ""));
        }
    }

    mod new_token {
        use super::*;

        #[test]
        fn should_differ_between_calls() {
            assert_ne!(new_token(), new_token());
        }
    }
}
