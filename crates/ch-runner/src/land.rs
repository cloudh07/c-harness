//! Committing the work on a branch, pushing it, and opening a pull request.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use ch_protocol::runner::{GitAuthor, LandRequest};

use crate::base64;
use crate::git::{self, Base, Git};
use crate::pr::{PrOpener, PrRequest};
use crate::proc_tree;
use crate::scratch::PrivateDir;

/// The one place a land sends a token. Any other host, or a local path, lands without one.
const GITHUB_PREFIX: &str = "https://github.com/";

/// Transfers slower than this many bytes a second for [`LOW_SPEED_SECS`] are abandoned.
const LOW_SPEED_BYTES: &str = "1000";
const LOW_SPEED_SECS: &str = "60";

pub struct Landed {
    pub branch: String,
    pub commit: String,
    pub pull_request_url: Option<String>,
}

pub struct Lander<'a> {
    pub git: &'a Git,
    pub base: &'a Base,
    pub session: &'a PrivateDir,
    /// Branch to the commit this session already landed there, so a retry reuses it.
    pub landed: &'a mut HashMap<String, String>,
    pub opener: &'a dyn PrOpener,
}

impl Lander<'_> {
    pub fn land(&mut self, request: &LandRequest) -> Result<Landed, String> {
        validate(request, self.opener.requires_token())?;
        check_branch_name(self.git, &request.branch)?;
        let reference = format!("refs/heads/{}", request.branch);
        if self.checked_out_branch().as_deref() == Some(reference.as_str()) {
            return Err(format!(
                "{} is the branch checked out in the working tree",
                request.branch
            ));
        }
        let commit = self.commit(request, &reference)?;
        let scratch = PrivateDir::create("ch-runner-push")
            .map_err(|e| format!("could not make a temp directory: {e}"))?;
        scratch
            .ensure_outside(self.git.workdir())
            .map_err(|e| e.to_string())?;
        push(
            self.git,
            &scratch,
            &reference,
            &request.remote_url,
            request.token.as_deref(),
        )?;
        let pull_request_url = match &request.pull_request {
            None => None,
            Some(spec) => Some(self.opener.open(
                &PrRequest {
                    repo: &spec.repo,
                    head: &request.branch,
                    base: &spec.base,
                    title: &spec.title,
                    body: &spec.body,
                    token: request.token.as_deref(),
                },
                scratch.path(),
            )?),
        };
        Ok(Landed {
            branch: request.branch.clone(),
            commit,
            pull_request_url,
        })
    }

    fn checked_out_branch(&self) -> Option<String> {
        let mut command = self.git.command();
        command.args(["symbolic-ref", "-q", "HEAD"]);
        git::run_text(command, "symbolic-ref").ok()
    }

    /// Commits the working tree on top of the base and points the branch at it.
    fn commit(&mut self, request: &LandRequest, reference: &str) -> Result<String, String> {
        let index = self.session.unique_path("land-index");
        let tree = self
            .git
            .stage(self.base, &index)
            .and_then(|()| self.git.write_tree(&index));
        // The index is scratch; a failure to remove it only wastes the temp dir.
        let _ = fs::remove_file(&index);
        let tree = tree?;
        if tree == self.git.base_tree(self.base)? {
            return Err("nothing to land: the working tree has no changes".to_owned());
        }
        let previous = self.landed.get(&request.branch).cloned();
        if let Some(previous) = &previous
            && self.tree_of(previous)? == tree
        {
            return Ok(previous.clone());
        }

        let mut commit_tree = self.git.command();
        commit_tree.args(["commit-tree", "--no-gpg-sign"]);
        if self.base.is_commit {
            commit_tree.args(["-p", &self.base.id]);
        }
        commit_tree.arg(&tree);
        set_identity(&mut commit_tree, &request.author);
        let message = commit_message(&request.message);
        let commit = git::run_with_input(commit_tree, "commit-tree", message.as_bytes())?;
        let commit = String::from_utf8_lossy(&commit).trim().to_owned();

        let mut update = self.git.command();
        update
            .args(["update-ref", "-m", "c-harness land", reference, &commit])
            // An empty old value makes git refuse to overwrite a branch this session did not make.
            .arg(previous.as_deref().unwrap_or(""));
        git::run(update, "update-ref")?;
        self.landed.insert(request.branch.clone(), commit.clone());
        Ok(commit)
    }

    fn tree_of(&self, commit: &str) -> Result<String, String> {
        let mut command = self.git.command();
        command
            .args(["rev-parse", "--verify", "-q"])
            .arg(format!("{commit}^{{tree}}"));
        git::run_text(command, "rev-parse tree")
    }
}

