//! The setup check the first run shows: is the engine installed where sessions run, can the
//! keychain hold a key, does the model answer, and which test command the project points to.

use std::sync::mpsc::Receiver;

use ch_protocol::preflight::{MODEL_TEST_MAX_COST, ModelTest, ModelTestRequest, PreflightReport};
use ch_protocol::{check_approval_mode_version, parse_version};
use ch_runtime::{RUNNER_INSTALL_COMMAND, RuntimeError};
use ch_vault::{SecretName, SecretStore, Vault};

use crate::launch::{self, Target};
use crate::settings::{Location, Settings};

pub const RUNNER: &str = "ch-runner";
pub const OMP: &str = "omp";
pub const GIT: &str = "git";
pub const KEYCHAIN: &str = "Keychain";
pub const MODEL_KEY: &str = "Model key";
pub const MODEL_TEST: &str = "Model answers";

/// The throwaway entry the keychain check writes and deletes.
const KEYCHAIN_PROBE: &str = "c-harness-keychain-check";
const KEYCHAIN_PROBE_VALUE: &str = "c-harness keychain check, safe to delete";
const KEYCHAIN_FIX: &str = "Unlock the system keychain (Credential Manager on Windows, Keychain on \
     macOS, a Secret Service such as GNOME Keyring or KWallet on Linux), then check again.";

/// How one check came out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass(String),
    Fail {
        why: String,
        fix: String,
    },
    /// Not checked yet, or a check it depends on could not run.
    Unknown(String),
}

impl Outcome {
    fn fail(why: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::Fail {
            why: why.into(),
            fix: fix.into(),
        }
    }

    pub fn unknown(why: impl Into<String>) -> Self {
        Self::Unknown(why.into())
    }

