//! Pre-flight parsing and post-mortem classification for the `omp` child process.
//!
//! The notes marked "measured" come from running real `omp` builds, and they are recorded
//! where the CLI behaves differently from what its help text suggests. The CLI has changed
//! between releases, so each note names the version when it is known.

/// Provider id used to interrogate `omp token`.
///
/// It is deliberately impossible, which forces the "no such credential" branch. That branch
/// prints the configured-provider list and *never* prints a credential; probing with a real
/// provider id would pull a live API key or OAuth token into the host process for no reason.
pub const PROBE_SENTINEL_PROVIDER: &str = "__ch_probe__";

/// Marker preceding the comma-separated provider list in `omp token` output.
pub const PROVIDER_LIST_PREFIX: &str = "Configured providers:";

/// Values `--approval-mode` accepts.
const APPROVAL_MODES: [&str; 3] = ["always-ask", "write", "yolo"];

/// Values `--thinking` accepts.
const THINKING_LEVELS: [&str; 8] = [
    "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto",
];

/// Extract the version from `omp --version` output.
///
/// Measured: `omp --version` prints `omp/17.2.10`, while `omp --mode rpc --version` prints a
/// bare `17.2.10`. Both shapes must parse, or the probe rejects a perfectly good binary.
pub fn parse_version(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.strip_prefix("omp/").unwrap_or(line);
    let line = line.strip_prefix('v').unwrap_or(line).trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Extract provider ids from a `Configured providers: a, b` line.
///
/// Only names are returned. When the marker is absent the result is empty. The function
/// never falls back to echoing input, because for a *real* provider id the surrounding output
/// is a live credential.
pub fn parse_configured_providers(text: &str) -> Vec<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix(PROVIDER_LIST_PREFIX))
        .map(|rest| {
            rest.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Why an `omp` child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// Exit 0, the documented shutdown after stdin closes.
    Graceful,
    /// No usable model credential. Fix by signing in or setting a provider API key.
    MissingCredential,
    /// The requested model does not resolve. Fix by choosing another model.
    UnknownModel,
    /// A flag the CLI does not know. That is a host bug, not a user problem.
    UnknownFlag,
    /// An unusable `--mode` value. Older omp builds fell into the TUI, wrote ANSI to stdout
    /// and died of SIGHUP (129). Newer builds exit 2 with `Error: Invalid --mode value: ...`
    /// on stderr and nothing on stdout. The stderr line decides when present, the exit code
    /// otherwise.
    InvalidMode,
    /// No exit status at all, i.e. terminated by a signal.
    TerminatedBySignal,
    /// Nonzero exit with no recognised diagnostic.
    Unknown,
}

/// Exit status older omp builds produced when `--mode` was rejected: SIGHUP as `128 + 1`.
const EXIT_SIGHUP: i32 = 129;

/// Classify a dead child from its exit status and the tail of its stderr.
///
/// stderr is consulted before the exit code because exit code 1 covers *both* a missing
/// credential and an unresolvable model; the code alone cannot tell a user-fixable auth
/// problem from a wrong `--model` argument.
pub fn classify_exit(exit_code: Option<i32>, stderr_tail: &[String]) -> ExitReason {
    if exit_code == Some(0) {
        return ExitReason::Graceful;
    }

    for line in stderr_tail {
        let line = line.trim();
        // The no-credential sentence changed between omp releases while the exit shape held
        // (exit 1, empty stdout). Only the two measured wordings classify. Anything else
        // stays Unknown, so the next rewording shows up instead of being guessed at.
        if line.starts_with("No models available") || line.starts_with("No default model selected")
        {
            return ExitReason::MissingCredential;
        }
        // Measured verbatim: `Model "zzz/does-not-exist-9000" not found`
        if line.starts_with("Model \"") && line.ends_with("not found") {
            return ExitReason::UnknownModel;
        }
        if line.contains("unknown flag") {
            return ExitReason::UnknownFlag;
        }
        // Measured verbatim on a newer build, with exit code 2:
        // `Error: Invalid --mode value: "bogus". Expected one of: text, json, rpc, rpc-ui, acp.`
        if line.starts_with("Error: Invalid --mode value") {
            return ExitReason::InvalidMode;
        }
    }

    match exit_code {
        None => ExitReason::TerminatedBySignal,
        Some(EXIT_SIGHUP) => ExitReason::InvalidMode,
        Some(_) => ExitReason::Unknown,
    }
}

/// Reject an `--approval-mode` value omp would silently swallow.
///
/// Measured: omp accepts any string here and exits 0, falling back to the configured default.
/// A UI typo would therefore run the agent under the *wrong* approval policy with no
/// error anywhere, so the host has to be the component that validates.
pub fn validate_approval_mode(value: &str) -> Result<(), String> {
    validate_choice("approvalMode", value, &APPROVAL_MODES)
}

/// Reject a `--thinking` value omp would silently swallow. Same failure mode as
/// [`validate_approval_mode`].
pub fn validate_thinking_level(value: &str) -> Result<(), String> {
    validate_choice("thinking", value, &THINKING_LEVELS)
}

/// Reject a `--model` value that is empty or would be re-read as a flag.
///
/// Model ids are fuzzy-matched by omp, so the host cannot enumerate them; it can only refuse
/// the shapes that would change the meaning of the command line.
pub fn validate_model(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("model must not be empty".to_string());
    }
    if value.starts_with('-') {
        return Err(format!("model {value:?} must not start with '-'"));
    }
    Ok(())
}

