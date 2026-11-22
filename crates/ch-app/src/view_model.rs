//! What the views show, worked out without a window so it can be tested.

use std::fmt::Write as _;

use ch_core::{ContextReading, ContextState, SessionState, UsageTotals};
use ch_protocol::runner::{Evidence, GitAuthor, LandRequest, PullRequestSpec};

use crate::settings::{Location, Settings};

mod failure;
mod labels;
pub use failure::{EMPTY_RUN_NOTE, failure_text, first_reply_text, retry_text};
pub use labels::{age_label, context_near_full, live_title};

/// Git's own advice for a commit subject line.
const SUBJECT_LIMIT: usize = 72;
/// About what fits in the session header next to its buttons.
const TITLE_LIMIT: usize = 80;

pub fn session_id(epoch_ms: u64) -> String {
    format!("s{epoch_ms}")
}

pub fn branch_for(session: &str) -> String {
    ch_runtime::session_branch(session)
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
    // Land holds the work to the last check, so a run on another tree checked earlier work.
    let landed = evidence.last().and_then(still_tree);
    for run in evidence {
        let earlier = landed.is_some_and(|tree| still_tree(run) != Some(tree));
        let note = if earlier { " (on earlier work)" } else { "" };
        let _ = writeln!(body, "- `{}`: {}{note}", run.command, check_outcome(run));
    }
    body
}