fn set_identity(command: &mut Command, author: &GitAuthor) {
    command
        .env("GIT_AUTHOR_NAME", &author.name)
        .env("GIT_AUTHOR_EMAIL", &author.email)
        .env("GIT_COMMITTER_NAME", &author.name)
        .env("GIT_COMMITTER_EMAIL", &author.email);
}

fn commit_message(message: &str) -> String {
    let mut text = message.trim_end().to_owned();
    text.push('\n');
    text
}

/// Pushes the branch from a fresh bare repository, so nothing in the working tree's own git
/// config (remotes, credential helpers, hooks, `insteadOf` rewrites) takes part in the push.
fn push(
    git: &Git,
    scratch: &PrivateDir,
    reference: &str,
    remote_url: &str,
    token: Option<&str>,
) -> Result<(), String> {
    let config = scratch.path().join("gitconfig");
    fs::write(&config, b"").map_err(|e| format!("could not write a temp file: {e}"))?;
    let repo = scratch.path().join("repo.git");
    let refspec = format!("{reference}:{reference}");

    let mut init = scratch_git(scratch.path(), &config, None);
    init.args(["init", "--bare", "-q", "--template="]);
    if object_format(git).as_deref() == Some("sha256") {
        init.arg("--object-format=sha256");
    }
    init.arg(&repo);
    git::run(init, "init")?;

    let mut fetch = scratch_git(&repo, &config, None);
    fetch
        .args(["fetch", "-q", "--no-tags"])
        .arg(git.fetch_source())
        .arg(&refspec);
    git::run(fetch, "fetch")?;

    let mut push = scratch_git(&repo, &config, token.map(|token| (remote_url, token)));
    push.args(["push", "-q", remote_url, &refspec]);
    git::run(push, "push")?;
    Ok(())
}

fn object_format(git: &Git) -> Option<String> {
    let mut command = git.command();
    command.args(["rev-parse", "--show-object-format"]);
    git::run_text(command, "rev-parse --show-object-format").ok()
}

/// A git command isolated from every config file and inherited `GIT_*` variable.
fn scratch_git(dir: &Path, config: &Path, credential: Option<(&str, &str)>) -> Command {
    let mut command = proc_tree::command("git");
    command
        // An empty helper list: no credential helper may answer for, or store, this push.
        .args(["-c", "credential.helper="])
        .current_dir(dir)
        .stdin(Stdio::null());
    for (name, _) in std::env::vars_os() {
        if is_git_variable(&name) {
            command.env_remove(name);
        }
    }
    command.env_remove("SSH_ASKPASS");
    command.envs(push_env(config, credential));
    command
}

fn is_git_variable(name: &OsString) -> bool {
    name.to_str()
        .is_some_and(|name| name.to_ascii_uppercase().starts_with("GIT_"))
}

/// The environment of every scratch git process; with a credential, the push's token travels
/// only here, as a header git sends to `remote_url` alone.
pub fn push_env(config: &Path, credential: Option<(&str, &str)>) -> Vec<(String, OsString)> {
    let mut env = vec![
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".into()),
        (
            "GIT_CONFIG_GLOBAL".to_owned(),
            config.as_os_str().to_owned(),
        ),
        ("GIT_TERMINAL_PROMPT".to_owned(), "0".into()),
        ("GIT_ALLOW_PROTOCOL".to_owned(), "https:file".into()),
        (
            "GIT_HTTP_LOW_SPEED_LIMIT".to_owned(),
            LOW_SPEED_BYTES.into(),
        ),
        ("GIT_HTTP_LOW_SPEED_TIME".to_owned(), LOW_SPEED_SECS.into()),
    ];
    if let Some((remote_url, token)) = credential {
        let basic = base64::encode(format!("x-access-token:{token}").as_bytes());
        env.extend([
            ("GIT_CONFIG_COUNT".to_owned(), "1".into()),
            (
                "GIT_CONFIG_KEY_0".to_owned(),
                format!("http.{remote_url}.extraheader").into(),
            ),
            (
                "GIT_CONFIG_VALUE_0".to_owned(),
                format!("AUTHORIZATION: basic {basic}").into(),
            ),
        ]);
    }
    env
}

/// Checks everything in a land request that needs no git.
pub fn validate(request: &LandRequest, opener_needs_token: bool) -> Result<(), String> {
    validate_branch_shape(&request.branch)?;
    validate_remote(&request.remote_url)?;
    if let Some(token) = &request.token {
        validate_token(token)?;
        if !is_github_https(&request.remote_url) {
            return Err(format!(
                "remote {:?} refused: a token is only ever sent to {GITHUB_PREFIX}",
                request.remote_url
            ));
        }
    }
    validate_author(&request.author)?;
    if request.message.trim().is_empty() {
        return Err("the commit message is empty".to_owned());
    }
    if request.message.contains('\0') {
        return Err("the commit message holds a NUL byte".to_owned());
    }
    if let Some(spec) = &request.pull_request {
        validate_repo(&spec.repo)?;
        if spec.base.trim().is_empty() || spec.title.trim().is_empty() {
            return Err("a pull request needs a base branch and a title".to_owned());
        }
        if opener_needs_token && request.token.is_none() {
            return Err("opening a pull request needs a token".to_owned());
        }
    }
    Ok(())
}

