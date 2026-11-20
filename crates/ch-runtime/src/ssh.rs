use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;

use crate::local::validate_session;
use crate::process::{command, runner_info, stderr_tail};
use crate::{RunnerInfo, RunnerProcess, RuntimeError, SandboxProvider};

/// The remote runner command when none is set: `ch-runner` on the remote PATH.
pub const DEFAULT_REMOTE_RUNNER: &str = "ch-runner";

// ssh exits with 255 when ssh itself fails, and passes the remote command's code through
// otherwise. POSIX shells use 127 for "command not found".
const SSH_FAILED: i32 = 255;
const COMMAND_NOT_FOUND: i32 = 127;

// The characters OpenSSH 9.6 refuses in a host name from the command line (CVE-2023-51385),
// because a `Match exec` or `ProxyCommand` in the user's config may pass the host to a shell.
const HOST_METACHARACTERS: &str = "'`\"$\\;&<>|(){}";

/// Runs sessions on another machine through the system `ssh` client.
///
/// Logging in is left to ssh: ssh-agent, `identity_file` and your SSH config. ssh runs with
/// `BatchMode=yes`, so it fails instead of asking for a password or a new host key; connect
/// once with `ssh <host>` in a terminal first. Nothing secret is put on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshProvider {
    /// A host name, `user@host`, or an alias from your SSH config.
    pub host: String,
    pub port: Option<u16>,
    pub identity_file: Option<PathBuf>,
    /// The directory the remote runner works in. A leading `~` is the remote home.
    pub remote_workdir: String,
    /// The remote `ch-runner` command. A leading `~` is the remote home: non-interactive SSH
    /// logins often leave `~/.cargo/bin` off PATH, so `~/.cargo/bin/ch-runner` is a common value.
    pub remote_runner: String,
}

impl SshProvider {
    /// A provider using the default port, the identities ssh picks itself, and
    /// [`DEFAULT_REMOTE_RUNNER`].
    pub fn new(host: impl Into<String>, remote_workdir: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: None,
            identity_file: None,
            remote_workdir: remote_workdir.into(),
            remote_runner: DEFAULT_REMOTE_RUNNER.to_owned(),
        }
    }

    fn serve_command(&self) -> Result<String, RuntimeError> {
        let runner = remote_word("remote ch-runner command", &self.remote_runner)?;
        let workdir = remote_word("remote working directory", &self.remote_workdir)?;
        Ok(format!("{runner} serve --workdir {workdir}"))
    }

    fn version_command(&self) -> Result<String, RuntimeError> {
        let runner = remote_word("remote ch-runner command", &self.remote_runner)?;
        Ok(format!("{runner} --version"))
    }

    /// The arguments for the local `ssh` that runs `remote_command` on the host.
    fn ssh_args(&self, remote_command: String) -> Result<Vec<OsString>, RuntimeError> {
        validate_host(&self.host)?;
        let mut args: Vec<OsString> = [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=4",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        if let Some(port) = self.port {
            if port == 0 {
                return Err(RuntimeError::InvalidSetting {
                    setting: "SSH port",
                    problem: "must be between 1 and 65535",
                });
            }
            args.extend(["-p".into(), port.to_string().into()]);
        }
        if let Some(identity) = &self.identity_file {
            if identity.as_os_str().is_empty() {
                return Err(RuntimeError::InvalidSetting {
                    setting: "SSH identity file",
                    problem: "is empty",
                });
            }
            args.extend(["-i".into(), identity.into()]);
        }
        // ssh joins everything after the host into one string for the remote shell, so the
        // command goes as a single argument that is already quoted for that shell.
        args.extend([
            "--".into(),
            self.host.as_str().into(),
            remote_command.into(),
        ]);
        Ok(args)
    }
}

impl SandboxProvider for SshProvider {
    fn label(&self) -> String {
        match self.port {
            Some(port) => format!("SSH: {}:{port}", self.host),
            None => format!("SSH: {}", self.host),
        }
    }

