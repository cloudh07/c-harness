use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::RUNNER_INSTALL_COMMAND;

/// Why a provider could not check or start a runner.
#[derive(Debug)]
pub enum RuntimeError {
    /// The session id is not 1 to 64 of `[A-Za-z0-9_-]`, or is a Windows device name.
    InvalidSession,
    /// The SSH host would be read as an option or could reach a shell.
    InvalidHost(String),
    /// A provider setting cannot be used as given.
    InvalidSetting {
        setting: &'static str,
        problem: &'static str,
    },
    /// `ch-runner` is not installed at `location`.
    RunnerNotFound { location: String },
    /// `ch-runner --version` ran at `location` but failed or printed nothing.
    RunnerCheckFailed {
        location: String,
        code: Option<i32>,
        stderr: String,
    },
    /// `ch-runner` at `location` predates `ch-runner preflight`.
    RunnerTooOld { location: String },
    /// `ch-runner preflight` ran at `location` but failed or printed no report.
    PreflightFailed {
        location: String,
        code: Option<i32>,
        stderr: String,
    },
    /// `ch-runner preflight` at `location` was still running after `seconds` and was killed.
    PreflightTimedOut { location: String, seconds: u64 },
    /// A program this machine needs, `git` or `ssh`, is not installed.
    ToolNotFound { tool: &'static str },
    /// `ssh` could not connect or log in.
    SshFailed { host: String, stderr: String },
    /// A git command failed.
    Git {
        action: &'static str,
        stderr: String,
    },
    /// The session's worktree directory already exists.
    WorktreeExists(PathBuf),
    /// Another C-Harness window has this session open.
    SessionInUse,
    /// A leftover session cannot be picked up again, for `reason`.
    NotResumable { reason: &'static str },
    /// A process could not be started for a reason other than "not found".
    Spawn { program: String, source: io::Error },
    /// A filesystem step failed.
    Io {
        action: &'static str,
        source: io::Error,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSession => f.write_str(
                "session id is not allowed: use 1 to 64 ASCII letters, digits, '-' or '_', and \
                 not a Windows device name such as CON or COM1",
            ),
            Self::InvalidHost(host) => write!(
                f,
                "SSH host {host:?} is not allowed: give a host name, user@host or an alias from \
                 your SSH config, without spaces, a leading '-', or any of ' ` \" $ \\ ; & < > | \
                 ( ) {{ }}"
            ),
            Self::InvalidSetting { setting, problem } => {
                write!(
                    f,
                    "the {setting} {problem}; fix it in the provider settings"
                )
            }
            Self::RunnerNotFound { location } => write!(
                f,
                "ch-runner was not found on {location}. Install it there with `{RUNNER_INSTALL_COMMAND}`, \
                 or set its full path in the provider settings (for example ~/.cargo/bin/ch-runner)"
            ),
            Self::RunnerCheckFailed {
                location,
                code,
                stderr,
            } => {
                write!(f, "`ch-runner --version` on {location} ")?;
                match code {
                    Some(code) => write!(f, "exited with code {code}")?,
                    None => f.write_str("was killed by a signal")?,
                }
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                write!(f, ". Reinstall it with `{RUNNER_INSTALL_COMMAND}`")
            }
            Self::RunnerTooOld { location } => write!(
                f,
                "ch-runner on {location} is too old to run the setup check. Reinstall it with \
                 `{RUNNER_INSTALL_COMMAND} --force`"
            ),
            Self::PreflightFailed {
                location,
                code,
                stderr,
            } => {
                write!(f, "the setup check on {location} ")?;
                match code {
                    Some(code) => write!(f, "exited with code {code}")?,
                    None => f.write_str("was killed by a signal")?,
                }
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                Ok(())
            }
            Self::PreflightTimedOut { location, seconds } => write!(
                f,
                "the setup check on {location} did not finish within {seconds} seconds and was \
                 stopped"
            ),
            Self::ToolNotFound { tool } => write!(
                f,
                "`{tool}` was not found on this machine. Install it and make sure it is on PATH, \
                 then try again"
            ),
            Self::SshFailed { host, stderr } => write!(
                f,
                "ssh could not reach {host}: {stderr}. Check that `ssh {host}` works in a \
                 terminal without any prompt: accept the host key once, and load your key into \
                 ssh-agent or set an identity file"
            ),
            Self::Git { action, stderr } => {
                write!(
                    f,
                    "git could not {action}: {stderr}. Fix what git reports and try again"
                )
            }
            Self::WorktreeExists(path) => write!(
                f,
                "a worktree for this session already exists at {}. Remove it first or use a new \
                 session id",
                path.display()
            ),
            Self::SessionInUse => f.write_str(
                "this session is open in another C-Harness window. Close it there, or wait for \
                 that window to finish, then try again",
            ),
            Self::NotResumable { reason } => write!(
                f,
                "this session cannot be resumed: {reason}. Discard it, or start a new session"
            ),
            Self::Spawn { program, source } => write!(
                f,
                "could not start {program}: {source}. Check that the path is right and the file \
                 is executable"
            ),
            Self::Io { action, source } => write!(
                f,
                "could not {action}: {source}. Check that the path in the provider settings \
                 exists and that you can write to it"
            ),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_message_carries_the_install_command() {
        let message = RuntimeError::RunnerNotFound {
            location: "box".into(),
        }
        .to_string();
        assert!(message.contains("on box"));
        assert!(message.contains(RUNNER_INSTALL_COMMAND));
    }

    #[test]
    fn check_failure_says_how_it_ended() {
        let killed = RuntimeError::RunnerCheckFailed {
            location: "box".into(),
            code: None,
            stderr: String::new(),
        };
        assert!(killed.to_string().contains("killed by a signal"));
        let exited = RuntimeError::RunnerCheckFailed {
            location: "box".into(),
            code: Some(3),
            stderr: "bad".into(),
        };
        assert!(exited.to_string().contains("exited with code 3: bad"));
    }

    #[test]
    fn preflight_errors_say_what_to_do() {
        let old = RuntimeError::RunnerTooOld {
            location: "box".into(),
        }
        .to_string();
        assert!(old.contains("on box is too old"));
        assert!(old.contains(&format!("{RUNNER_INSTALL_COMMAND} --force")));
        let failed = RuntimeError::PreflightFailed {
            location: "box".into(),
            code: Some(1),
            stderr: "boom".into(),
        };
        assert_eq!(
            failed.to_string(),
            "the setup check on box exited with code 1: boom"
        );
        let stuck = RuntimeError::PreflightTimedOut {
            location: "box".into(),
            seconds: 120,
        };
        assert_eq!(
            stuck.to_string(),
            "the setup check on box did not finish within 120 seconds and was stopped"
        );
    }
}