fn validate_branch_shape(branch: &str) -> Result<(), String> {
    if branch.is_empty() || branch.starts_with('-') || branch.chars().any(char::is_control) {
        return Err(format!("{branch:?} is not a valid branch name"));
    }
    Ok(())
}

/// Asks git whether `branch` is a branch name, refusing anything git would expand (`@{-1}`).
fn check_branch_name(git: &Git, branch: &str) -> Result<(), String> {
    let mut command = git.command();
    command.args(["check-ref-format", "--branch", branch]);
    let normalized = git::run_text(command, "check-ref-format")
        .map_err(|_| format!("{branch:?} is not a valid branch name"))?;
    if normalized != branch {
        return Err(format!("{branch:?} is not a valid branch name"));
    }
    Ok(())
}

/// Allows `https://` without credentials, or an absolute local path; never a transport git
/// would hand to a helper program (`ext::`, `<helper>::`), ssh, or plain http.
pub fn validate_remote(url: &str) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("remote {url:?} refused: {why}"));
    if url.chars().any(char::is_control) {
        return refuse("it holds control characters");
    }
    if url.contains("::") || url.to_ascii_lowercase().starts_with("ext:") {
        return refuse("transport helpers are not allowed");
    }
    if let Some(rest) = url.strip_prefix("https://") {
        if rest.chars().any(char::is_whitespace) {
            return refuse("a URL cannot hold whitespace");
        }
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() {
            return refuse("it has no host");
        }
        if authority.contains('@') {
            return refuse("credentials go in the token, not the URL");
        }
        return Ok(());
    }
    let lower = url.to_ascii_lowercase();
    if lower.contains("://") {
        return refuse("only https:// remotes are allowed");
    }
    if url.starts_with("\\\\") || url.starts_with("//") {
        return refuse("network paths are not allowed");
    }
    if url.starts_with('-') || !Path::new(url).is_absolute() {
        return refuse("a local remote must be an absolute path");
    }
    Ok(())
}

fn is_github_https(url: &str) -> bool {
    url.get(..GITHUB_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(GITHUB_PREFIX))
}

/// `owner/name` with GitHub's characters.
pub fn validate_repo(repo: &str) -> Result<(), String> {
    let valid_part = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if valid_part(owner) && valid_part(name) => Ok(()),
        _ => Err(format!("{repo:?} is not an owner/name repository")),
    }
}

/// A token goes into an HTTP header, so only visible ASCII is allowed.
pub fn validate_token(token: &str) -> Result<(), String> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("the token holds characters a token cannot have".to_owned());
    }
    Ok(())
}

