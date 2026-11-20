//! What the views show, worked out without a window so it can be tested.

use std::fmt::Write as _;

use ch_core::SessionState;
use ch_protocol::runner::{Evidence, GitAuthor, LandRequest, PullRequestSpec};

use crate::settings::Settings;

/// Git's own advice for a commit subject line.
const SUBJECT_LIMIT: usize = 72;

pub fn session_id(epoch_ms: u64) -> String {
    format!("s{epoch_ms}")
}

pub fn branch_for(session: &str) -> String {
    format!("ch/{session}")
}

pub fn transcript_file_name(session: &str) -> String {
    format!("{session}.jsonl")
}

/// The inbox row a session sits under, in the order the sidebar lists them.
pub fn inbox_slot(state: SessionState) -> usize {
    match state {
        SessionState::NeedsYou | SessionState::Idle | SessionState::Failed => 0,
        SessionState::Starting | SessionState::Running => 1,
        SessionState::NeedsReview => 2,
        SessionState::Landed => 3,
    }
}

pub fn inbox_counts(state: Option<SessionState>) -> [usize; 4] {
    let mut counts = [0; 4];
    if let Some(state) = state {
        counts[inbox_slot(state)] = 1;
    }
    counts
}

pub fn state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Idle => "Waiting for a prompt",
        SessionState::Starting => "Starting",
        SessionState::Running => "Running",
        SessionState::NeedsYou => "Needs you",
        SessionState::NeedsReview => "Needs review",
        SessionState::Landed => "Landed",
        SessionState::Failed => "Failed",
    }
}

/// The first line of the first prompt, cut to fit a commit subject.
pub fn commit_message(first_prompt: &str, session: &str) -> String {
    let line = first_prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    if line.is_empty() {
        return format!("C-Harness session {session}");
    }
    if line.chars().count() <= SUBJECT_LIMIT {
        return line.to_string();
    }
    let cut: String = line.chars().take(SUBJECT_LIMIT - 3).collect();
    format!("{}...", cut.trim_end())
}

/// How a check ended and how long it took, such as `passed in 1.5s`.
pub fn check_outcome(run: &Evidence) -> String {
    let outcome = match (run.timed_out, run.exit_code) {
        (true, _) => "timed out".to_string(),
        (false, Some(0)) => "passed".to_string(),
        (false, Some(code)) => format!("failed with exit code {code}"),
        (false, None) => "ended without an exit code".to_string(),
    };
    let tenths = run.duration_ms / 100;
    format!("{outcome} in {}.{}s", tenths / 10, tenths % 10)
}

pub fn check_passed(run: &Evidence) -> bool {
    !run.timed_out && run.exit_code == Some(0)
}

pub fn pr_body(session: &str, evidence: &[Evidence]) -> String {
    let mut body = format!("Opened by C-Harness from session `{session}`.\n");
    if evidence.is_empty() {
        body.push_str("\nNo checks were run before landing.\n");
        return body;
    }
    body.push_str("\nChecks run before landing:\n\n");
    for run in evidence {
        let _ = writeln!(body, "- `{}`: {}", run.command, check_outcome(run));
    }
    body
}

/// An empty port means the SSH default.
pub fn parse_port(text: &str) -> Result<Option<u16>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!(
            "{text} is not a port. Use a number from 1 to 65535."
        )),
        Ok(port) => Ok(Some(port)),
    }
}

/// The first `max` characters of `text`, and whether anything was cut.
pub fn first_chars(text: &str, max: usize) -> (&str, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (&text[..end], true),
        None => (text, false),
    }
}

/// The last `max` characters of `text`, and whether anything was cut.
pub fn last_chars(text: &str, max: usize) -> (&str, bool) {
    if max == 0 {
        return ("", !text.is_empty());
    }
    match text.char_indices().rev().nth(max - 1) {
        Some((0, _)) | None => (text, false),
        Some((start, _)) => (&text[start..], true),
    }
}

pub fn land_request(
    settings: &Settings,
    session: &str,
    first_prompt: &str,
    token: String,
    evidence: &[Evidence],
) -> LandRequest {
    let repo = settings.github_repo.trim().to_string();
    let message = commit_message(first_prompt, session);
    LandRequest {
        id: String::new(),
        branch: branch_for(session),
        message: message.clone(),
        author: GitAuthor {
            name: settings.author_name.trim().to_string(),
            email: settings.author_email.trim().to_string(),
        },
        remote_url: format!("https://github.com/{repo}.git"),
        token: Some(token),
        pull_request: Some(PullRequestSpec {
            repo,
            base: settings.base_branch.trim().to_string(),
            title: message,
            body: pr_body(session, evidence),
        }),
    }
}

