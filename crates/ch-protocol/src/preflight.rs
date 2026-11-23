//! What `ch-runner preflight` reports about the place sessions run.
//!
//! The app runs `ch-runner preflight [--workdir <dir>]` before a first session. The runner
//! prints one JSON [`PreflightReport`] on stdout and exits 0. Without `--workdir` it checks omp
//! alone. With `--test-model` it first reads one JSON [`ModelTestRequest`] from stdin and asks
//! the model for a one-word reply.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::runner::{MASKED, RUNNER_PROTOCOL_VERSION};

/// The oldest runner protocol that knows `ch-runner preflight`.
pub const PREFLIGHT_PROTOCOL: u32 = 4;

const _: () = assert!(PREFLIGHT_PROTOCOL <= RUNNER_PROTOCOL_VERSION);

/// omp's `--max-cost` for the model test, in US dollars. omp stops sending requests past it,
/// but one request already on its way can still cost more.
pub const MODEL_TEST_MAX_COST: &str = "0.05";

/// Every test command `ch-runner preflight` can suggest. The app drops anything else a runner
/// sends, so a host it reaches over SSH cannot put its own command in the verify field.
pub const VERIFY_COMMANDS: [&str; 6] = [
    "cargo test",
    "npm test",
    "pnpm test",
    "yarn test",
    "go test ./...",
    "pytest",
];

/// What the runner found in the working directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightReport {
    /// The runner's [`RUNNER_PROTOCOL_VERSION`].
    pub protocol: u32,
    /// The runner's version, without the program name.
    pub version: String,
    /// What `omp --version` printed, or `None` when omp did not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omp_version: Option<String>,
    /// Why git cannot work in the directory, or `None` when it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_problem: Option<String>,
    /// Test commands the project files point to, in a fixed order. Empty when none match.
    #[serde(default)]
    pub verify_commands: Vec<String>,
    /// The model test's outcome, when one was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_test: Option<ModelTest>,
}

/// How the one-word model test went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTest {
    pub answered: bool,
    /// One line for the UI: the reason it failed, or what omp printed when it answered.
    /// Secrets the runner was given are scrubbed out.
    pub detail: String,
}

/// What `ch-runner preflight --test-model` reads from stdin.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTestRequest {
    /// Environment for omp only, usually the model provider's key.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The model to ask, as for `omp --model`. `None` uses omp's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl fmt::Debug for ModelTestRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env: BTreeMap<&str, &str> = self.env.keys().map(|k| (k.as_str(), MASKED)).collect();
        f.debug_struct("ModelTestRequest")
            .field("env", &env)
            .field("model", &self.model)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_round_trips() {
        let report = PreflightReport {
            protocol: PREFLIGHT_PROTOCOL,
            version: "0.1.0".into(),
            omp_version: Some("18.13.1".into()),
            git_problem: None,
            verify_commands: vec!["cargo test".into()],
            model_test: Some(ModelTest {
                answered: true,
                detail: "ok".into(),
            }),
        };
        let line = serde_json::to_string(&report).unwrap();
        assert!(!line.contains("git_problem"));
        assert_eq!(
            serde_json::from_str::<PreflightReport>(&line).unwrap(),
            report
        );
    }

    #[test]
    fn report_with_only_required_fields_parses() {
        let report: PreflightReport =
            serde_json::from_str(r#"{"protocol":4,"version":"0.1.0"}"#).unwrap();
        assert_eq!(report.omp_version, None);
        assert_eq!(report.verify_commands, Vec::<String>::new());
        assert_eq!(report.model_test, None);
    }

    #[test]
    fn request_debug_hides_env_values() {
        let request = ModelTestRequest {
            env: BTreeMap::from([("ANTHROPIC_API_KEY".into(), "sk-ant-secret".into())]),
            model: Some("sonnet".into()),
        };
        let shown = format!("{request:?}");
        assert!(shown.contains("ANTHROPIC_API_KEY"));
        assert!(!shown.contains("sk-ant-secret"));
        let line = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<ModelTestRequest>(&line).unwrap(),
            request
        );
    }
}