    #[cfg(test)]
    pub fn is_fail(&self) -> bool {
        matches!(self, Self::Fail { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub outcome: Outcome,
}

impl Check {
    fn new(name: &'static str, outcome: Outcome) -> Self {
        Self { name, outcome }
    }
}

/// What the runner said about where sessions run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Engine {
    /// The ch-runner, omp and git rows.
    pub checks: Vec<Check>,
    /// Test commands the repository's files point to. Empty until a repository is picked.
    pub verify_commands: Vec<String>,
}

impl Engine {
    /// The rows before the check could run, for `why`.
    pub fn unknown(why: &str) -> Self {
        let outcome = Outcome::unknown(why);
        Self {
            checks: [RUNNER, OMP, GIT]
                .into_iter()
                .map(|name| Check::new(name, outcome.clone()))
                .collect(),
            verify_commands: Vec::new(),
        }
    }
}

/// A local session needs a repository; an SSH session names its checkout on the host instead.
pub fn repo_chosen(settings: &Settings) -> bool {
    !matches!(settings.location, Location::Local) || !settings.repo.trim().is_empty()
}

/// Turns what `ch-runner preflight` returned into rows.
pub fn engine(settings: &Settings, result: &Result<PreflightReport, RuntimeError>) -> Engine {
    let report = match result {
        Ok(report) => report,
        Err(err) => {
            let waiting = Outcome::unknown("Checked once ch-runner answers.");
            return Engine {
                checks: vec![
                    Check::new(RUNNER, runner_failure(err)),
                    Check::new(OMP, waiting.clone()),
                    Check::new(GIT, waiting),
                ],
                verify_commands: Vec::new(),
            };
        }
    };
    let chosen = repo_chosen(settings);
    Engine {
        checks: vec![
            Check::new(
                RUNNER,
                Outcome::Pass(format!("ch-runner {} answered.", report.version)),
            ),
            Check::new(OMP, omp_outcome(settings, report.omp_version.as_deref())),
            Check::new(GIT, git_outcome(chosen, report.git_problem.as_deref())),
        ],
        verify_commands: if chosen {
            report.verify_commands.clone()
        } else {
            Vec::new()
        },
    }
}

fn runner_failure(err: &RuntimeError) -> Outcome {
    let reinstall = format!("Reinstall it with `{RUNNER_INSTALL_COMMAND} --force`.");
    match err {
        RuntimeError::RunnerNotFound { location } => Outcome::fail(
            format!("ch-runner is not installed on {location}."),
            format!(
                "Install it with `{RUNNER_INSTALL_COMMAND}`, or set its full path in Settings."
            ),
        ),
        RuntimeError::RunnerTooOld { location } => Outcome::fail(
            format!("ch-runner on {location} is too old to run the setup check."),
            reinstall,
        ),
        RuntimeError::PreflightFailed { .. } | RuntimeError::RunnerCheckFailed { .. } => {
            Outcome::fail(format!("{err}."), reinstall)
        }
        RuntimeError::PreflightTimedOut { location, seconds } => Outcome::fail(
            format!("The setup check on {location} did not finish within {seconds} seconds."),
            "Something there is stuck or very slow. Check the network and how busy that \
             machine is, then check again.",
        ),
        RuntimeError::SshFailed { host, stderr } => Outcome::fail(
            format!("ssh could not reach {host}: {stderr}"),
            format!("Make `ssh {host}` work in a terminal without any prompt, then check again."),
        ),
        RuntimeError::ToolNotFound { tool } => Outcome::fail(
            format!("`{tool}` is not installed on this computer."),
            "Install it, make sure it is on PATH, then check again.",
        ),
        RuntimeError::InvalidHost(_) | RuntimeError::InvalidSetting { .. } => {
            Outcome::fail(format!("{err}."), "Fix it in Settings, then check again.")
        }
        other => Outcome::fail(
            format!("{other}."),
            "Fix what the message says, then check again.",
        ),
    }
}

fn omp_outcome(settings: &Settings, raw: Option<&str>) -> Outcome {
    let Some(version) = raw.and_then(parse_version) else {
        return Outcome::fail(
            "omp did not answer `omp --version` where sessions run.",
            "Install omp there and make sure it is on PATH, then check again.",
        );
    };
    let mode = settings.approval_mode.as_str();
    match check_approval_mode_version(mode, || Some(version.clone())) {
        Ok(()) => Outcome::Pass(format!("omp {version} answered.")),
        Err(why) => Outcome::fail(
            format!("{why}."),
            "Update omp, or pick another approval mode in Settings.",
        ),
    }
}

fn git_outcome(repo_chosen: bool, problem: Option<&str>) -> Outcome {
    if !repo_chosen {
        return Outcome::unknown("Checked once you pick the repository in step 2.");
    }
    match problem {
        None => Outcome::Pass("git can work in the repository.".into()),
        Some(why) => Outcome::fail(
            why,
            "Point Settings at the top folder of a git checkout, then check again.",
        ),
    }
}

/// Saves, reads back and deletes a throwaway entry. The delete runs whatever happened before
/// it, and an entry left behind counts as a failure.
pub fn keychain_round_trip<S: SecretStore>(vault: &Vault<S>) -> Outcome {
    let Ok(name) = SecretName::model(KEYCHAIN_PROBE) else {
        return Outcome::unknown("The keychain check has no valid entry name.");
    };
    let read = vault
        .set(&name, KEYCHAIN_PROBE_VALUE)
        .and_then(|()| vault.get(&name));
    let deleted = vault.delete(&name);
    match (read, deleted) {
        (Err(e), _) => Outcome::fail(
            format!("The keychain refused a test entry: {e}."),
            KEYCHAIN_FIX,
        ),
        (Ok(_), Err(e)) => Outcome::fail(
            format!("The keychain kept a test entry it was asked to delete: {e}."),
            format!(
                "Delete `{}` under `{}` in the system keychain, then check again.",
                name.as_str(),
                ch_vault::SERVICE
            ),
        ),
        (Ok(Some(secret)), Ok(())) if secret.expose() == KEYCHAIN_PROBE_VALUE => {
            Outcome::Pass("The keychain saved, read back and deleted a test entry.".into())
        }
        (Ok(_), Ok(())) => Outcome::fail(
            "The keychain gave back something other than the test entry it saved.",
            KEYCHAIN_FIX,
        ),
    }
}

/// Whether a key for the configured provider is stored. Only presence is looked at.
pub fn model_key<S: SecretStore>(settings: &Settings, vault: &Vault<S>) -> Outcome {
    let provider = settings.provider.trim();
    let Ok(name) = SecretName::model(provider) else {
        return Outcome::unknown("Fix the provider id in Settings first.");
    };
    match vault.get(&name) {
        Ok(Some(_)) => Outcome::Pass(format!("A key for {provider} is in the keychain.")),
        Ok(None) => Outcome::unknown(format!(
            "No key for {provider} in the keychain, so omp uses its own sign-in or the \
             runner's environment. Paste a key in Settings to use one."
        )),
        Err(e) => Outcome::fail(
            format!("Could not read the model key from the keychain: {e}."),
            KEYCHAIN_FIX,
        ),
    }
}

/// What the model test costs, shown beside its button before anything is sent.
pub fn model_test_cost(settings: &Settings) -> String {
    format!(
        "Sends one short prompt with tools off. {} bills it to the key or sign-in omp uses. \
         omp stops after ${MODEL_TEST_MAX_COST}, but a request already sent can cost more.",
        settings.provider.trim()
    )
}

pub fn model_test_untested() -> Outcome {
    Outcome::unknown("Not tested. The test costs money, so it runs only when you press the button.")
}

pub fn model_test_request<S: SecretStore>(
    settings: &Settings,
    vault: &Vault<S>,
) -> Result<ModelTestRequest, String> {
    let model = settings.model.trim();
    Ok(ModelTestRequest {
        env: launch::model_env(settings, vault)?,
        model: (!model.is_empty()).then(|| model.to_owned()),
    })
}

pub fn model_test_outcome(result: &Result<PreflightReport, RuntimeError>) -> Outcome {
    let again = "Check the provider, model and key in Settings, then test again.";
    match result {
        Err(err) => runner_failure(err),
        Ok(PreflightReport {
            model_test: Some(ModelTest { answered, detail }),
            ..
        }) => {
            if *answered {
                Outcome::Pass("The model answered a one-word prompt.".into())
            } else {
                Outcome::fail(detail.clone(), again)
            }
        }
        Ok(_) => Outcome::fail(
            "The runner sent back no model answer.",
            format!(
                "Reinstall ch-runner with `{RUNNER_INSTALL_COMMAND} --force`, then test again."
            ),
        ),
    }
}

/// What step 3 and Settings offer for the verify command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Suggestion {
    /// The runner has not answered yet.
    Checking,
    /// The repository could not be looked at. The outcome says why and how to fix it.
    Unavailable(Outcome),
    /// A command is already typed. It stays as it is.
    Keep(String),
    /// The project's files point to these. The person picks one.
    Offer(Vec<String>),
    /// The repository was read and nothing matched. The person types the command.
    Ask,
}

/// `engine` is `None` while the check runs.
pub fn verify_suggestion(typed: &str, engine: Option<&Engine>) -> Suggestion {
    let typed = typed.trim();
    if !typed.is_empty() {
        return Suggestion::Keep(typed.to_owned());
    }
    let Some(engine) = engine else {
        return Suggestion::Checking;
    };
    if !engine.verify_commands.is_empty() {
        return Suggestion::Offer(engine.verify_commands.clone());
    }
    // omp has no say in what the repository's files point to.
    let blocked = engine
        .checks
        .iter()
        .find(|c| c.name != OMP && !matches!(c.outcome, Outcome::Pass(_)));
    match blocked {
        Some(check) => Suggestion::Unavailable(check.outcome.clone()),
        None => Suggestion::Ask,
    }
}

/// Runs `ch-runner preflight` where sessions run, off the UI thread.
pub fn check_engine(settings: Settings) -> Receiver<Result<Engine, String>> {
    launch::background(move || {
        Ok(match Target::for_check(&settings) {
            Ok(target) => engine(&settings, &target.provider().preflight(None)),
            Err(problem) => Engine::unknown(&format!("{problem} Then check again.")),
        })
    })
}

/// The keychain round trip and the model key row, off the UI thread: a keychain can block on
/// an unlock prompt.
pub fn check_keychain(settings: Settings) -> Receiver<Result<[Check; 2], String>> {
    launch::background(move || {
        let vault = Vault::os();
        Ok([
            Check::new(KEYCHAIN, keychain_round_trip(&vault)),
            Check::new(MODEL_KEY, model_key(&settings, &vault)),
        ])
    })
}

/// Sends the paid one-word prompt. Call it only from a button that showed
/// [`model_test_cost`].
pub fn test_model(settings: Settings) -> Receiver<Result<Outcome, String>> {
    launch::background(move || {
        let target = Target::for_check(&settings)?;
        let request = model_test_request(&settings, &Vault::os())?;
        Ok(model_test_outcome(
            &target.provider().preflight(Some(&request)),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ch_vault::{MemoryStore, Secret, VaultError};

    fn ready() -> Settings {
        Settings {
            repo: "/work/demo".into(),
            ..Settings::default()
        }
    }

    fn report() -> PreflightReport {
        PreflightReport {
            protocol: 4,
            version: "0.1.0".into(),
            omp_version: Some("omp/18.12.3\n".into()),
            git_problem: None,
            verify_commands: vec!["cargo test".into()],
            model_test: None,
        }
    }

    fn platform(name: &SecretName) -> VaultError {
        VaultError::Platform {
            name: name.clone(),
            detail: "refused".into(),
        }
    }

    struct Refusing;

    impl SecretStore for Refusing {
        fn set(&self, name: &SecretName, _: &str) -> Result<(), VaultError> {
            Err(platform(name))
        }
        fn get(&self, name: &SecretName) -> Result<Option<Secret>, VaultError> {
            Err(platform(name))
        }
        fn delete(&self, _: &SecretName) -> Result<(), VaultError> {
            Ok(())
        }
    }

    struct Sticky(MemoryStore);

    impl SecretStore for Sticky {
        fn set(&self, name: &SecretName, value: &str) -> Result<(), VaultError> {
            self.0.set(name, value)
        }
        fn get(&self, name: &SecretName) -> Result<Option<Secret>, VaultError> {
            self.0.get(name)
        }
        fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
            Err(platform(name))
        }
    }

    struct Garbled;

    impl SecretStore for Garbled {
        fn set(&self, _: &SecretName, _: &str) -> Result<(), VaultError> {
            Ok(())
        }
        fn get(&self, _: &SecretName) -> Result<Option<Secret>, VaultError> {
            Ok(Some(Secret::new("something else")))
        }
        fn delete(&self, _: &SecretName) -> Result<(), VaultError> {
            Ok(())
        }
    }

    fn outcome<'a>(checks: &'a [Check], name: &str) -> &'a Outcome {
        &checks.iter().find(|c| c.name == name).unwrap().outcome
    }

    #[test]
    fn the_keychain_check_cleans_up_after_itself() {
        let vault = Vault::new(MemoryStore::new());
        assert!(matches!(keychain_round_trip(&vault), Outcome::Pass(_)));
        let probe = SecretName::model(KEYCHAIN_PROBE).unwrap();
        assert!(vault.get(&probe).unwrap().is_none(), "the entry is gone");
    }

    #[test]
    fn a_keychain_that_refuses_kept_or_garbled_entries_fails() {
        let refused = keychain_round_trip(&Vault::new(Refusing));
        assert!(matches!(refused, Outcome::Fail { ref fix, .. } if fix == KEYCHAIN_FIX));

        let kept = keychain_round_trip(&Vault::new(Sticky(MemoryStore::new())));
        let Outcome::Fail { why, fix } = kept else {
            panic!("a kept entry must fail");
        };
        assert!(why.contains("kept a test entry"), "{why}");
        assert!(fix.contains("model/c-harness-keychain-check"), "{fix}");

        assert!(keychain_round_trip(&Vault::new(Garbled)).is_fail());
    }

    #[test]
    fn the_model_key_row_looks_only_at_presence() {
        let vault = Vault::new(MemoryStore::new());
        let settings = ready();
        assert!(matches!(model_key(&settings, &vault), Outcome::Unknown(_)));

        vault
            .set(&SecretName::model("openai").unwrap(), "test-key-not-real")
            .unwrap();
        let Outcome::Pass(text) = model_key(&settings, &vault) else {
            panic!("a stored key passes");
        };
        assert!(!text.contains("test-key-not-real"));

        let broken = model_key(&settings, &Vault::new(Refusing));
        assert!(broken.is_fail());
    }

    #[test]
    fn a_healthy_runner_passes_every_row() {
        let found = engine(&ready(), &Ok(report()));
        assert!(
            found
                .checks
                .iter()
                .all(|c| matches!(c.outcome, Outcome::Pass(_)))
        );
        assert_eq!(
            outcome(&found.checks, OMP),
            &Outcome::Pass("omp 18.12.3 answered.".into())
        );
        assert_eq!(found.verify_commands, ["cargo test"]);
    }

    #[test]
    fn without_a_repository_git_and_verify_wait() {
        let mut no_repo = report();
        no_repo.git_problem = Some("an SSH folder that is not a git checkout".into());
        let found = engine(&Settings::default(), &Ok(no_repo));
        assert!(matches!(outcome(&found.checks, GIT), Outcome::Unknown(_)));
        assert_eq!(found.verify_commands, Vec::<String>::new());
        assert!(matches!(outcome(&found.checks, RUNNER), Outcome::Pass(_)));
    }

    #[test]
    fn missing_omp_and_broken_git_fail_with_a_fix() {
        let mut broken = report();
        broken.omp_version = None;
        broken.git_problem = Some("/work/demo is not a folder".into());
        let found = engine(&ready(), &Ok(broken));
        assert!(outcome(&found.checks, OMP).is_fail());
        let Outcome::Fail { why, fix } = outcome(&found.checks, GIT) else {
            panic!("git must fail");
        };
        assert_eq!(why, "/work/demo is not a folder");
        assert_ne!(fix, "");
    }

    #[test]
    fn an_old_runner_says_how_to_reinstall() {
        let old = Err(RuntimeError::RunnerTooOld {
            location: "this computer".into(),
        });
        let found = engine(&ready(), &old);
        let Outcome::Fail { why, fix } = outcome(&found.checks, RUNNER) else {
            panic!("an old runner fails");
        };
        assert!(why.contains("too old"), "{why}");
        assert_eq!(
            fix,
            &format!("Reinstall it with `{RUNNER_INSTALL_COMMAND} --force`.")
        );
        assert!(matches!(outcome(&found.checks, OMP), Outcome::Unknown(_)));
        assert!(matches!(outcome(&found.checks, GIT), Outcome::Unknown(_)));
    }

    #[test]
    fn a_missing_runner_says_how_to_install() {
        let missing = Err(RuntimeError::RunnerNotFound {
            location: "box".into(),
        });
        let found = engine(&ready(), &missing);
        let Outcome::Fail { fix, .. } = outcome(&found.checks, RUNNER) else {
            panic!("a missing runner fails");
        };
        assert!(fix.contains(RUNNER_INSTALL_COMMAND), "{fix}");
    }

    fn failure(err: &RuntimeError) -> (String, String) {
        let Outcome::Fail { why, fix } = runner_failure(err) else {
            panic!("every runner error fails the row");
        };
        (why, fix)
    }

    #[test]
    fn ssh_io_and_timeout_failures_say_what_to_do() {
        let (why, fix) = failure(&RuntimeError::SshFailed {
            host: "box".into(),
            stderr: "Connection refused".into(),
        });
        assert_eq!(why, "ssh could not reach box: Connection refused");
        assert!(fix.contains("`ssh box`"), "{fix}");

        let (why, fix) = failure(&RuntimeError::Io {
            action: "resolve the repository path",
            source: std::io::Error::other("denied"),
        });
        assert!(why.starts_with("could not resolve the repository path: denied"), "{why}");
        assert!(why.ends_with('.'), "{why}");
        assert_eq!(fix, "Fix what the message says, then check again.");

        let (why, fix) = failure(&RuntimeError::PreflightTimedOut {
            location: "box".into(),
            seconds: 180,
        });
        assert_eq!(
            why,
            "The setup check on box did not finish within 180 seconds."
        );
        assert!(fix.contains("check again"), "{fix}");
    }

    #[test]
    fn the_model_test_reports_each_answer() {
        let mut answered = report();
        answered.model_test = Some(ModelTest {
            answered: true,
            detail: "ok".into(),
        });
        assert!(matches!(
            model_test_outcome(&Ok(answered)),
            Outcome::Pass(_)
        ));

        let mut refused = report();
        refused.model_test = Some(ModelTest {
            answered: false,
            detail: "401 invalid key".into(),
        });
        assert!(matches!(
            model_test_outcome(&Ok(refused)),
            Outcome::Fail { ref why, .. } if why == "401 invalid key"
        ));

        assert!(
            model_test_outcome(&Ok(report())).is_fail(),
            "no answer at all"
        );
    }

    #[test]
    fn the_cost_note_names_the_provider_and_the_cap() {
        let note = model_test_cost(&ready());
        assert!(note.contains("openai"), "{note}");
        assert!(note.contains(MODEL_TEST_MAX_COST), "{note}");
    }

    #[test]
    fn the_test_request_carries_the_key_and_model() {
        let vault = Vault::new(MemoryStore::new());
        vault
            .set(&SecretName::model("openai").unwrap(), "test-key-not-real")
            .unwrap();
        let mut settings = ready();
        let request = model_test_request(&settings, &vault).unwrap();
        assert_eq!(request.env["OPENAI_API_KEY"], "test-key-not-real");
        assert_eq!(request.model, None);

        settings.model = " gpt-5 ".into();
        let request = model_test_request(&settings, &vault).unwrap();
        assert_eq!(request.model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn a_typed_verify_command_is_never_replaced() {
        let found = engine(&ready(), &Ok(report()));
        assert_eq!(
            verify_suggestion(" make check ", Some(&found)),
            Suggestion::Keep("make check".into())
        );
        assert_eq!(
            verify_suggestion(" make check ", None),
            Suggestion::Keep("make check".into())
        );
        assert_eq!(
            verify_suggestion("  ", Some(&found)),
            Suggestion::Offer(vec!["cargo test".into()])
        );
    }

    #[test]
    fn the_verify_question_waits_for_a_runner_that_read_the_repository() {
        assert_eq!(verify_suggestion("", None), Suggestion::Checking);

        let mut nothing = report();
        nothing.verify_commands.clear();
        let read = engine(&ready(), &Ok(nothing.clone()));
        assert_eq!(verify_suggestion("", Some(&read)), Suggestion::Ask);

        nothing.omp_version = None;
        let no_omp = engine(&ready(), &Ok(nothing.clone()));
        assert_eq!(verify_suggestion("", Some(&no_omp)), Suggestion::Ask);

        let old = engine(
            &ready(),
            &Err(RuntimeError::RunnerTooOld {
                location: "this computer".into(),
            }),
        );
        let Suggestion::Unavailable(Outcome::Fail { fix, .. }) = verify_suggestion("", Some(&old))
        else {
            panic!("an old runner cannot look at the repository");
        };
        assert!(fix.contains("--force"), "{fix}");

        nothing.git_problem = Some("/work/demo is not a folder".into());
        let broken = engine(&ready(), &Ok(nothing));
        assert!(matches!(
            verify_suggestion("", Some(&broken)),
            Suggestion::Unavailable(Outcome::Fail { .. })
        ));

        let no_repo = engine(&Settings::default(), &Ok(report()));
        assert!(matches!(
            verify_suggestion("", Some(&no_repo)),
            Suggestion::Unavailable(Outcome::Unknown(_))
        ));
        let blocked = Engine::unknown("Fix the SSH host in Settings. Then check again.");
        assert!(matches!(
            verify_suggestion("", Some(&blocked)),
            Suggestion::Unavailable(Outcome::Unknown(_))
        ));
    }
}