fn validate_choice(label: &str, value: &str, allowed: &[&str]) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "invalid {label} {value:?}; expected one of {}",
            allowed.join(", ")
        ))
    }
}

/// The `config.yml` overlay every host-spawned session gets.
///
/// `startup.checkUpdate` defaults to true, which turns every single spawn into a GitHub
/// round-trip before the session is usable. Measured: `--config` is repeatable and last-wins,
/// so whoever spawns omp must pass this overlay last for its keys to win.
///
/// `disabled_providers` is written as omp's `disabledProviders` setting. Because the overlay
/// is last-wins, the caller must pass the user's own list merged with the host's additions
/// (see [`host_disabled_providers`]), or the user's choices would be re-enabled.
///
/// `memory.backend` is off so omp keeps no memory of its own across sessions; the value is
/// quoted because a bare `off` is a YAML boolean.
pub fn host_config_overlay(disabled_providers: &[String]) -> String {
    let mut text = String::from("startup:\n  checkUpdate: false\nmemory:\n  backend: \"off\"\n");
    if !disabled_providers.is_empty() {
        text.push_str("disabledProviders:\n");
        for id in disabled_providers {
            // A JSON string is a valid YAML scalar: no id can break out of the list.
            text.push_str("  - ");
            text.push_str(&serde_json::Value::from(id.as_str()).to_string());
            text.push('\n');
        }
    }
    text
}

/// omp's provider id for Anthropic (Claude).
pub const ANTHROPIC_PROVIDER: &str = "anthropic";

/// omp's provider id for Google Antigravity, which only signs in with Antigravity OAuth.
/// Google Antigravity Additional Terms section 6 (<https://antigravity.google/terms>, read
/// 2026-10-13): using third-party software to access the Service, "e.g. using `OpenClaw` with
/// Antigravity OAuth", is a breach that may suspend the user's Antigravity and Gemini CLI
/// accounts. C-Harness is such software, so it never runs this provider. Gemini through an
/// AI Studio API key (provider `google`) is unaffected.
pub const GOOGLE_ANTIGRAVITY_PROVIDER: &str = "google-antigravity";

/// The sentence omp prints when a provider has no stored OAuth account (measured, 18.10.1).
/// 18.13.1 prints it on stderr and exits 1, so callers read both streams whatever the status.
pub const NO_OAUTH_ACCOUNTS: &str = "No OAuth accounts found";

/// Count the OAuth accounts in `omp token <provider> --list` output. The command prints one
/// `N. identity` line per stored OAuth account and never prints a token; with none stored it
/// prints [`NO_OAUTH_ACCOUNTS`]. Anything else (an older omp without `token`, a locked store,
/// an error) is `None`: an unrecognised answer must never read as "no OAuth".
pub fn parse_oauth_account_count(text: &str) -> Option<usize> {
    let listed = text
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            let digits = line.chars().take_while(char::is_ascii_digit).count();
            digits > 0 && line[digits..].starts_with(". ")
        })
        .count();
    if listed > 0 {
        Some(listed)
    } else if text.contains(NO_OAUTH_ACCOUNTS) {
        Some(0)
    } else {
        None
    }
}

