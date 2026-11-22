//! What kind of failure a session ended in, and omp's own retries while it works.
//!
//! omp owns the retry budget: it retries a provider error itself (`retry.maxRetries` in its
//! settings) and reports each attempt with `auto_retry_start`. The app only shows that count,
//! and after a failure says whether trying again can help. It never retries on its own.

use serde_json::Value;

// A key omp cannot use (401), a provider that wants payment (402), or one that refuses the
// request (403): a person has to change something before a retry can work.
const HUMAN_HTTP_STATUSES: &[u64] = &[401, 402, 403];
const COST_CAP_CODE: &str = "cost_cap";

/// Whether trying the same thing again can help.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// omp said a later retry may work. Its own retries are already used up.
    Transient,
    /// Trying again fails the same way, or nothing said otherwise.
    Permanent,
    /// A person has to act first: a cost cap, a key, or a provider account.
    NeedsHuman,
}

/// omp is retrying a failed model call. Cleared when the run goes on or ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retrying {
    /// omp's attempt number, from 1.
    pub attempt: Option<u32>,
    /// omp's budget for this error.
    pub max_attempts: Option<u32>,
    /// The error that started this retry; empty when omp gave none.
    pub reason: String,
}

impl Retrying {
    /// Reads an `auto_retry_start` frame.
    pub(crate) fn from_frame(frame: &Value) -> Self {
        Self {
            attempt: count(&frame["attempt"]),
            max_attempts: count(&frame["maxAttempts"]),
            reason: frame["errorMessage"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        }
    }
}

fn count(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// The kind of a `prompt_result` error. omp sends an object with `retryable`, `httpStatus` and
/// `code`; anything else, a bare string included, is permanent.
pub(crate) fn prompt_error_kind(error: Option<&Value>) -> FailureKind {
    let Some(Value::Object(fields)) = error else {
        return FailureKind::Permanent;
    };
    let status = fields.get("httpStatus").and_then(Value::as_u64);
    if fields.get("code").and_then(Value::as_str) == Some(COST_CAP_CODE)
        || status.is_some_and(|s| HUMAN_HTTP_STATUSES.contains(&s))
    {
        FailureKind::NeedsHuman
    } else if fields.get("retryable").and_then(Value::as_bool) == Some(true) {
        FailureKind::Transient
    } else {
        FailureKind::Permanent
    }
}
