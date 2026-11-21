//! Turning saved settings into a running `ch-runner`, off the UI thread.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use ch_runtime::{Leftover, LocalProvider, RunnerProcess, SandboxProvider, SshProvider};
use ch_vault::{SecretName, SecretStore, Vault};

use crate::link::Link;
use crate::settings::{self, Location, Settings};

/// How long a runner gets to shut omp down after its stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(5);
const EXIT_POLL: Duration = Duration::from_millis(50);

pub enum Target {
    Local(LocalProvider),
    Ssh(SshProvider),
}

impl Target {
    pub fn from_settings(settings: &Settings) -> Result<Self, String> {
        if let Some(problem) = settings.problems().into_iter().next() {
            return Err(problem.message);
        }
        match &settings.location {
            Location::Local => {
                let worktrees_dir = settings::worktrees_dir()
                    .ok_or("This system has no local data folder for worktrees.")?;
                let sessions_dir = settings::sessions_dir()
                    .ok_or("This system has no local data folder for session records.")?;
                Ok(Self::Local(LocalProvider {
                    repo: PathBuf::from(settings.repo.trim()),
                    runner: runner_path(settings.runner.trim(), app_dir().as_deref()),
                    worktrees_dir,
                    sessions_dir,
                }))
            }
            Location::Ssh {
                host,
                port,
                remote_workdir,
                remote_runner,
            } => {
                let mut provider = SshProvider::new(host.trim(), remote_workdir.trim());
                provider.port = *port;
                provider.remote_runner = remote_runner.trim().to_string();
                Ok(Self::Ssh(provider))
            }
        }
    }

    fn provider(&self) -> &dyn SandboxProvider {
        match self {
            Self::Local(provider) => provider,
            Self::Ssh(provider) => provider,
        }
    }
}

fn app_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// A bare program name means the copy unpacked beside the app when there is one, so a release
/// runs without editing `PATH`. Windows searches the app's folder anyway; macOS and Linux do not.
fn runner_path(configured: &str, app_dir: Option<&Path>) -> PathBuf {
    let bare = !configured.contains(['/', '\\']);
    if let (true, Some(dir)) = (bare, app_dir) {
        let mut beside = dir.join(configured);
        if beside.extension().is_none() {
            beside.set_extension(std::env::consts::EXE_EXTENSION);
        }
        if beside.is_file() {
            return beside;
        }
    }
    PathBuf::from(configured)
}

/// The model key omp should see, read from the keychain. No stored key means an empty map, so
/// omp falls back to its own login or the runner's environment.
pub fn model_env<S: SecretStore>(
    settings: &Settings,
    vault: &Vault<S>,
) -> Result<BTreeMap<String, String>, String> {
    let name = SecretName::model(&settings.provider).map_err(|e| e.to_string())?;
    let secret = vault
        .get(&name)
        .map_err(|e| format!("Could not read the model key from the keychain: {e}"))?;
    Ok(secret
        .map(|key| BTreeMap::from([(settings.key_env.clone(), key.expose().to_string())]))
        .unwrap_or_default())
}

pub struct Launched {
    pub link: Link,
    pub runner: Runner,
    pub runner_version: String,
    pub env: BTreeMap<String, String>,
}

/// The runner process, plus the worktree it was given when it runs on this computer.
pub struct Runner {
    process: RunnerProcess,
    local: Option<(LocalProvider, String)>,
}

impl Runner {
    pub fn worktree(&self) -> Option<PathBuf> {
        let (provider, session) = self.local.as_ref()?;
        provider.worktree_path(session).ok()
    }

    /// Lets the runner exit on its own, kills it after [`EXIT_GRACE`], and removes the local
    /// worktree when `remove_worktree` is set. Runs on its own thread.
    pub fn retire(self, remove_worktree: bool) {
        thread::spawn(move || {
            let Self { mut process, local } = self;
            let deadline = Instant::now() + EXIT_GRACE;
            while Instant::now() < deadline {
                match process.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => thread::sleep(EXIT_POLL),
                }
            }
            // Held until the worktree is gone, so no other window starts or discards it meanwhile.
            let lock = process.take_lock();
            drop(process);
            if remove_worktree && let Some((provider, session)) = local {
                let _ = provider.remove_worktree(&session);
            }
            drop(lock);
        });
    }
}

/// Runs `work` on its own thread; the receiver gets its result.
fn background<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Receiver<Result<T, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx
}

/// Checks the runner, reads the key and starts `session` on a background thread.
pub fn spawn(settings: Settings, session: String) -> Receiver<Result<Launched, String>> {
    background(move || launch(&settings, &session))
}

