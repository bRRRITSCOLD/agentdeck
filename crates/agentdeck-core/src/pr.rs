//! Pull request fields synced from GitHub (spec §3 `pull_requests`, §8).

use crate::task::TaskStatus;
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

/// The status PR sync moves a PR task to when it sees the PR in state `now`, having last seen
/// it in `before` (`None`: seen for the first time). `None` leaves the task's status alone, so
/// a status the user picked for an open PR sticks until the PR's state changes.
///
/// - A new PR starts in `review`, or in `done` when it's already merged or closed.
/// - A PR that merges or closes moves its task to `done`.
/// - A PR that reopens moves its task back to `review`.
pub fn pr_task_status(before: Option<PrState>, now: PrState) -> Option<TaskStatus> {
    let finished = |state| matches!(state, PrState::Merged | PrState::Closed);
    match before {
        None if finished(now) => Some(TaskStatus::Done),
        None => Some(TaskStatus::Review),
        Some(before) if finished(before) == finished(now) => None,
        Some(_) if finished(now) => Some(TaskStatus::Done),
        Some(_) => Some(TaskStatus::Review),
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

    mod pr_task_status {
        use super::*;

        #[test]
        fn should_start_a_new_open_pr_in_review() {
            assert_eq!(
                pr_task_status(None, PrState::Open),
                Some(TaskStatus::Review)
            );
        }

        #[test]
        fn should_start_a_new_merged_or_closed_pr_in_done() {
            assert_eq!(
                pr_task_status(None, PrState::Merged),
                Some(TaskStatus::Done)
            );
            assert_eq!(
                pr_task_status(None, PrState::Closed),
                Some(TaskStatus::Done)
            );
        }

        #[test]
        fn should_move_to_done_when_the_pr_merges_or_closes() {
            let open = Some(PrState::Open);
            assert_eq!(
                pr_task_status(open, PrState::Merged),
                Some(TaskStatus::Done)
            );
            assert_eq!(
                pr_task_status(open, PrState::Closed),
                Some(TaskStatus::Done)
            );
        }

        #[test]
        fn should_move_back_to_review_when_the_pr_reopens() {
            assert_eq!(
                pr_task_status(Some(PrState::Closed), PrState::Open),
                Some(TaskStatus::Review)
            );
        }

        #[test]
        fn should_leave_the_status_alone_while_the_state_holds() {
            for state in PrState::ALL {
                assert_eq!(pr_task_status(Some(*state), *state), None, "{state:?}");
            }
        }
    }
}