/// The tree a check ran on, when the work stood still while it ran.
fn still_tree(run: &Evidence) -> Option<&str> {
    match (&run.tree_before, &run.tree_after) {
        (Some(before), Some(after)) if before == after => Some(before),
        _ => None,
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffTotals {
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

/// The totals from the line `git diff --stat` ends with, such as
/// ` 3 files changed, 10 insertions(+), 2 deletions(-)`. `None` when the stat is empty or was
/// cut before git finished it: git ends the stat with a newline and a cut stat has none.
pub fn diff_totals(stat: &str) -> Option<DiffTotals> {
    let body = stat.strip_suffix('\n')?;
    let line = body.rsplit('\n').next()?.trim();
    let mut parts = line.split(", ");
    let (count, rest) = parts.next()?.split_once(' ')?;
    if !matches!(rest, "file changed" | "files changed") {
        return None;
    }
    let mut totals = DiffTotals {
        files: count.parse().ok()?,
        insertions: 0,
        deletions: 0,
    };
    for part in parts {
        let (count, rest) = part.split_once(' ')?;
        let n: u32 = count.parse().ok()?;
        match rest {
            "insertion(+)" | "insertions(+)" => totals.insertions = n,
            "deletion(-)" | "deletions(-)" => totals.deletions = n,
            _ => return None,
        }
    }
    Some(totals)
}

/// Each whole file line of `git diff --stat`: the path and the count git printed (`12`, `Bin`).
/// A line cut off at the end of a truncated stat is skipped rather than shown with a wrong count.
pub fn diff_files(stat: &str) -> Vec<(&str, &str)> {
    stat.split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .filter_map(|line| line.rsplit_once(" | "))
        .map(|(path, change)| {
            (
                path.trim(),
                change.split_whitespace().next().unwrap_or_default(),
            )
        })
        .collect()
}

/// A one-line name for a session: the first written line of its prompt, cut to fit a header.
/// Empty when the prompt has no text yet.
pub fn session_title(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    match first_chars(line, TITLE_LIMIT) {
        (shown, true) => format!("{}…", shown.trim_end()),
        (shown, false) => shown.to_string(),
    }
}

/// What the header calls the repository: its folder name, or the SSH host it runs on.
pub fn repo_label(settings: &Settings) -> String {
    match &settings.location {
        Location::Ssh { host, .. } => host.trim().to_string(),
        Location::Local => settings
            .repo
            .trim()
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_string(),
    }
}

/// The header's usage line, or `None` before the first turn ends. "≥" marks a sum that misses
/// turns omp could not count; the cost is omp's own estimate.
pub fn usage_label(totals: &UsageTotals) -> Option<String> {
    if totals.turns == 0 {
        return None;
    }
    let counts = [
        totals.input,
        totals.output,
        totals.cache_read,
        totals.cache_write,
    ];
    if counts.iter().all(|n| *n == 0) {
        return Some("Usage not reported".into());
    }
    let at_least = if totals.tokens_complete { "" } else { "≥" };
    let mut parts = vec![
        format!("{at_least}{} in", compact(totals.input)),
        format!("{at_least}{} out", compact(totals.output)),
    ];
    if totals.cache_read > 0 {
        parts.push(format!("{at_least}{} cached", compact(totals.cache_read)));
    }
    if let Some(cost) = totals.cost_usd {
        if totals.cost_complete {
            parts.push(if cost < 0.01 {
                "<$0.01".into()
            } else {
                format!("${cost:.2}")
            });
        } else {
            // A lower bound must not round up, so whole cents only.
            let floor = (cost * 100.0).floor() / 100.0;
            if floor >= 0.01 {
                parts.push(format!("≥${floor:.2}"));
            }
        }
    }
    Some(parts.join(" · "))
}

/// `950`, `12.3k`, `1.2M`: one decimal, cut rather than rounded.
fn compact(n: u64) -> String {
    let (whole, tenth, unit) = match n {
        0..1_000 => return n.to_string(),
        1_000..1_000_000 => (n / 1_000, n % 1_000 / 100, "k"),
        _ => (n / 1_000_000, n % 1_000_000 / 100_000, "M"),
    };
    if tenth == 0 {
        format!("{whole}{unit}")
    } else {
        format!("{whole}.{tenth}{unit}")
    }
}

/// The header's context line, or `None` before omp first answered. "≈" because omp estimates
/// what came after its last reply; "<1%" says that already.
pub fn context_label(state: ContextState) -> Option<String> {
    match state {
        ContextState::NotRead => None,
        ContextState::Compacting => Some("Compacting context…".into()),
        ContextState::Unknown => Some("Context unknown".into()),
        ContextState::Known(ContextReading {
            tokens,
            window: Some(window),
        }) => {
            let percent = labels::context_percent(tokens, window);
            let shown = if percent == 0 && tokens > 0 {
                "<1".to_string()
            } else {
                format!("≈{percent}")
            };
            Some(format!("Context {shown}% of {}", compact(window)))
        }
        ContextState::Known(ContextReading {
            tokens,
            window: None,
        }) => Some(format!("Context ≈{} tokens", compact(tokens))),
    }
}

/// Names the two ends of a turn diff. Turn 0 is the session's start, `None` the working tree.
pub fn turn_diff_title(from: u32, to: Option<u32>) -> String {
    let end = |turn: u32| {
        if turn == 0 {
            "the session start".to_string()
        } else {
            format!("checkpoint {turn}")
        }
    };
    let to = to.map_or_else(|| "the working tree".to_string(), end);
    format!("Changes from {} to {to}", end(from))
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
            tree_before: None,
            tree_after: None,
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
    fn pr_body_marks_checks_that_ran_on_earlier_work() {
        let on = |command: &str, tree: &str| Evidence {
            tree_before: Some(tree.into()),
            tree_after: Some(tree.into()),
            ..evidence(command, Some(0), false)
        };
        let body = pr_body("s1", &[on("cargo test", "t1"), on("cargo clippy", "t2")]);
        assert!(
            body.contains("- `cargo test`: passed in 1.5s (on earlier work)\n"),
            "{body}"
        );
        assert!(
            body.contains("- `cargo clippy`: passed in 1.5s\n"),
            "{body}"
        );
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

    const STAT: &str = " src/main.rs  | 12 ++++++++----\n README.md    |  3 +++\n logo.png     | Bin 0 -> 1234 bytes\n 3 files changed, 11 insertions(+), 4 deletions(-)\n";

    fn totals(files: u32, insertions: u32, deletions: u32) -> DiffTotals {
        DiffTotals {
            files,
            insertions,
            deletions,
        }
    }

    #[test]
    fn diff_totals_reads_the_summary_line() {
        assert_eq!(diff_totals(STAT), Some(totals(3, 11, 4)));
        let one = " a.rs | 1 +\n 1 file changed, 1 insertion(+)\n";
        assert_eq!(diff_totals(one), Some(totals(1, 1, 0)));
        let gone = " a.rs | 2 --\n 1 file changed, 2 deletions(-)\n";
        assert_eq!(diff_totals(gone), Some(totals(1, 0, 2)));
        let crlf = " a.rs | 1 +\r\n 1 file changed, 1 insertion(+)\r\n";
        assert_eq!(diff_totals(crlf), Some(totals(1, 1, 0)));
    }

    #[test]
    fn diff_totals_refuses_what_git_did_not_finish() {
        assert_eq!(diff_totals(""), None);
        assert_eq!(diff_totals(STAT.trim_end_matches('\n')), None);
        assert_eq!(diff_totals(" src/main.rs | 12 ++++++++----\n src/li"), None);
        assert_eq!(diff_totals("warning: something odd\n"), None);
        assert_eq!(diff_totals(" 2 files changed, 3 carrots(+)\n"), None);
    }

    #[test]
    fn diff_files_lists_whole_lines_only() {
        assert_eq!(
            diff_files(STAT),
            vec![
                ("src/main.rs", "12"),
                ("README.md", "3"),
                ("logo.png", "Bin")
            ]
        );
        assert_eq!(
            diff_files(" src/main.rs | 12 ++++\n src/lib.rs | 1"),
            vec![("src/main.rs", "12")]
        );
        assert_eq!(diff_files(""), Vec::<(&str, &str)>::new());
    }

    #[test]
    fn session_title_is_the_first_written_line() {
        assert_eq!(
            session_title("\n   Fix the login bug  \nthen tidy up"),
            "Fix the login bug"
        );
        assert_eq!(session_title("  \n "), "");
        let title = session_title(&"word ".repeat(40));
        assert!(title.ends_with('…'), "{title}");
        assert!(title.chars().count() <= TITLE_LIMIT + 1, "{title}");
    }

    #[test]
    fn repo_label_names_the_folder_or_the_host() {
        let windows = Settings {
            repo: "D:\\code\\c-harness\\".to_string(),
            ..Settings::default()
        };
        assert_eq!(repo_label(&windows), "c-harness");
        let unix = Settings {
            repo: "/home/dev/work/app/".to_string(),
            ..Settings::default()
        };
        assert_eq!(repo_label(&unix), "app");
        let remote = Settings {
            location: Location::Ssh {
                host: " devbox ".to_string(),
                port: None,
                remote_workdir: "/srv/app".to_string(),
                remote_runner: "ch-runner".to_string(),
            },
            ..Settings::default()
        };
        assert_eq!(repo_label(&remote), "devbox");
    }

    fn used(input: u64, output: u64, cache_read: u64, cost: Option<f64>) -> UsageTotals {
        UsageTotals {
            turns: 2,
            input,
            output,
            cache_read,
            cache_write: 0,
            cost_usd: cost,
            tokens_complete: true,
            cost_complete: cost.is_some(),
        }
    }

    #[test]
    fn context_label_says_how_full_the_window_is() {
        let known = |tokens, window| ContextState::Known(ContextReading { tokens, window });
        let cases = [
            (ContextState::NotRead, None),
            (ContextState::Compacting, Some("Compacting context…")),
            (ContextState::Unknown, Some("Context unknown")),
            (known(18_003, Some(200_000)), Some("Context ≈9% of 200k")),
            (known(18_003, Some(1_050_000)), Some("Context ≈2% of 1M")),
            (known(100, Some(200_000)), Some("Context <1% of 200k")),
            (known(0, Some(200_000)), Some("Context ≈0% of 200k")),
            (known(210_000, Some(200_000)), Some("Context ≈105% of 200k")),
            (known(18_003, None), Some("Context ≈18k tokens")),
        ];
        for (state, label) in cases {
            assert_eq!(context_label(state).as_deref(), label, "{state:?}");
        }
    }

    #[test]
    fn usage_label_is_hidden_before_the_first_turn() {
        assert_eq!(usage_label(&UsageTotals::default()), None);
    }

    #[test]
    fn usage_label_shows_counts_cache_and_cost() {
        let full = used(12_345, 4_567, 1_250_000, Some(0.126));
        assert_eq!(
            usage_label(&full).as_deref(),
            Some("12.3k in · 4.5k out · 1.2M cached · $0.13")
        );
        let small = used(950, 40, 0, Some(0.004));
        assert_eq!(
            usage_label(&small).as_deref(),
            Some("950 in · 40 out · <$0.01")
        );
        let round = used(2_000, 7, 0, None);
        assert_eq!(usage_label(&round).as_deref(), Some("2k in · 7 out"));
    }

    #[test]
    fn usage_label_marks_lower_bounds() {
        let gaps = UsageTotals {
            tokens_complete: false,
            cost_complete: false,
            ..used(1_500, 20, 0, Some(0.129))
        };
        assert_eq!(
            usage_label(&gaps).as_deref(),
            Some("≥1.5k in · ≥20 out · ≥$0.12")
        );
        let tiny = UsageTotals {
            cost_complete: false,
            ..used(10, 1, 0, Some(0.001))
        };
        assert_eq!(usage_label(&tiny).as_deref(), Some("10 in · 1 out"));
    }

    #[test]
    fn usage_label_says_when_nothing_was_counted() {
        let none = UsageTotals {
            tokens_complete: false,
            ..used(0, 0, 0, None)
        };
        assert_eq!(usage_label(&none).as_deref(), Some("Usage not reported"));
    }

    #[test]
    fn turn_diff_title_names_both_ends() {
        assert_eq!(
            turn_diff_title(0, Some(1)),
            "Changes from the session start to checkpoint 1"
        );
        assert_eq!(
            turn_diff_title(2, Some(3)),
            "Changes from checkpoint 2 to checkpoint 3"
        );
        assert_eq!(
            turn_diff_title(3, None),
            "Changes from checkpoint 3 to the working tree"
        );
    }
}