/// A provider for the local records and worktrees only. It looks in the configured repository
/// even when sessions now run over SSH, since earlier local ones may still be there.
fn sweeper(settings: &Settings) -> Result<LocalProvider, String> {
    let (Some(worktrees_dir), Some(sessions_dir)) =
        (settings::worktrees_dir(), settings::sessions_dir())
    else {
        return Err("This system has no local data folder for session records.".to_string());
    };
    Ok(LocalProvider {
        repo: PathBuf::from(settings.repo.trim()),
        runner: PathBuf::new(),
        worktrees_dir,
        sessions_dir,
    })
}

/// Finds the local sessions earlier runs left behind, on a background thread.
pub fn leftovers(settings: &Settings) -> Receiver<Result<Vec<Leftover>, String>> {
    let provider = sweeper(settings);
    background(move || provider?.leftovers().map_err(|e| e.to_string()))
}

/// Deletes what `session` left behind, on a background thread; `seen_in` is the repository the
/// scan found its worktree in. The result is the folder that was kept because git no longer
/// listed it, if any.
pub fn discard(
    settings: &Settings,
    session: String,
    seen_in: Option<PathBuf>,
) -> Receiver<Result<Option<PathBuf>, String>> {
    let provider = sweeper(settings);
    background(move || {
        provider?
            .discard(&session, seen_in.as_deref())
            .map_err(|e| e.to_string())
    })
}

fn launch(settings: &Settings, session: &str) -> Result<Launched, String> {
    let target = Target::from_settings(settings)?;
    let provider = target.provider();
    let info = provider.check().map_err(|e| e.to_string())?;
    let env = model_env(settings, &Vault::os())?;
    let mut process = provider.start(session).map_err(|e| e.to_string())?;
    let (Some(stdin), Some(stdout)) = (process.take_stdin(), process.take_stdout()) else {
        return Err("The runner started without its stdin or stdout.".to_string());
    };
    let link = Link::new(stdin, stdout, process.take_stderr());
    let local = match target {
        Target::Local(provider) => Some((provider, session.to_string())),
        Target::Ssh(_) => None,
    };
    Ok(Launched {
        link,
        runner: Runner { process, local },
        runner_version: info.version,
        env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ch_vault::MemoryStore;

    fn ready() -> Settings {
        Settings {
            repo: "/work/demo".into(),
            ..Settings::default()
        }
    }

    #[test]
    fn local_settings_make_a_local_target() {
        let Ok(Target::Local(provider)) = Target::from_settings(&ready()) else {
            panic!("expected a local target");
        };
        assert_eq!(provider.repo, PathBuf::from("/work/demo"));
        assert_eq!(provider.runner, PathBuf::from("ch-runner"));
    }

    #[test]
    fn a_bare_runner_name_prefers_the_copy_beside_the_app() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            runner_path("ch-runner", Some(dir.path())),
            PathBuf::from("ch-runner")
        );

        let shipped = dir
            .path()
            .join("ch-runner")
            .with_extension(std::env::consts::EXE_EXTENSION);
        std::fs::write(&shipped, b"").unwrap();
        assert_eq!(runner_path("ch-runner", Some(dir.path())), shipped);
        assert_eq!(runner_path("ch-runner", None), PathBuf::from("ch-runner"));

        let explicit = "/opt/tools/ch-runner";
        assert_eq!(
            runner_path(explicit, Some(dir.path())),
            PathBuf::from(explicit)
        );
    }

    #[test]
    fn ssh_settings_carry_port_and_runner() {
        let settings = Settings {
            location: Location::Ssh {
                host: "box".into(),
                port: Some(2222),
                remote_workdir: "/srv/demo".into(),
                remote_runner: "~/.cargo/bin/ch-runner".into(),
            },
            ..Settings::default()
        };
        let Ok(Target::Ssh(provider)) = Target::from_settings(&settings) else {
            panic!("expected an ssh target");
        };
        assert_eq!(provider.host, "box");
        assert_eq!(provider.port, Some(2222));
        assert_eq!(provider.remote_workdir, "/srv/demo");
        assert_eq!(provider.remote_runner, "~/.cargo/bin/ch-runner");
    }

    #[test]
    fn unfinished_settings_name_the_first_problem() {
        let Err(message) = Target::from_settings(&Settings::default()) else {
            panic!("an empty repo must not start");
        };
        assert!(message.contains("repository"), "{message}");
    }

    #[test]
    fn stored_key_goes_under_the_configured_variable() {
        let vault = Vault::new(MemoryStore::new());
        let settings = ready();
        assert!(model_env(&settings, &vault).unwrap().is_empty());

        vault
            .set(&SecretName::model("openai").unwrap(), "test-key-not-real")
            .unwrap();
        let env = model_env(&settings, &vault).unwrap();
        assert_eq!(env.len(), 1);
        assert_eq!(env["OPENAI_API_KEY"], "test-key-not-real");
    }
}
