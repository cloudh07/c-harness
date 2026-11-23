//! What the user sets once: where sessions run, which model key to send, how to verify and
//! where to land. Stored as JSON in the OS config directory. Secrets are not stored here; they
//! live in the keychain through `ch-vault`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ch_protocol::{SUBSCRIPTION_TOKEN_ENV, validate_model};
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "settings.json";
const APP_DIR: &str = "c-harness";

/// Where `ch-runner` runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Location {
    /// A detached git worktree of `Settings::repo` on this computer.
    Local,
    /// A checkout already prepared on another machine, reached with the system `ssh`.
    Ssh {
        host: String,
        #[serde(default)]
        port: Option<u16>,
        remote_workdir: String,
        #[serde(default = "default_remote_runner")]
        remote_runner: String,
    },
}

fn default_remote_runner() -> String {
    ch_runtime::DEFAULT_REMOTE_RUNNER.to_string()
}

/// Which palette the window uses. `System` follows the OS setting as it changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

/// omp's `--approval-mode`. Full auto (yolo) is not offered: a file that names it does not load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalMode {
    /// Every tool call waits for an answer.
    AlwaysAsk,
    /// Reads run; edits and commands wait for an answer.
    #[default]
    Write,
}

impl ApprovalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlwaysAsk => "always-ask",
            Self::Write => "write",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Local repository the worktrees are cut from.
    pub repo: String,
    pub location: Location,
    /// `ch-runner` on this computer, a name on `PATH` or a full path.
    pub runner: String,
    /// omp provider id. The key is stored in the keychain under `model/<provider>`.
    pub provider: String,
    /// Environment variable omp reads the key from.
    pub key_env: String,
    /// `--model` for omp. Empty leaves omp's default.
    pub model: String,
    /// Always sent, so omp's own default (yolo) and a project's config never decide it.
    pub approval_mode: ApprovalMode,
    /// Run by `ch-runner` when you press Verify. Empty hides the button.
    pub verify: String,
    pub author_name: String,
    pub author_email: String,
    /// `owner/name` on GitHub.
    pub github_repo: String,
    pub base_branch: String,
    pub theme: ThemePref,
    /// Aborts the run once the priced turns of a session cost this many cents. `None` is off.
    pub cost_cap_cents: Option<u32>,
    /// Tell the OS when a run ends while the window is in the background.
    pub notify_finished: bool,
    /// Play the OS alert sound with that notice.
    pub finished_sound: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            repo: String::new(),
            location: Location::Local,
            runner: "ch-runner".to_string(),
            provider: "openai".to_string(),
            key_env: "OPENAI_API_KEY".to_string(),
            model: String::new(),
            approval_mode: ApprovalMode::Write,
            verify: String::new(),
            author_name: String::new(),
            author_email: String::new(),
            github_repo: String::new(),
            base_branch: "main".to_string(),
            theme: ThemePref::System,
            cost_cap_cents: None,
            notify_finished: true,
            finished_sound: false,
        }
    }
}

/// Key variables omp 18.13.1 lists in its help text, by provider id.
const KNOWN_KEY_ENV: [(&str, &str); 9] = [
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("cerebras", "CEREBRAS_API_KEY"),
    ("google", "GEMINI_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("mistral", "MISTRAL_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("zai", "ZAI_API_KEY"),
];

/// The usual key variable for a provider id, if omp documents one.
pub fn key_env_for(provider: &str) -> Option<&'static str> {
    KNOWN_KEY_ENV
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, env)| *env)
}

/// The key variable to switch to when the provider changes. Only a field that is empty or still
/// holds another provider's usual variable follows; a name the user typed stays.
pub fn key_env_after_provider_change(provider: &str, current: &str) -> Option<&'static str> {
    let current = current.trim();
    let follows = current.is_empty() || KNOWN_KEY_ENV.iter().any(|(_, env)| *env == current);
    key_env_for(provider.trim()).filter(|env| follows && *env != current)
}

