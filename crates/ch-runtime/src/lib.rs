//! Where a session runs: this machine, or a host reached over SSH.
//!
//! A provider starts `ch-runner serve` and hands back its stdio. The frames on that stdio are
//! described in [`ch_protocol::runner`]; this crate only moves the bytes.

mod error;
mod local;
mod process;
mod ssh;

pub use error::RuntimeError;
pub use local::{LocalProvider, validate_session};
pub use process::RunnerProcess;
pub use ssh::{DEFAULT_REMOTE_RUNNER, SshProvider};

/// How to install `ch-runner` where a session runs.
pub const RUNNER_INSTALL_COMMAND: &str =
    "cargo install --git https://github.com/cloudh07/c-harness ch-runner --locked";

/// What `ch-runner --version` reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerInfo {
    /// The version text, without a leading `ch-runner `.
    pub version: String,
}

/// A place where `ch-runner` can run a session.
///
/// Every method blocks on a child process, so call them off the UI thread.
pub trait SandboxProvider: Send + Sync {
    /// A short name for the UI.
    fn label(&self) -> String;

    /// Runs `ch-runner --version` where sessions will run.
    fn check(&self) -> Result<RunnerInfo, RuntimeError>;

    /// Starts `ch-runner serve --workdir <dir>` for `session` with piped stdin, stdout and
    /// stderr. `session` must pass [`validate_session`].
    fn start(&self, session: &str) -> Result<RunnerProcess, RuntimeError>;
}