fn validate_author(author: &GitAuthor) -> Result<(), String> {
    for (what, value) in [("name", &author.name), ("email", &author.email)] {
        if value.trim().is_empty()
            || value
                .chars()
                .any(|c| c.is_control() || matches!(c, '<' | '>'))
        {
            return Err(format!("the author {what} {value:?} is not usable"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ch_protocol::runner::PullRequestSpec;

    fn request() -> LandRequest {
        LandRequest {
            id: "l1".into(),
            branch: "ch/s1".into(),
            message: "Add a test".into(),
            author: GitAuthor {
                name: "Octo Cat".into(),
                email: "octo@example.com".into(),
            },
            remote_url: "https://github.com/octo/repo.git".into(),
            token: Some("test-token-not-real".into()),
            pull_request: Some(PullRequestSpec {
                repo: "octo/repo".into(),
                base: "main".into(),
                title: "Add a test".into(),
                body: String::new(),
            }),
        }
    }

    #[test]
    fn accepts_a_normal_request() {
        assert_eq!(validate(&request(), true), Ok(()));
    }

    #[test]
    fn accepts_https_and_absolute_local_remotes() {
        assert!(validate_remote("https://github.com/octo/repo.git").is_ok());
        let local = std::env::temp_dir().join("remote.git");
        assert!(validate_remote(local.to_str().unwrap()).is_ok());
    }

    #[test]
    fn refuses_remotes_that_could_run_programs_or_leak_credentials() {
        for url in [
            "ext::sh -c touch% /tmp/pwned",
            "ext:whatever",
            "fd::17",
            "https://github.com/octo/repo.git::x",
            "http://github.com/octo/repo.git",
            "ssh://git@github.com/octo/repo.git",
            "git://github.com/octo/repo.git",
            "file:///srv/repo.git",
            "git@github.com:octo/repo.git",
            "https://user:pw@github.com/octo/repo.git",
            "https://test-token-not-real@github.com/octo/repo.git",
            "https://",
            "https://github.com/octo/repo .git",
            "relative/repo.git",
            "-uhack",
            "--upload-pack=touch",
        ] {
            assert!(validate_remote(url).is_err(), "{url}");
        }
    }

    #[test]
    fn refuses_bad_repositories() {
        for repo in [
            "octo",
            "octo/",
            "/repo",
            "octo/repo/x",
            "octo/re po",
            "../repo",
            "octo/..",
            "octo/repo?x",
        ] {
            assert!(validate_repo(repo).is_err(), "{repo}");
        }
        assert!(validate_repo("octo-org/my_repo.rs").is_ok());
    }

    #[test]
    fn refuses_tokens_that_could_break_a_header() {
        for token in ["", "a b", "a\nb", "a\rX-Evil: 1", "tökén"] {
            assert!(validate_token(token).is_err(), "{token:?}");
        }
        assert!(validate_token("github_pat_test-token-not-real").is_ok());
    }

    #[test]
    fn refuses_branches_that_look_like_options() {
        let mut bad = request();
        bad.branch = "--force".into();
        assert!(validate(&bad, true).is_err());
        bad.branch = String::new();
        assert!(validate(&bad, true).is_err());
        bad.branch = "a\nb".into();
        assert!(validate(&bad, true).is_err());
    }

    #[test]
    fn a_token_only_goes_to_github() {
        for remote in [
            "https://gitlab.example/octo/repo.git",
            "https://github.com.evil.example/octo/repo.git",
            "/srv/git/repo.git",
            r"\\host\share\repo.git",
        ] {
            let mut elsewhere = request();
            elsewhere.remote_url = remote.into();
            assert!(validate(&elsewhere, false).is_err(), "{remote}");
        }
        assert!(validate_remote(r"\\host\share\repo.git").is_err());
        assert!(validate_remote("//host/share/repo.git").is_err());
        let mut mixed_case = request();
        mixed_case.remote_url = "https://GitHub.com/octo/repo.git".into();
        assert!(validate(&mixed_case, false).is_ok());
    }

    #[test]
    fn a_pull_request_needs_a_token_when_the_opener_does() {
        let mut no_token = request();
        no_token.token = None;
        assert!(validate(&no_token, true).is_err());
        assert!(validate(&no_token, false).is_ok());
        no_token.pull_request = None;
        assert!(validate(&no_token, true).is_ok());
    }

    #[test]
    fn refuses_unusable_authors_and_messages() {
        let mut bad = request();
        bad.author.email = "a>b".into();
        assert!(validate(&bad, true).is_err());
        let mut bad = request();
        bad.author.name = " ".into();
        assert!(validate(&bad, true).is_err());
        let mut bad = request();
        bad.message = "\n".into();
        assert!(validate(&bad, true).is_err());
    }

    #[test]
    fn push_env_isolates_git_and_scopes_the_token_to_the_remote() {
        let config = Path::new("gitconfig");
        let env = push_env(
            config,
            Some(("https://github.com/octo/repo.git", "test-token-not-real")),
        );
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.to_str().unwrap().to_owned())
        };
        assert_eq!(get("GIT_CONFIG_NOSYSTEM").as_deref(), Some("1"));
        assert_eq!(get("GIT_CONFIG_GLOBAL").as_deref(), Some("gitconfig"));
        assert_eq!(get("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
        assert_eq!(get("GIT_ALLOW_PROTOCOL").as_deref(), Some("https:file"));
        assert_eq!(get("GIT_CONFIG_COUNT").as_deref(), Some("1"));
        assert_eq!(
            get("GIT_CONFIG_KEY_0").as_deref(),
            Some("http.https://github.com/octo/repo.git.extraheader")
        );
        assert_eq!(
            get("GIT_CONFIG_VALUE_0").as_deref(),
            Some("AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46dGVzdC10b2tlbi1ub3QtcmVhbA==")
        );
        assert!(
            env.iter()
                .all(|(_, value)| !value.to_str().unwrap().contains("test-token-not-real"))
        );
    }

    #[test]
    fn push_env_without_a_token_sets_no_header() {
        let env = push_env(Path::new("gitconfig"), None);
        assert!(
            env.iter()
                .all(|(key, _)| !key.starts_with("GIT_CONFIG_KEY"))
        );
    }

    #[test]
    fn commit_message_ends_with_one_newline() {
        assert_eq!(commit_message("Add a test\n\n"), "Add a test\n");
        assert_eq!(commit_message("Add a test"), "Add a test\n");
    }
}