/// Environment variables omp's Anthropic auth reads BEFORE `ANTHROPIC_API_KEY` (pi-ai
/// `registry/hooks/env.ts`: `ANTHROPIC_OAUTH_TOKEN`) or that carry a Claude subscription
/// token by convention (`CLAUDE_CODE_OAUTH_TOKEN`, the `claude setup-token` output). An omp
/// child never inherits them, so a subscription token exported in the user's shell cannot
/// slip past the stored-login check.
pub const SUBSCRIPTION_TOKEN_ENV: [&str; 2] = ["ANTHROPIC_OAUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"];

/// Parse `omp config get disabledProviders --json`. Older builds print the bare array; 18.13.1
/// prints `{"key":"disabledProviders","value":[...],"type":"array",...}` (measured).
pub fn parse_disabled_providers(text: &str) -> Option<Vec<String>> {
    let answer: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let ids = match answer {
        serde_json::Value::Object(mut fields) => fields.remove("value")?,
        bare => bare,
    };
    serde_json::from_value(ids).ok()
}

/// The `disabledProviders` list every host-spawned omp session must run with.
///
/// Anthropic forbids third-party apps from routing requests through Claude Free/Pro/Max
/// credentials (<https://code.claude.com/docs/en/legal-and-compliance>), and omp resolves a
/// stored OAuth login BEFORE any API key (pi-ai `auth/cascade.ts`). So while an Anthropic
/// OAuth account is stored, the only way to keep the session on API keys is to disable the
/// provider for the session. Fail closed: an unknown count (`None`) blocks too.
/// [`GOOGLE_ANTIGRAVITY_PROVIDER`] is always disabled (OAuth-only, see its doc). Other
/// providers are left exactly as the user configured them.
pub fn host_disabled_providers(
    user: &[String],
    anthropic_oauth_accounts: Option<usize>,
) -> Vec<String> {
    let mut out: Vec<String> = user.to_vec();
    let block = anthropic_oauth_accounts.is_none_or(|n| n > 0);
    if block && !out.iter().any(|id| id == ANTHROPIC_PROVIDER) {
        out.push(ANTHROPIC_PROVIDER.to_string());
    }
    if !out.iter().any(|id| id == GOOGLE_ANTIGRAVITY_PROVIDER) {
        out.push(GOOGLE_ANTIGRAVITY_PROVIDER.to_string());
    }
    out
}

/// Whether a `--model` selector names the Antigravity provider (early, friendly refusal only;
/// `disabledProviders` is the enforcement).
pub fn selects_antigravity_model(model: &str) -> bool {
    model
        .trim()
        .to_ascii_lowercase()
        .starts_with("google-antigravity/")
}