/// One field the user has to fix before a session can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub field: &'static str,
    pub message: String,
}

impl Problem {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl Settings {
    /// Everything that would stop a session from starting or landing. Empty means ready.
    pub fn problems(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        match &self.location {
            Location::Local if self.repo.trim().is_empty() => {
                out.push(Problem::new("repo", "Pick the repository to work in."));
            }
            Location::Local => {}
            Location::Ssh {
                host,
                remote_workdir,
                ..
            } => {
                if host.trim().is_empty() {
                    out.push(Problem::new("host", "Enter an SSH host."));
                }
                if remote_workdir.trim().is_empty() {
                    out.push(Problem::new(
                        "remote_workdir",
                        "Enter the checkout path on the SSH host.",
                    ));
                }
            }
        }
        if self.runner.trim().is_empty() {
            out.push(Problem::new("runner", "Enter the ch-runner program."));
        }
        if !is_provider_id(&self.provider) {
            out.push(Problem::new(
                "provider",
                "Use omp's provider id: lowercase letters, digits and dashes.",
            ));
        }
        if let Err(message) = check_key_env(&self.key_env) {
            out.push(Problem::new("key_env", message));
        }
        if !self.model.trim().is_empty()
            && let Err(message) = validate_model(self.model.trim())
        {
            out.push(Problem::new("model", message));
        }
        out
    }

    /// What landing needs on top of [`Settings::problems`].
    pub fn land_problems(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        if self.author_name.trim().is_empty() {
            out.push(Problem::new("author_name", "Enter the commit author name."));
        }
        if !self.author_email.contains('@') {
            out.push(Problem::new(
                "author_email",
                "Enter the commit author email.",
            ));
        }
        if !is_github_repo(&self.github_repo) {
            out.push(Problem::new("github_repo", "Use the owner/name form."));
        }
        if !is_branch_name(&self.base_branch) {
            out.push(Problem::new(
                "base_branch",
                "Enter the branch to open the pull request against.",
            ));
        }
        out
    }

    /// `--model` and its value when one is set.
    pub fn model_args(&self) -> Vec<String> {
        let model = self.model.trim();
        if model.is_empty() {
            Vec::new()
        } else {
            vec!["--model".to_string(), model.to_string()]
        }
    }

    /// What the app adds to omp's command line: the approval mode, then the model.
    pub fn omp_args(&self) -> Vec<String> {
        let mut args = vec![
            "--approval-mode".to_string(),
            self.approval_mode.as_str().to_string(),
        ];
        args.extend(self.model_args());
        args
    }
}

fn is_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn check_key_env(value: &str) -> Result<(), String> {
    let valid = value
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if !valid {
        return Err("Use an environment variable name such as OPENAI_API_KEY.".to_string());
    }
    if SUBSCRIPTION_TOKEN_ENV.contains(&value) {
        return Err(format!(
            "{value} carries a subscription login, which third-party apps may not use. Use an API key variable."
        ));
    }
    Ok(())
}

fn is_github_repo(value: &str) -> bool {
    let mut parts = value.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    ok(owner) && ok(name)
}

fn is_branch_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.contains("..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
}

/// `<config dir>/c-harness/settings.json`, or `None` on a system without a config dir.
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join(APP_DIR).join(FILE_NAME))
}

/// Where local worktrees go: `<local data dir>/c-harness/worktrees`.
pub fn worktrees_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(APP_DIR).join("worktrees"))
}

/// Where local session records go: `<local data dir>/c-harness/sessions`.
pub fn sessions_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(APP_DIR).join("sessions"))
}

/// Where per-turn usage is kept: `<local data dir>/c-harness/usage`.
pub fn usage_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(APP_DIR).join("usage"))
}

/// Where redacted transcripts are exported: `<local data dir>/c-harness/transcripts`.
pub fn transcripts_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(APP_DIR).join("transcripts"))
}