/// Whether a pull request link from the runner is safe to open: a pull request page in the
/// repository these settings land into, and nothing else.
pub fn pr_link_allowed(url: &str, github_repo: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let (Some(owner), Some(name), Some("pull"), Some(number), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    let repo = format!("{owner}/{name}");
    repo.eq_ignore_ascii_case(github_repo.trim())
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(command: &str, exit_code: Option<i32>, timed_out: bool) -> Evidence {
        Evidence {
            id: "req_1".into(),
            command: command.into(),
            exit_code,
            duration_ms: 1500,
            head: None,
            output_tail: String::new(),
            truncated: false,
            timed_out,
        }
    }

    #[test]
    fn ids_and_branches_follow_one_scheme() {
        let id = session_id(1_700_000_000_000);
        assert_eq!(id, "s1700000000000");
        assert_eq!(branch_for(&id), "ch/s1700000000000");
        assert_eq!(transcript_file_name(&id), "s1700000000000.jsonl");
    }

    #[test]
    fn each_state_counts_under_one_inbox_row() {
        assert_eq!(inbox_counts(None), [0, 0, 0, 0]);
        assert_eq!(inbox_counts(Some(SessionState::NeedsYou)), [1, 0, 0, 0]);
        assert_eq!(inbox_counts(Some(SessionState::Failed)), [1, 0, 0, 0]);
        assert_eq!(inbox_counts(Some(SessionState::Running)), [0, 1, 0, 0]);
        assert_eq!(inbox_counts(Some(SessionState::NeedsReview)), [0, 0, 1, 0]);
        assert_eq!(inbox_counts(Some(SessionState::Landed)), [0, 0, 0, 1]);
    }

    #[test]
    fn commit_subject_is_the_first_line_cut_on_a_char_boundary() {
        assert_eq!(
            commit_message("\n  Fix the parser  \nmore", "s1"),
            "Fix the parser"
        );
        assert_eq!(commit_message("   \n", "s1"), "C-Harness session s1");

        let long = "Sửa lỗi ".repeat(20);
        let subject = commit_message(&long, "s1");
        assert!(subject.chars().count() <= SUBJECT_LIMIT, "{subject}");
        assert!(subject.ends_with("..."));
    }

    #[test]
    fn pr_body_lists_each_check_and_its_outcome() {
        let body = pr_body(
            "s1",
            &[
                evidence("cargo test", Some(0), false),
                evidence("cargo clippy", Some(101), false),
                evidence("npm test", None, true),
            ],
        );
        assert!(body.contains("- `cargo test`: passed in 1.5s"), "{body}");
        assert!(body.contains("failed with exit code 101"), "{body}");
        assert!(body.contains("- `npm test`: timed out"), "{body}");
        assert!(pr_body("s1", &[]).contains("No checks were run"));
    }

    #[test]
    fn land_request_targets_the_configured_repository() {
        let settings = Settings {
            author_name: " Ada ".into(),
            author_email: "ada@example.com".into(),
            github_repo: "example/demo".into(),
            base_branch: "trunk".into(),
            ..Settings::default()
        };
        let request = land_request(&settings, "s1", "Fix it", "tok".into(), &[]);
        assert_eq!(request.branch, "ch/s1");
        assert_eq!(request.message, "Fix it");
        assert_eq!(request.author.name, "Ada");
        assert_eq!(request.remote_url, "https://github.com/example/demo.git");
        assert_eq!(request.token.as_deref(), Some("tok"));
        let pr = request.pull_request.unwrap();
        assert_eq!(pr.repo, "example/demo");
        assert_eq!(pr.base, "trunk");
        assert_eq!(pr.title, "Fix it");
    }

    #[test]
    fn check_outcome_reads_like_a_sentence() {
        assert_eq!(
            check_outcome(&evidence("t", Some(0), false)),
            "passed in 1.5s"
        );
        assert!(check_passed(&evidence("t", Some(0), false)));
        assert!(!check_passed(&evidence("t", Some(0), true)));
        assert!(!check_passed(&evidence("t", None, false)));
    }

    #[test]
    fn ports_are_optional_but_must_be_real() {
        assert_eq!(parse_port(""), Ok(None));
        assert_eq!(parse_port(" 2222 "), Ok(Some(2222)));
        assert!(parse_port("0").is_err());
        assert!(parse_port("70000").is_err());
        assert!(parse_port("22a").is_err());
    }

    #[test]
    fn clipping_keeps_whole_characters() {
        assert_eq!(first_chars("abc", 5), ("abc", false));
        assert_eq!(first_chars("ăâđ", 2), ("ăâ", true));
        assert_eq!(last_chars("abc", 3), ("abc", false));
        assert_eq!(last_chars("ăâđ", 2), ("âđ", true));
        assert_eq!(last_chars("abc", 0), ("", true));
        assert_eq!(last_chars("", 0), ("", false));
    }

    #[test]
    fn only_pull_request_pages_of_the_target_repo_open() {
        let repo = "Example/Demo";
        assert!(pr_link_allowed(
            "https://github.com/example/demo/pull/12",
            repo
        ));
        assert!(!pr_link_allowed(
            "https://github.com/example/other/pull/12",
            repo
        ));
        assert!(!pr_link_allowed(
            "http://github.com/example/demo/pull/12",
            repo
        ));
        assert!(!pr_link_allowed(
            "https://github.com.evil.io/example/demo/pull/1",
            repo
        ));
        assert!(!pr_link_allowed(
            "https://github.com/example/demo/pull/12/files",
            repo
        ));
        assert!(!pr_link_allowed(
            "https://github.com/example/demo/pull/",
            repo
        ));
        assert!(!pr_link_allowed(
            "https://github.com/example/demo/pull/1?x=y",
            repo
        ));
        assert!(!pr_link_allowed(
            "https://github.com/example/demo/issues/1",
            repo
        ));
    }
}
