//! Session state, approvals, risk, transcripts and redaction for C-Harness.
//!
//! Pure: frames and actions in, state and strings out. No I/O, no clock, no process spawning, so
//! every rule here is reachable from `cargo test -p ch-core`.

mod approval;
mod redact;
mod risk;
mod session;
mod transcript;

pub use approval::{ApprovalDecision, ApprovalRequest, ReplyError, withdrawn_approval};
pub use redact::{MASK, holds_provider_secret, redact_secrets};
pub use risk::{Assessment, Risk, classify};
pub use session::{
    ClientAction, Diff, Failure, Landing, Notice, Session, SessionState, ToolCall, ToolStatus,
};
pub use transcript::Transcript;
