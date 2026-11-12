//! Pull request fields synced from GitHub (spec §3 `pull_requests`, §8).

use crate::text_enum::text_enum;

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

#[cfg(test)]
mod tests {
    use super::*;

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