#[derive(Debug)]
pub enum LoadError {
    Read(io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(err) => write!(f, "could not read settings: {err}"),
            Self::Parse(err) => write!(f, "settings file is not valid: {err}"),
        }
    }
}

/// A missing file is the first run and yields defaults. A file that exists but does not parse
/// is an error, so a typo never silently resets everything.
pub fn load(path: &Path) -> Result<Settings, LoadError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(LoadError::Parse),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(err) => Err(LoadError::Read(err)),
    }
}

/// Write through a sibling temp file and rename, so a crash never leaves half a file.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "settings path has no parent")
    })?;
    fs::create_dir_all(dir)?;
    let text = serde_json::to_string_pretty(settings).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ch_protocol::validate_approval_mode;

    fn ready() -> Settings {
        Settings {
            repo: "/work/demo".to_string(),
            verify: "cargo test".to_string(),
            author_name: "Ada".to_string(),
            author_email: "ada@example.com".to_string(),
            github_repo: "ada/demo".to_string(),
            ..Settings::default()
        }
    }

    fn fields(problems: &[Problem]) -> Vec<&'static str> {
        problems.iter().map(|p| p.field).collect()
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load(&dir.path().join("settings.json")).unwrap();
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let mut settings = ready();
        settings.location = Location::Ssh {
            host: "devbox".to_string(),
            port: Some(2222),
            remote_workdir: "/srv/demo".to_string(),
            remote_runner: "ch-runner".to_string(),
        };
        save(&path, &settings).unwrap();
        assert_eq!(load(&path).unwrap(), settings);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn broken_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(matches!(load(&path), Err(LoadError::Parse(_))));
    }

    #[test]
    fn unknown_and_missing_fields_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"repo":"/x","future_field":1}"#).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.repo, "/x");
        assert_eq!(loaded.base_branch, "main");
    }

    #[test]
    fn ready_settings_have_no_problems() {
        let settings = ready();
        let start = settings.problems();
        assert!(start.is_empty(), "{start:?}");
        let land = settings.land_problems();
        assert!(land.is_empty(), "{land:?}");
    }

    #[test]
    fn local_needs_a_repo_and_ssh_needs_host_and_workdir() {
        let mut settings = ready();
        settings.repo.clear();
        assert_eq!(fields(&settings.problems()), ["repo"]);

        settings.location = Location::Ssh {
            host: " ".to_string(),
            port: None,
            remote_workdir: String::new(),
            remote_runner: "ch-runner".to_string(),
        };
        assert_eq!(fields(&settings.problems()), ["host", "remote_workdir"]);
    }

    #[test]
    fn subscription_token_variables_are_refused() {
        for name in SUBSCRIPTION_TOKEN_ENV {
            let settings = Settings {
                key_env: name.to_string(),
                ..ready()
            };
            assert_eq!(fields(&settings.problems()), ["key_env"]);
        }
    }

    #[test]
    fn key_env_must_look_like_a_variable() {
        for bad in ["", "openai_key", "1KEY", "KEY-NAME", "KEY NAME"] {
            let settings = Settings {
                key_env: bad.to_string(),
                ..ready()
            };
            assert_eq!(fields(&settings.problems()), ["key_env"], "{bad:?}");
        }
    }

    #[test]
    fn model_that_reads_as_a_flag_is_refused() {
        let settings = Settings {
            model: "--yolo".to_string(),
            ..ready()
        };
        assert_eq!(fields(&settings.problems()), ["model"]);
        assert_eq!(
            Settings {
                model: " gpt-5 ".to_string(),
                ..ready()
            }
            .model_args(),
            ["--model", "gpt-5"]
        );
        let args = ready().model_args();
        assert!(args.is_empty(), "{args:?}");
    }

    #[test]
    fn land_fields_are_checked() {
        let settings = Settings {
            author_email: "nobody".to_string(),
            github_repo: "owner/name/extra".to_string(),
            base_branch: "-x".to_string(),
            ..ready()
        };
        assert_eq!(
            fields(&settings.land_problems()),
            ["author_email", "github_repo", "base_branch"]
        );
        for bad in ["", "owner", "/name", "owner/", "../x", "o w/n"] {
            assert!(!is_github_repo(bad), "{bad:?}");
        }
        assert!(is_github_repo("cloudh07/c-harness"));
    }

    #[test]
    fn known_providers_map_to_their_key_variable() {
        assert_eq!(key_env_for("openrouter"), Some("OPENROUTER_API_KEY"));
        assert_eq!(key_env_for("google"), Some("GEMINI_API_KEY"));
        assert_eq!(key_env_for("nope"), None);
    }

    #[test]
    fn key_variable_follows_the_provider_until_the_user_types_one() {
        assert_eq!(
            key_env_after_provider_change("anthropic", ""),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(
            key_env_after_provider_change("anthropic", "OPENAI_API_KEY"),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(key_env_after_provider_change("anthropic", "MY_KEY"), None);
        assert_eq!(
            key_env_after_provider_change("openai", "OPENAI_API_KEY"),
            None
        );
        assert_eq!(
            key_env_after_provider_change("nope", "OPENAI_API_KEY"),
            None
        );
    }
    #[test]
    fn omp_args_always_carry_the_approval_mode() {
        assert_eq!(ready().omp_args(), ["--approval-mode", "write"]);
        let asking = Settings {
            approval_mode: ApprovalMode::AlwaysAsk,
            model: "gpt-5".to_string(),
            ..ready()
        };
        assert_eq!(
            asking.omp_args(),
            ["--approval-mode", "always-ask", "--model", "gpt-5"]
        );
        for mode in [ApprovalMode::AlwaysAsk, ApprovalMode::Write] {
            assert_eq!(validate_approval_mode(mode.as_str()), Ok(()), "{mode:?}");
        }
    }

    #[test]
    fn a_file_without_an_approval_mode_asks_before_writes() {
        let old: Settings = serde_json::from_str(r#"{"repo": "/work/app"}"#).expect("parses");
        assert_eq!(old.approval_mode, ApprovalMode::Write);
        let text = serde_json::to_string(&Settings {
            approval_mode: ApprovalMode::AlwaysAsk,
            ..Settings::default()
        })
        .expect("serializes");
        assert!(text.contains(r#""approval_mode":"always-ask""#), "{text}");
    }

    #[test]
    fn a_hand_edited_yolo_is_refused_not_obeyed() {
        // Full auto is not offered yet: the file fails to load and the app falls back to write.
        // Nor is the cached mode, which an older omp replaces with full auto.
        for mode in ["yolo", "always-ask-cached", "Write", ""] {
            let text = format!(r#"{{"approval_mode": "{mode}"}}"#);
            assert!(serde_json::from_str::<Settings>(&text).is_err(), "{mode:?}");
        }
        assert_eq!(Settings::default().approval_mode, ApprovalMode::Write);
    }

    #[test]
    fn a_file_from_before_the_cost_cap_has_no_cap() {
        let old: Settings = serde_json::from_str(r#"{"repo": "/work/app"}"#).expect("parses");
        assert_eq!(old.cost_cap_cents, None);
    }

    #[test]
    fn a_file_from_before_themes_follows_the_system() {
        let old: Settings = serde_json::from_str(r#"{"repo": "/work/app"}"#).expect("parses");
        assert_eq!(old.theme, ThemePref::System);
        let dark = Settings {
            theme: ThemePref::Dark,
            ..Settings::default()
        };
        let text = serde_json::to_string(&dark).expect("serializes");
        assert!(text.contains(r#""theme":"dark""#), "{text}");
        assert_eq!(
            serde_json::from_str::<Settings>(&text).expect("parses"),
            dark
        );
    }
}
