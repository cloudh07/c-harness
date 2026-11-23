//! `ch-runner preflight`: what the app checks before a first session, from the runner's side.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use ch_protocol::preflight::{ModelTest, ModelTestRequest, PreflightReport};
use ch_protocol::runner::RUNNER_PROTOCOL_VERSION;
use ch_protocol::{host_config_overlay, host_disabled_providers, parse_version, validate_model};

use crate::git::Git;
use crate::omp;
use crate::scratch::PrivateDir;
use crate::scrub::Scrubber;

/// A model test request holds one key and a model name, so this is plenty.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

/// Checks git, omp and the project files in `workdir`, and asks the model for one word when
/// `request` is given. Without a `workdir` only omp is checked.
///
/// omp runs in a private temp directory, not in `workdir`, so the repository's own omp
/// config cannot load extensions before anyone has looked at it.
pub fn preflight(
    workdir: Option<&Path>,
    omp_program: &OsStr,
    request: Option<&ModelTestRequest>,
) -> io::Result<PreflightReport> {
    let dir = PrivateDir::create("ch-runner")?;
    let (git_problem, verify_commands) = match workdir {
        Some(workdir) => check_workdir(&dir, workdir)?,
        None => (None, Vec::new()),
    };
    let omp_version = omp::probe_version(omp_program, dir.path(), &BTreeMap::new())
        .as_deref()
        .and_then(parse_version);
    let model_test = request.map(|request| test_model(omp_program, &dir, request));
    Ok(PreflightReport {
        protocol: RUNNER_PROTOCOL_VERSION,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        omp_version,
        git_problem,
        verify_commands: verify_commands.into_iter().map(str::to_owned).collect(),
        model_test,
    })
}

fn check_workdir(
    dir: &PrivateDir,
    workdir: &Path,
) -> io::Result<(Option<String>, Vec<&'static str>)> {
    let workdir = std::path::absolute(workdir)?;
    if !workdir.is_dir() {
        let problem = format!("{} is not a folder on this machine", workdir.display());
        return Ok((Some(problem), Vec::new()));
    }
    dir.ensure_outside(&workdir)?;
    let commands = crate::detect::verify_commands(&workdir);
    Ok((git_problem(dir, &workdir)?, commands))
}

/// The same checks a session start makes: `workdir` is the top of a git working tree with a
/// readable `HEAD` or no commits yet.
fn git_problem(dir: &PrivateDir, workdir: &Path) -> io::Result<Option<String>> {
    let no_hooks = dir.path().join("no-hooks");
    fs::create_dir(&no_hooks)?;
    let mut git = Git::new(workdir, no_hooks);
    Ok(git.pin().and_then(|()| git.base()).err())
}

fn test_model(omp_program: &OsStr, dir: &PrivateDir, request: &ModelTestRequest) -> ModelTest {
    let refused = |detail: String| ModelTest {
        answered: false,
        detail: Scrubber::for_env(request.env.values()).scrub(&detail),
    };
    if let Err(e) = omp::check_env(&request.env) {
        return refused(e);
    }
    if let Some(Err(e)) = request.model.as_deref().map(validate_model) {
        return refused(e);
    }
    let providers = omp::probe_providers(omp_program, dir.path(), &request.env);
    let disabled = host_disabled_providers(&providers.user_disabled, providers.oauth_accounts);
    let host = match dir.write_file("host-config.yml", host_config_overlay(&disabled).as_bytes()) {
        Ok(host) => host,
        Err(e) => return refused(format!("could not write omp's config overlay: {e}")),
    };
    omp::test_model(
        omp_program,
        dir.path(),
        &request.env,
        request.model.as_deref(),
        &host,
    )
}

/// Reads the one JSON request `--test-model` expects on stdin.
pub fn read_request(input: impl Read) -> io::Result<ModelTestRequest> {
    let mut bytes = Vec::new();
    input.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the model test request is larger than {MAX_REQUEST_BYTES} bytes"),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the model test request is not valid JSON: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_request() {
        let request = read_request(&br#"{"env":{"K":"v"},"model":"sonnet"}"#[..]).unwrap();
        assert_eq!(request.env["K"], "v");
        assert_eq!(request.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn refuses_a_bad_or_huge_request() {
        assert!(read_request(&b"not json"[..]).is_err());
        let huge = vec![b' '; usize::try_from(MAX_REQUEST_BYTES).unwrap() + 1];
        let err = read_request(&huge[..]).unwrap_err();
        assert!(err.to_string().contains("larger than"));
    }

    #[test]
    fn refused_request_never_runs_omp_and_hides_the_value() {
        let dir = PrivateDir::create("ch-runner-test").unwrap();
        let request = ModelTestRequest {
            env: BTreeMap::from([("BAD=NAME".into(), "a-value-long-enough".into())]),
            model: None,
        };
        let missing = OsStr::new("ch-runner-test-no-such-omp");
        let result = test_model(missing, &dir, &request);
        assert!(!result.answered);
        assert!(result.detail.contains("not a usable"), "{}", result.detail);

        let request = ModelTestRequest {
            env: BTreeMap::new(),
            model: Some("--yolo".into()),
        };
        let result = test_model(missing, &dir, &request);
        assert!(!result.answered);
        assert!(
            result.detail.contains("must not start with"),
            "{}",
            result.detail
        );
    }

    #[test]
    fn missing_folder_is_reported_not_fatal() {
        let parent = tempfile::tempdir().unwrap();
        let gone = parent.path().join("gone");
        let report =
            preflight(Some(&gone), OsStr::new("ch-runner-test-no-such-omp"), None).unwrap();
        assert!(report.git_problem.unwrap().contains("not a folder"));
        assert_eq!(report.omp_version, None);
        assert_eq!(report.verify_commands, Vec::<String>::new());
        assert_eq!(report.model_test, None);
    }

    #[test]
    fn without_a_workdir_only_omp_is_checked() {
        let report = preflight(None, OsStr::new("ch-runner-test-no-such-omp"), None).unwrap();
        assert_eq!(report.git_problem, None);
        assert_eq!(report.verify_commands, Vec::<String>::new());
        assert_eq!(report.omp_version, None);
    }
}
