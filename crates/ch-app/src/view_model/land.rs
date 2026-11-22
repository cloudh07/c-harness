//! What a land sends, and the checklist that decides whether it may go.

use std::fmt::Write as _;

use ch_core::Failure;
use ch_protocol::runner::{Evidence, GitAuthor, LandRequest, PullRequestSpec, SecretScan};
use ch_runtime::RUNNER_INSTALL_COMMAND;

use super::{branch_for, check_outcome, check_passed};
use crate::driver::RunnerBlock;
use crate::settings::Settings;

/// Git's own advice for a commit subject line.
const SUBJECT_LIMIT: usize = 72;
/// The runner stage a refused or failed land reports.
pub const LAND_STAGE: &str = "land";
const AGAIN: &str = "Run the checks again.";

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

pub fn land_request(
    settings: &Settings,
    session: &str,
    first_prompt: &str,
    token: String,
    evidence: &[Evidence],
    acknowledged_tree: Option<String>,
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
        acknowledged_tree,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Pass,
    Fail,
    Unknown,
}

impl Mark {
    /// The word shown next to the icon, so the state never rests on colour.
    pub fn word(self) -> &'static str {
        match self {
            Self::Pass => "Pass",
            Self::Fail => "Fail",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandItem {
    pub name: &'static str,
    pub mark: Mark,
    pub reason: String,
    /// Whether this item alone keeps Land off. A refused secret scan fails without blocking,
    /// since removing the secret or landing anyway are both ways on.
    pub blocks: bool,
}

impl LandItem {
    fn new(name: &'static str, mark: Mark, reason: impl Into<String>, blocks: bool) -> Self {
        Self {
            name,
            mark,
            reason: reason.into(),
            blocks,
        }
    }

    /// What a screen reader says for the row.
    pub fn spoken(&self) -> String {
        format!("{}: {}. {}", self.name, self.mark.word(), self.reason)
    }
}

/// Everything shown before a land: one item per check, and the branch and pull request text
/// the land would send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checklist {
    pub items: Vec<LandItem>,
    pub branch: String,
    pub title: String,
    pub body: String,
}

impl Checklist {
    /// What keeps Land off, in plain words, or `None` when it may go. Land, Land anyway and the
    /// send itself all ask this one question.
    pub fn blocked(&self) -> Option<String> {
        let reasons: Vec<&str> = self
            .items
            .iter()
            .filter(|item| item.blocks)
            .map(|item| item.reason.as_str())
            .collect();
        (!reasons.is_empty()).then(|| reasons.join(" "))
    }
}

/// The checklist for the land these inputs would send. `failure` is the session's current
/// failure; only a land's counts.
pub fn checklist(
    settings: &Settings,
    session: &str,
    first_prompt: &str,
    evidence: &[Evidence],
    failure: Option<&Failure>,
    runner: Option<RunnerBlock>,
) -> Checklist {
    let failure = failure.filter(|f| f.stage == LAND_STAGE);
    let scan = failure.and_then(|f| f.secret_scan.as_ref());
    let branch = branch_for(session);
    let title = commit_message(first_prompt, session);
    let items = vec![
        evidence_item(
            evidence,
            &settings.verify,
            failure.is_some() && scan.is_none(),
        ),
        scan_item(runner, scan),
        branch_item(&branch, &settings.base_branch),
        pr_item(settings, &title),
    ];
    Checklist {
        items,
        branch,
        title,
        body: pr_body(session, evidence),
    }
}

/// The last check stands for the work. The runner holds a land to it too, but only while the
/// runner that ran it is alive, so a pass here never promises the work is unchanged.
fn evidence_item(evidence: &[Evidence], verify: &str, land_refused: bool) -> LandItem {
    const NAME: &str = "Checks";
    let Some(run) = evidence.last() else {
        let reason = if verify.trim().is_empty() {
            "No check is set up, so none has run. The pull request will say so."
        } else {
            "No check has run on this work. The pull request will say so."
        };
        return LandItem::new(NAME, Mark::Unknown, reason, false);
    };
    let command = &run.command;
    // Checks run only in review, and a refused land leaves review, so a block there would last.
    let fail = |why: String, again: &str| {
        if land_refused {
            LandItem::new(
                NAME,
                Mark::Fail,
                format!("{why} The pull request will say so."),
                false,
            )
        } else {
            LandItem::new(NAME, Mark::Fail, format!("{why} {again}"), true)
        }
    };
    if !check_passed(run) {
        return fail(
            format!("`{command}` {}.", check_outcome(run)),
            "Fix the work, then run the checks again.",
        );
    }
    match (&run.tree_before, &run.tree_after) {
        (Some(before), Some(after)) if before != after => {
            fail(format!("The work changed while `{command}` ran."), AGAIN)
        }
        (Some(_), Some(_)) if land_refused => LandItem::new(
            NAME,
            Mark::Unknown,
            format!("`{command}` passed before the runner refused the last land."),
            false,
        ),
        (Some(_), Some(_)) => LandItem::new(
            NAME,
            Mark::Pass,
            format!(
                "`{command}` {}. If the work changed since, run the checks again.",
                check_outcome(run)
            ),
            false,
        ),
        _ => fail(
            format!("The runner could not record which work `{command}` ran on."),
            AGAIN,
        ),
    }
}

/// The scan runs inside the land on the runner, so before the first press its result is unknown.
fn scan_item(runner: Option<RunnerBlock>, refused: Option<&SecretScan>) -> LandItem {
    const NAME: &str = "Secret scan";
    let reinstall = format!(
        "Reinstall it where the session runs with `{RUNNER_INSTALL_COMMAND} --force`, then start \
         the session again."
    );
    match (runner, refused) {
        (Some(RunnerBlock::Outdated), _) => LandItem::new(
            NAME,
            Mark::Fail,
            format!("This ch-runner is too old to scan a land for secrets. {reinstall}"),
            true,
        ),
        (Some(RunnerBlock::Silent), _) => LandItem::new(
            NAME,
            Mark::Fail,
            format!(
                "This ch-runner never said which version it is and may not scan a land for \
                 secrets. {reinstall}"
            ),
            true,
        ),
        (None, Some(scan)) if scan.too_large => LandItem::new(
            NAME,
            Mark::Fail,
            "The change was too large to scan, so nothing was landed. Read it before you land \
             anyway.",
            false,
        ),
        (None, Some(scan)) => {
            let count = scan.findings.len() + scan.more as usize;
            let lines = if count == 1 {
                "1 added line looks like a secret".to_string()
            } else {
                format!("{count} added lines look like secrets")
            };
            LandItem::new(
                NAME,
                Mark::Fail,
                format!(
                    "{lines}, so nothing was landed. Remove them, or land anyway once you have \
                     checked them."
                ),
                false,
            )
        }
        (None, None) => LandItem::new(
            NAME,
            Mark::Unknown,
            "Runs when you press Land. A finding stops the land before anything is pushed.",
            false,
        ),
    }
}

fn branch_item(branch: &str, base: &str) -> LandItem {
    const NAME: &str = "Branch";
    if let Some(why) = branch_problem(branch) {
        return LandItem::new(
            NAME,
            Mark::Fail,
            format!("Git would refuse `{branch}`: {why}."),
            true,
        );
    }
    if branch == base.trim() {
        return LandItem::new(
            NAME,
            Mark::Fail,
            format!("`{branch}` is also the base branch, so the pull request would be empty."),
            true,
        );
    }
    LandItem::new(NAME, Mark::Pass, format!("Pushes `{branch}`."), false)
}

/// Why `git check-ref-format --branch` would refuse `name`, or a name it allows that is never
/// meant. The runner checks again, and also refuses the branch checked out in the work.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Git compares `.lock` exactly.
fn branch_problem(name: &str) -> Option<&'static str> {
    const FORBIDDEN: &[char] = &['~', '^', ':', '?', '*', '[', '\\'];
    if name.is_empty() {
        return Some("the name is empty");
    }
    if name.starts_with('-') {
        return Some("it starts with -");
    }
    if name == "HEAD" || name == "@" {
        return Some("Git keeps that name for itself");
    }
    if name
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || FORBIDDEN.contains(&c))
    {
        return Some("it has a space, a control character, or one of ~ ^ : ? * [ \\");
    }
    if name.contains("..") || name.contains("@{") || name.contains("//") {
        return Some("it has .., @{ or //");
    }
    if name.starts_with('/') || name.ends_with('/') || name.ends_with('.') {
        return Some("it starts or ends with /, or ends with .");
    }
    if name
        .split('/')
        .any(|part| part.starts_with('.') || part.ends_with(".lock"))
    {
        return Some("a part of it starts with . or ends with .lock");
    }
    None
}

fn pr_item(settings: &Settings, title: &str) -> LandItem {
    const NAME: &str = "Pull request";
    let problems = settings.land_problems();
    if !problems.is_empty() {
        let list: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();
        return LandItem::new(
            NAME,
            Mark::Fail,
            format!("Finish the Landing settings first. {}", list.join(" ")),
            true,
        );
    }
    if title.trim().is_empty() {
        return LandItem::new(NAME, Mark::Fail, "The pull request has no title.", true);
    }
    LandItem::new(
        NAME,
        Mark::Pass,
        format!(
            "Opens into `{}` on {}, committed as {}.",
            settings.base_branch.trim(),
            settings.github_repo.trim(),
            settings.author_name.trim()
        ),
        false,
    )
}

#[cfg(test)]
mod tests;
