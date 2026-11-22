//! What the session screen says about a failure, and about omp retrying while it works.

use ch_core::{Failure, FailureKind, Retrying};

use crate::driver::FIRST_REPLY_MS;

const TRANSIENT_HINT: &str = "The provider may accept it later. Send the prompt again to retry.";
const NEEDS_HUMAN_HINT: &str =
    "Check the provider key, the account or omp's cost cap, then send the prompt again.";
const RETRY_BODY: &str = "omp is sending the model call again.";
const FIRST_REPLY_BODY: &str = "The provider may be slow or unreachable, or an SSH link may have \
     stalled. Keep waiting, or stop the agent and send the prompt again.";
/// Shown under a run that ended with nothing in it, in a session that keeps checkpoints.
pub const EMPTY_RUN_NOTE: &str =
    "The model sent no reply and called no tools, so no checkpoint was saved.";

/// A failure banner: what happened, the error or what to do next, and a hint when both are needed.
#[derive(Debug, PartialEq, Eq)]
pub struct FailureText {
    pub title: String,
    pub body: String,
    pub hint: Option<&'static str>,
}

/// The words a banner uses for a runner stage. Most stage names already read as words.
fn stage_label(stage: &str) -> &str {
    match stage {
        "repo" => "the repository check",
        other => other,
    }
}

pub fn failure_text(failure: &Failure) -> FailureText {
    let stage = stage_label(&failure.stage);
    let (title, body, hint) = match failure.kind {
        FailureKind::Transient => (
            format!("Gave up: {}", failure.message),
            TRANSIENT_HINT.to_owned(),
            None,
        ),
        FailureKind::NeedsHuman => (
            format!("Action needed during {stage}"),
            failure.message.clone(),
            Some(NEEDS_HUMAN_HINT),
        ),
        FailureKind::Permanent => (
            format!("Failed during {stage}"),
            failure.message.clone(),
            None,
        ),
    };
    FailureText { title, body, hint }
}

/// The title and body of the banner shown while omp retries a model call.
pub fn retry_text(retrying: &Retrying) -> (String, String) {
    let title = match (retrying.attempt, retrying.max_attempts) {
        (Some(attempt), Some(max)) => format!("Retrying {attempt}/{max}"),
        (Some(attempt), None) => format!("Retrying, attempt {attempt}"),
        _ => "Retrying".to_owned(),
    };
    let body = if retrying.reason.is_empty() {
        RETRY_BODY.to_owned()
    } else {
        retrying.reason.clone()
    };
    (title, body)
}

/// The title and body of the banner shown once the model has been silent past the deadline.
pub fn first_reply_text() -> (String, &'static str) {
    let seconds = FIRST_REPLY_MS / 1000;
    let title = format!("No reply from the model in {seconds} seconds");
    (title, FIRST_REPLY_BODY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(kind: FailureKind) -> Failure {
        Failure {
            stage: "prompt".into(),
            kind,
            message: "529 overloaded".into(),
        }
    }

    fn retrying(attempt: Option<u32>, max_attempts: Option<u32>, reason: &str) -> Retrying {
        Retrying {
            attempt,
            max_attempts,
            reason: reason.into(),
        }
    }

    #[test]
    fn a_transient_failure_says_it_gave_up_and_that_later_may_work() {
        let text = failure_text(&failure(FailureKind::Transient));
        assert_eq!(text.title, "Gave up: 529 overloaded");
        assert_eq!(
            text.body, TRANSIENT_HINT,
            "the error is in the title already"
        );
        assert_eq!(text.hint, None);
    }

    #[test]
    fn a_failure_that_needs_a_person_says_what_to_check() {
        let text = failure_text(&failure(FailureKind::NeedsHuman));
        assert_eq!(text.title, "Action needed during prompt");
        assert_eq!(text.body, "529 overloaded");
        assert_eq!(text.hint, Some(NEEDS_HUMAN_HINT));
    }

    #[test]
    fn a_permanent_failure_offers_no_retry() {
        let text = failure_text(&failure(FailureKind::Permanent));
        assert_eq!(
            text,
            FailureText {
                title: "Failed during prompt".into(),
                body: "529 overloaded".into(),
                hint: None,
            }
        );
    }

    #[test]
    fn a_failed_repository_check_is_named_in_words() {
        let repo = Failure {
            stage: "repo".into(),
            kind: FailureKind::Permanent,
            message: "git rev-parse HEAD did not finish within 30 seconds.".into(),
        };
        assert_eq!(
            failure_text(&repo).title,
            "Failed during the repository check"
        );
    }

    #[test]
    fn a_retry_shows_omps_count_when_it_has_one() {
        let cases = [
            (retrying(Some(2), Some(3), "529"), "Retrying 2/3"),
            (retrying(Some(2), None, "529"), "Retrying, attempt 2"),
            (retrying(None, Some(3), "529"), "Retrying"),
            (retrying(None, None, "529"), "Retrying"),
        ];
        for (retry, title) in cases {
            assert_eq!(retry_text(&retry), (title.to_owned(), "529".to_owned()));
        }
    }

    #[test]
    fn a_retry_without_a_reason_still_says_what_is_happening() {
        assert_eq!(retry_text(&retrying(Some(1), Some(3), "")).1, RETRY_BODY);
    }

    #[test]
    fn a_late_first_reply_names_the_deadline_the_driver_uses() {
        let (title, body) = first_reply_text();
        assert_eq!(title, "No reply from the model in 90 seconds");
        assert!(body.contains("stop the agent"), "{body}");
    }
}