/// Whether a `--model` selector names a Claude model. omp also fuzzy-matches ("opus"), so this
/// only drives the early, friendly refusal; `disabledProviders` is the actual enforcement.
pub fn selects_anthropic_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("anthropic/") || m.contains("claude")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parses_both_measured_cli_shapes() {
        // `omp --version`
        assert_eq!(parse_version("omp/17.2.10\n").as_deref(), Some("17.2.10"));
        // `omp --mode rpc --version`
        assert_eq!(parse_version("17.2.10\n").as_deref(), Some("17.2.10"));
        assert_eq!(parse_version("  v17.2.10  \n").as_deref(), Some("17.2.10"));
        assert_eq!(parse_version("\n\n   \n"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn providers_parse_from_the_measured_credential_error() {
        let observed = concat!(
            "No active credential found for provider \"__ch_probe__\".\n",
            "Configured providers: anthropic, zai\n"
        );
        assert_eq!(
            parse_configured_providers(observed),
            vec!["anthropic".to_string(), "zai".to_string()]
        );
    }

    #[test]
    fn providers_are_empty_when_the_marker_is_absent() {
        // The credential itself must never be mistaken for a provider list.
        assert_eq!(
            parse_configured_providers("sk-ant-oat01-SECRET-VALUE\n"),
            Vec::<String>::new()
        );
        assert_eq!(parse_configured_providers(""), Vec::<String>::new());
        assert_eq!(
            parse_configured_providers("Configured providers: \n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn providers_tolerate_a_single_entry_and_ragged_spacing() {
        assert_eq!(
            parse_configured_providers("  Configured providers:   openai  \n"),
            vec!["openai".to_string()]
        );
        assert_eq!(
            parse_configured_providers("Configured providers: a,,b ,c\n"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    fn tail(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn exit_zero_is_graceful_even_with_stderr_noise() {
        assert_eq!(
            classify_exit(Some(0), &tail(&["some warning"])),
            ExitReason::Graceful
        );
    }

    #[test]
    fn exit_one_is_split_by_stderr_because_the_code_is_ambiguous() {
        let credential = tail(&[
            "No models available. Use /login or set an API key environment variable. Then use /model to select a model.",
            "",
            "Set an API key environment variable:",
        ]);
        let model = tail(&[
            "Model \"zzz/does-not-exist-9000\" not found",
            "",
            "Set an API key environment variable:",
        ]);
        assert_eq!(
            classify_exit(Some(1), &credential),
            ExitReason::MissingCredential
        );
        assert_eq!(classify_exit(Some(1), &model), ExitReason::UnknownModel);
        // Same exit code, different cause. That is why the stderr tail is mandatory.
        assert_eq!(classify_exit(Some(1), &[]), ExitReason::Unknown);
    }

    #[test]
    fn the_no_credential_sentence_drift_stays_pinned_to_both_known_forms() {
        let old_sentence = tail(&[
            "No models available. Use /login or set an API key environment variable. Then use /model to select a model.",
        ]);
        let new_sentence = tail(&[
            "No default model selected. Use /login, set an API key environment variable, or select a local model with /model or --model.",
        ]);
        assert_eq!(
            classify_exit(Some(1), &old_sentence),
            ExitReason::MissingCredential
        );
        assert_eq!(
            classify_exit(Some(1), &new_sentence),
            ExitReason::MissingCredential
        );
        let drifted = tail(&["Something completely different went wrong"]);
        assert_eq!(classify_exit(Some(1), &drifted), ExitReason::Unknown);
    }

    #[test]
    fn unknown_flag_and_invalid_mode_are_distinguished() {
        assert_eq!(
            classify_exit(Some(2), &tail(&["Error: unknown flag: --nope"])),
            ExitReason::UnknownFlag
        );
        // Older builds died of SIGHUP with ANSI-only stdout, so the code was the only signal.
        assert_eq!(classify_exit(Some(129), &[]), ExitReason::InvalidMode);
        // Newer builds exit 2 and say so on stderr.
        assert_eq!(
            classify_exit(
                Some(2),
                &tail(&[
                    "Error: Invalid --mode value: \"bogus\". Expected one of: text, json, rpc, rpc-ui, acp."
                ])
            ),
            ExitReason::InvalidMode
        );
        // The stderr line wins on the old exit code too, so both binaries classify the same.
        assert_eq!(
            classify_exit(
                Some(129),
                &tail(&[
                    "Error: Invalid --mode value: \"bogus\". Expected one of: text, json, rpc, rpc-ui, acp."
                ])
            ),
            ExitReason::InvalidMode
        );
    }

    #[test]
    fn no_status_means_a_signal_ended_the_child() {
        assert_eq!(classify_exit(None, &[]), ExitReason::TerminatedBySignal);
    }

    #[test]
    fn spawn_arguments_reject_values_omp_would_swallow() {
        assert!(validate_approval_mode("yolo").is_ok());
        assert!(validate_approval_mode("write").is_ok());
        assert!(validate_approval_mode("always-ask").is_ok());
        assert!(validate_approval_mode("Yolo").is_err());
        assert!(validate_approval_mode("").is_err());

        assert!(validate_thinking_level("xhigh").is_ok());
        assert!(validate_thinking_level("very").is_err());

        assert!(validate_model("anthropic/claude-opus-4").is_ok());
        assert!(validate_model("--api-key").is_err());
        assert!(validate_model("   ").is_err());
    }

    #[test]
    fn host_overlay_disables_the_startup_update_check() {
        assert!(host_config_overlay(&[]).contains("checkUpdate: false"));
        assert!(!host_config_overlay(&[]).contains("disabledProviders"));
    }

    #[test]
    fn host_overlay_turns_omp_memory_off_with_a_quoted_value() {
        // Unquoted `off` is a YAML 1.1 boolean, not the string omp expects.
        assert_eq!(
            host_config_overlay(&[]),
            "startup:\n  checkUpdate: false\nmemory:\n  backend: \"off\"\n"
        );
        let text = host_config_overlay(&["openai".to_string()]);
        assert!(text.contains("memory:\n  backend: \"off\"\n"), "{text}");
    }

    #[test]
    fn oauth_accounts_count_from_the_measured_list_shapes() {
        assert_eq!(
            parse_oauth_account_count("1. someone@example.com (Org)\n"),
            Some(1)
        );
        assert_eq!(
            parse_oauth_account_count("1. a@x (A)\n2. b@x (B)\n"),
            Some(2)
        );
        let none = "No OAuth accounts found for provider \"anthropic\".\n--account/--list select among OAuth accounts; this provider has none stored.\n";
        assert_eq!(parse_oauth_account_count(none), Some(0));
        // Fail closed: empty output (non-zero exit), an unknown command, a store error.
        assert_eq!(parse_oauth_account_count(""), None);
        assert_eq!(
            parse_oauth_account_count("Error: Unknown command token\n"),
            None
        );
        assert_eq!(
            parse_oauth_account_count("Error: database is locked\n"),
            None
        );
    }

    #[test]
    fn subscription_token_env_covers_the_oauth_env_hook() {
        assert!(SUBSCRIPTION_TOKEN_ENV.contains(&"ANTHROPIC_OAUTH_TOKEN"));
        assert!(SUBSCRIPTION_TOKEN_ENV.contains(&"CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(
            !SUBSCRIPTION_TOKEN_ENV.contains(&"ANTHROPIC_API_KEY"),
            "API keys stay usable"
        );
    }

    #[test]
    fn disabled_providers_parse_and_merge_without_dropping_the_users_list() {
        assert_eq!(parse_disabled_providers("[]\n"), Some(vec![]));
        assert_eq!(
            parse_disabled_providers("[\"openai\"]"),
            Some(vec!["openai".to_string()])
        );
        assert_eq!(parse_disabled_providers("not json"), None);
        let described =
            r#"{"key":"disabledProviders","value":["openai"],"type":"array","description":""}"#;
        assert_eq!(
            parse_disabled_providers(described),
            Some(vec!["openai".to_string()])
        );
        assert_eq!(
            parse_disabled_providers(r#"{"key":"disabledProviders","value":[],"type":"array"}"#),
            Some(vec![])
        );
        assert_eq!(
            parse_disabled_providers(r#"{"key":"disabledProviders"}"#),
            None
        );
        assert_eq!(parse_disabled_providers(r#"{"value":"openai"}"#), None);
        let user = vec!["openai".to_string()];
        let ag = || "google-antigravity".to_string();
        assert_eq!(
            host_disabled_providers(&user, Some(0)),
            vec!["openai".to_string(), ag()]
        );
        assert_eq!(
            host_disabled_providers(&user, Some(1)),
            vec!["openai".to_string(), "anthropic".to_string(), ag()]
        );
        // Fail closed when the OAuth check could not run.
        assert_eq!(
            host_disabled_providers(&[], None),
            vec!["anthropic".to_string(), ag()]
        );
        // No duplicate when the user already disabled them.
        assert_eq!(
            host_disabled_providers(&["anthropic".to_string(), ag()], Some(2)),
            vec!["anthropic".to_string(), ag()]
        );
    }

    #[test]
    fn overlay_lists_disabled_providers_as_quoted_yaml() {
        let text = host_config_overlay(&["openai".to_string(), "anthropic".to_string()]);
        assert!(
            text.ends_with("disabledProviders:\n  - \"openai\"\n  - \"anthropic\"\n"),
            "{text}"
        );
    }

    #[test]
    fn overlay_quotes_hostile_ids_onto_a_single_line() {
        let text = host_config_overlay(&["a\"b\n- injected".to_string()]);
        assert!(
            text.ends_with("disabledProviders:\n  - \"a\\\"b\\n- injected\"\n"),
            "{text}"
        );
    }

    #[test]
    fn claude_selectors_are_recognised_for_the_early_refusal() {
        assert!(selects_anthropic_model("anthropic/claude-haiku-5-5"));
        assert!(selects_anthropic_model("claude-opus-4-8"));
        assert!(!selects_anthropic_model("google/gemini-3.8-flash"));
        assert!(!selects_anthropic_model("zai/glm-5.3"));
        assert!(selects_antigravity_model(
            "google-antigravity/gemini-3.8-flash"
        ));
        assert!(
            !selects_antigravity_model("google/gemini-3.8-flash"),
            "AI Studio API key route stays usable"
        );
    }
}