    fn check(&self) -> Result<RunnerInfo, RuntimeError> {
        let args = self.ssh_args(self.version_command()?)?;
        let output = command("ssh")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(ssh_spawn_error)?;
        match output.status.code() {
            Some(SSH_FAILED) => Err(RuntimeError::SshFailed {
                host: self.host.clone(),
                stderr: stderr_tail(&output.stderr),
            }),
            Some(COMMAND_NOT_FOUND) => Err(RuntimeError::RunnerNotFound {
                location: self.host.clone(),
            }),
            _ => runner_info(&self.host, &output),
        }
    }

    /// Starts the runner in `remote_workdir`. The session id is checked but not sent: the
    /// remote side has one working directory per provider.
    ///
    /// Connection and login run after this returns, so their failures arrive as ssh exiting
    /// with code 255 and its message on stderr. Call [`SandboxProvider::check`] first.
    fn start(&self, session: &str) -> Result<RunnerProcess, RuntimeError> {
        validate_session(session)?;
        let args = self.ssh_args(self.serve_command()?)?;
        let child = command("ssh")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ssh_spawn_error)?;
        Ok(RunnerProcess::new(child))
    }
}

fn ssh_spawn_error(source: io::Error) -> RuntimeError {
    if source.kind() == io::ErrorKind::NotFound {
        RuntimeError::ToolNotFound { tool: "ssh" }
    } else {
        RuntimeError::Spawn {
            program: "ssh".to_owned(),
            source,
        }
    }
}

fn validate_host(host: &str) -> Result<(), RuntimeError> {
    let refused = host.is_empty()
        || host.starts_with('-')
        || host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || HOST_METACHARACTERS.contains(c));
    if refused {
        Err(RuntimeError::InvalidHost(host.to_owned()))
    } else {
        Ok(())
    }
}

/// `word` quoted for the remote POSIX shell, keeping a leading `~` or `~/` as the remote home.
fn remote_word(setting: &'static str, word: &str) -> Result<String, RuntimeError> {
    if word.is_empty() || word.chars().any(char::is_control) {
        return Err(RuntimeError::InvalidSetting {
            setting,
            problem: "is empty or contains control characters",
        });
    }
    // A quoted `~` is not expanded, so the home directory comes from $HOME instead.
    Ok(match word.strip_prefix('~') {
        Some("") => "\"$HOME\"".to_owned(),
        Some("/") => "\"$HOME\"/".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("\"$HOME\"/{}", shell_quote(&rest[1..])),
        _ => shell_quote(word),
    })
}

