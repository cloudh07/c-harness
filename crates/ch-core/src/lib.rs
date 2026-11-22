//! Session state, approvals, risk, transcripts and redaction for C-Harness.
//!
//! Pure: frames and actions in, state and strings out. No I/O, no clock, no process spawning, so
//! every rule here is reachable from `cargo test -p ch-core`.

mod approval;
mod checkpoints;
mod context;
mod failure;
mod history;
mod redact;
mod risk;
mod secret_shape;
mod session;
mod transcript;
mod usage;

pub use approval::{ApprovalDecision, ApprovalRequest, ReplyError, withdrawn_approval};
pub use checkpoints::{RESTORE_NOTICE, previous_turn, run_ended};
pub use context::{
    COMPACTION_NOTICE, ContextMeter, ContextReading, ContextState, STATS_COMMAND,
    compaction_notice, is_stats, session_file,
};
pub use failure::{FailureKind, Retrying};
pub use history::{
    HISTORY_COMMAND, HISTORY_PAGE_LIMIT, History, HistoryPage, MAX_HISTORY_TURNS, history_request,
    read_page,
};
pub use redact::{MASK, redact_secrets};
pub use risk::{Assessment, Risk, classify};
pub use secret_shape::{holds_provider_secret, secret_kind};
pub use session::{
    ClientAction, Diff, Failure, Landing, Notice, ReplyWait, Session, SessionState, ToolCall,
    ToolStatus, TurnDiff,
};
pub use transcript::Transcript;
pub use usage::{Usage, UsageRow, UsageStatus, UsageTotals};