/// POSIX single quoting: nothing inside single quotes is special, and a `'` ends the quote,
/// is escaped, and reopens it.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: [&str; 7] = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=4",
    ];

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    #[test]
    fn single_quotes_survive_quoting() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(
            shell_quote("$(rm -rf ~) `x` \"y\""),
            "'$(rm -rf ~) `x` \"y\"'"
        );
    }

    #[test]
    fn leading_tilde_becomes_the_remote_home() {
        let word = |w| remote_word("setting", w).unwrap();
        assert_eq!(word("~"), "\"$HOME\"");
        assert_eq!(word("~/"), "\"$HOME\"/");
        assert_eq!(
            word("~/.cargo/bin/ch-runner"),
            "\"$HOME\"/'.cargo/bin/ch-runner'"
        );
        assert_eq!(word("~/it's here"), r#""$HOME"/'it'\''s here'"#);
        assert_eq!(word("~bob/x"), "'~bob/x'");
        assert_eq!(word("/srv/a~b"), "'/srv/a~b'");
    }

    #[test]
    fn empty_or_control_words_are_settings_errors() {
        for bad in ["", "a\nb", "a\0b", "\u{1b}[31m"] {
            assert!(matches!(
                remote_word("remote working directory", bad),
                Err(RuntimeError::InvalidSetting {
                    setting: "remote working directory",
                    ..
                })
            ));
        }
    }

    #[test]
    fn serve_arguments_without_options() {
        let provider = SshProvider::new("dev@box", "~/work");
        let args = provider
            .ssh_args(provider.serve_command().unwrap())
            .unwrap();
        let mut expected: Vec<String> = BASE.iter().map(ToString::to_string).collect();
        expected.extend([
            "--".into(),
            "dev@box".into(),
            "'ch-runner' serve --workdir \"$HOME\"/'work'".into(),
        ]);
        assert_eq!(strings(args), expected);
    }

    #[test]
    fn serve_arguments_with_port_identity_and_runner() {
        let provider = SshProvider {
            port: Some(2222),
            identity_file: Some(PathBuf::from("keys/id_ed25519")),
            remote_runner: "~/.cargo/bin/ch-runner".into(),
            ..SshProvider::new("box", "/srv/repo's")
        };
        let args = provider
            .ssh_args(provider.serve_command().unwrap())
            .unwrap();
        let mut expected: Vec<String> = BASE.iter().map(ToString::to_string).collect();
        expected.extend(
            [
                "-p",
                "2222",
                "-i",
                "keys/id_ed25519",
                "--",
                "box",
                r#""$HOME"/'.cargo/bin/ch-runner' serve --workdir '/srv/repo'\''s'"#,
            ]
            .map(String::from),
        );
        assert_eq!(strings(args), expected);
        assert_eq!(
            provider.version_command().unwrap(),
            r#""$HOME"/'.cargo/bin/ch-runner' --version"#
        );
        assert_eq!(provider.label(), "SSH: box:2222");
    }

    #[test]
    fn bad_port_and_identity_are_refused() {
        let zero_port = SshProvider {
            port: Some(0),
            ..SshProvider::new("box", "/w")
        };
        assert!(matches!(
            zero_port.ssh_args(String::new()),
            Err(RuntimeError::InvalidSetting {
                setting: "SSH port",
                ..
            })
        ));
        let empty_identity = SshProvider {
            identity_file: Some(PathBuf::new()),
            ..SshProvider::new("box", "/w")
        };
        assert!(empty_identity.ssh_args(String::new()).is_err());
    }

    #[test]
    fn hosts_that_could_become_options_or_shell_code_are_refused() {
        for ok in [
            "box",
            "dev@box.example.com",
            "10.0.0.1",
            "::1",
            "my_alias-2",
            "dev@[::1]",
        ] {
            assert!(validate_host(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-oProxyCommand=calc",
            "a b",
            "a\tb",
            "a\nb",
            "box\u{7f}",
            "box;id",
            "$(id)",
            "`id`",
            "a|b",
            "a&b",
            "a>b",
            "'box'",
            "\"box\"",
            "a\\b",
            "{a,b}",
        ] {
            assert!(validate_host(bad).is_err(), "{bad:?}");
        }
        let provider = SshProvider::new("-oProxyCommand=calc", "/w");
        assert!(matches!(
            provider.check(),
            Err(RuntimeError::InvalidHost(_))
        ));
        assert!(matches!(
            provider.start("s1"),
            Err(RuntimeError::InvalidHost(_))
        ));
    }

    #[test]
    fn start_checks_the_session_before_running_ssh() {
        let provider = SshProvider::new("box", "/w");
        assert!(matches!(
            provider.start("../x"),
            Err(RuntimeError::InvalidSession)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn quoted_words_reach_sh_unchanged() {
        let run = |word: &str| {
            let script = format!("printf '%s' {}", remote_word("setting", word).unwrap());
            let output = std::process::Command::new("sh")
                .args(["-c", &script])
                .env("HOME", "/home/a b")
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        for word in [
            "it's", "a  b", "$HOME", "`id`", "\\", "*", "-n", "\"q\"", "é",
        ] {
            assert_eq!(run(word), word);
        }
        assert_eq!(run("~"), "/home/a b");
        assert_eq!(run("~/x 'y'"), "/home/a b/x 'y'");
    }

    // Port 1 on loopback refuses the connection, so a working ssh exits with 255.
    #[test]
    fn unreachable_host_is_an_ssh_failure() {
        let provider = SshProvider {
            port: Some(1),
            ..SshProvider::new("127.0.0.1", "/w")
        };
        match provider.check() {
            Err(RuntimeError::SshFailed { host, .. }) => assert_eq!(host, "127.0.0.1"),
            Err(RuntimeError::ToolNotFound { tool: "ssh" }) => {}
            other => panic!("expected an ssh failure, got {other:?}"),
        }
    }
}
