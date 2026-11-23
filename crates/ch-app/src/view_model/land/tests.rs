use ch_core::FailureKind;
use ch_protocol::runner::{SecretFinding, SecretKind};

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

/// A check that ran from `before` to `after`.
fn run(exit_code: Option<i32>, before: Option<&str>, after: Option<&str>) -> Evidence {
    Evidence {
        tree_before: before.map(Into::into),
        tree_after: after.map(Into::into),
        ..evidence("cargo test", exit_code, false)
    }
}

fn passed() -> Evidence {
    run(Some(0), Some("t1"), Some("t1"))
}

fn ready_settings() -> Settings {
    Settings {
        verify: "cargo test".into(),
        author_name: " Ada ".into(),
        author_email: "ada@example.com".into(),
        github_repo: "example/demo".into(),
        base_branch: "main".into(),
        ..Settings::default()
    }
}

fn land_failure(secret_scan: Option<SecretScan>) -> Failure {
    Failure {
        stage: LAND_STAGE.into(),
        kind: FailureKind::Permanent,
        message: "the push was rejected".into(),
        secret_scan,
    }
}

fn scan(findings: usize, more: u32, too_large: bool) -> SecretScan {
    let finding = SecretFinding {
        path: "src/main.rs".into(),
        line: 3,
        kind: SecretKind::ProviderToken,
    };
    SecretScan {
        tree: "4b825dc".into(),
        findings: vec![finding; findings],
        more,
        too_large,
    }
}

fn item(list: &Checklist, name: &str) -> LandItem {
    list.items
        .iter()
        .find(|item| item.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no {name} item in {list:?}"))
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
        base_branch: "trunk".into(),
        ..ready_settings()
    };
    let request = land_request(&settings, "s1", "Fix it", "tok".into(), &[], None);
    assert_eq!(request.acknowledged_tree, None, "a plain land is scanned");
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
fn land_anyway_carries_the_tree_the_person_looked_at() {
    let request = land_request(
        &Settings::default(),
        "s1",
        "Fix it",
        "tok".into(),
        &[],
        Some("4b825dc".into()),
    );
    assert_eq!(request.acknowledged_tree.as_deref(), Some("4b825dc"));
}

#[test]
fn only_pull_request_pages_of_the_target_repo_open() {
    let repo = "Example/Demo";
    assert!(pr_link_allowed(
        "https://github.com/example/demo/pull/12",
        repo
    ));
    for url in [
        "https://github.com/example/other/pull/12",
        "http://github.com/example/demo/pull/12",
        "https://github.com.evil.io/example/demo/pull/1",
        "https://github.com/example/demo/pull/12/files",
        "https://github.com/example/demo/pull/",
        "https://github.com/example/demo/pull/1?x=y",
        "https://github.com/example/demo/issues/1",
    ] {
        assert!(!pr_link_allowed(url, repo), "{url}");
    }
}

#[test]
fn the_preview_is_what_the_land_sends() {
    let settings = ready_settings();
    let evidence = [passed()];
    let list = checklist(&settings, "s1", "Fix it\nmore", &evidence, None, None, None);
    let request = land_request(
        &settings,
        "s1",
        "Fix it\nmore",
        "tok".into(),
        &evidence,
        None,
    );
    let pr = request.pull_request.unwrap();
    assert_eq!(list.branch, request.branch);
    assert_eq!(list.title, pr.title);
    assert_eq!(list.title, request.message);
    assert_eq!(list.body, pr.body);
}

#[test]
fn a_clean_land_has_no_blockers() {
    let list = checklist(
        &ready_settings(),
        "s1",
        "Fix it",
        &[passed()],
        None,
        None,
        None,
    );
    assert_eq!(list.blocked(), None, "{list:?}");
    let marks: Vec<Mark> = list.items.iter().map(|item| item.mark).collect();
    assert_eq!(
        marks,
        [Mark::Unknown, Mark::Unknown, Mark::Pass, Mark::Pass],
        "with no current diff the app can't match the check to the work, and the scan runs \
         during Land"
    );
}

/// With no current diff the app has nothing to match the checked tree against, so it cannot
/// prove a passed check still holds.
#[test]
fn a_passed_check_with_no_diff_reads_unknown_and_does_not_block() {
    let list = checklist(
        &ready_settings(),
        "s1",
        "Fix it",
        &[passed()],
        None,
        None,
        None,
    );
    let checks = item(&list, "Checks");
    assert_eq!(checks.mark, Mark::Unknown);
    assert!(!checks.blocks);
    assert_eq!(
        checks.reason,
        "`cargo test` passed in 1.5s. Press Show diff or Refresh diff under Changes to match the \
         check to the work as it is now."
    );
    assert_eq!(list.blocked(), None);
}

fn diff(tree: Option<&str>) -> Diff {
    Diff {
        base: "abc".into(),
        stat: String::new(),
        patch: String::new(),
        truncated: false,
        tree: tree.map(Into::into),
    }
}

fn checks_against(diff: &Diff) -> LandItem {
    let list = checklist(
        &ready_settings(),
        "s1",
        "Fix it",
        &[passed()],
        Some(diff),
        None,
        None,
    );
    item(&list, "Checks").clone()
}

#[test]
fn a_passed_check_on_the_tree_the_diff_shows_passes() {
    let list = checklist(
        &ready_settings(),
        "s1",
        "Fix it",
        &[passed()],
        Some(&diff(Some("t1"))),
        None,
        None,
    );
    let checks = item(&list, "Checks");
    assert_eq!(checks.mark, Mark::Pass);
    assert!(!checks.blocks);
    assert_eq!(
        checks.reason,
        "`cargo test` passed in 1.5s on the work the latest diff shows."
    );
    assert_eq!(list.blocked(), None);
}

/// The runner refuses this land, but it says why, so the row only warns.
#[test]
fn a_passed_check_on_other_work_is_not_a_pass() {
    let checks = checks_against(&diff(Some("t2")));
    assert_eq!(checks.mark, Mark::Unknown);
    assert!(!checks.blocks);
    assert!(checks.reason.contains("other work"), "{checks:?}");
}

#[test]
fn a_diff_without_a_tree_reads_unknown() {
    let checks = checks_against(&diff(None));
    assert_eq!(checks.mark, Mark::Unknown);
    assert!(!checks.blocks);
    assert!(
        checks.reason.contains("did not say which work"),
        "{checks:?}"
    );
}

#[test]
fn a_refused_land_outweighs_a_matching_diff() {
    let refused = land_failure(None);
    let list = checklist(
        &ready_settings(),
        "s1",
        "Fix it",
        &[passed()],
        Some(&diff(Some("t1"))),
        Some(&refused),
        None,
    );
    let checks = item(&list, "Checks");
    assert_eq!(checks.mark, Mark::Unknown);
    assert!(
        checks.reason.contains("refused the last land"),
        "{checks:?}"
    );
}

/// The review gap end to end: a diff on the checked tree, then a prompt that changes the work.
/// The session stops offering that diff, so the row can't pass on it.
#[test]
fn a_prompt_after_the_diff_takes_the_pass_away() {
    use ch_core::{ClientAction, Session};
    use ch_protocol::runner::route_runner_line;

    let mut session = Session::new();
    let feed = |session: &mut Session, line: &str| {
        session.apply_line(&route_runner_line(line).expect("a routable line"));
    };
    session.apply_action(&ClientAction::StartSent);
    feed(&mut session, r#"{"type":"ready"}"#);
    session.apply_action(&ClientAction::DiffSent { id: "d1".into() });
    feed(
        &mut session,
        r#"{"type":"ch_diff_result","id":"d1","base":"abc","stat":"","patch":"","truncated":false,"tree":"t1"}"#,
    );
    let mark = |session: &Session| {
        let list = checklist(
            &ready_settings(),
            "s1",
            "Fix it",
            &[passed()],
            session.current_diff(),
            None,
            None,
        );
        item(&list, "Checks").mark
    };
    assert_eq!(mark(&session), Mark::Pass);

    session.apply_action(&ClientAction::PromptSent);
    feed(&mut session, r#"{"type":"session_settled"}"#);
    assert_eq!(mark(&session), Mark::Unknown);
}

#[test]
fn checks_with_no_run_are_unknown_and_do_not_block() {
    let list = checklist(&ready_settings(), "s1", "Fix it", &[], None, None, None);
    let checks = item(&list, "Checks");
    assert_eq!(checks.mark, Mark::Unknown);
    assert!(!checks.blocks);
    assert!(checks.reason.contains("No check has run"), "{checks:?}");

    let unset = Settings {
        verify: "  ".into(),
        ..ready_settings()
    };
    let checks = item(
        &checklist(&unset, "s1", "Fix it", &[], None, None, None),
        "Checks",
    );
    assert_eq!(checks.mark, Mark::Unknown);
    assert!(checks.reason.contains("No check is set up"), "{checks:?}");
}

#[test]
fn a_failed_or_timed_out_last_check_blocks() {
    for last in [
        run(Some(101), Some("t1"), Some("t1")),
        Evidence {
            timed_out: true,
            ..passed()
        },
    ] {
        let list = checklist(&ready_settings(), "s1", "Fix it", &[last], None, None, None);
        let checks = item(&list, "Checks");
        assert_eq!(checks.mark, Mark::Fail, "{checks:?}");
        assert!(checks.blocks);
        assert!(checks.reason.contains("`cargo test`"), "{checks:?}");
        assert!(list.blocked().is_some());
    }
}

#[test]
fn only_the_last_check_counts() {
    let earlier = run(Some(1), Some("t0"), Some("t0"));
    let list = checklist(
        &ready_settings(),
        "s1",
        "Fix it",
        &[earlier, passed()],
        None,
        None,
        None,
    );
    let checks = item(&list, "Checks");
    assert_eq!(checks.mark, Mark::Unknown);
    assert!(!checks.blocks);
}

#[test]
fn a_check_on_work_that_moved_while_it_ran_blocks() {
    let stale = run(Some(0), Some("t1"), Some("t2"));
    let checks = item(
        &checklist(
            &ready_settings(),
            "s1",
            "Fix it",
            &[stale],
            None,
            None,
            None,
        ),
        "Checks",
    );
    assert_eq!(checks.mark, Mark::Fail);
    assert!(checks.blocks);
    assert!(checks.reason.contains("changed while"), "{checks:?}");
}

#[test]
fn a_check_with_no_recorded_work_blocks() {
    for unrecorded in [
        run(Some(0), None, None),
        run(Some(0), Some("t1"), None),
        run(Some(0), None, Some("t1")),
    ] {
        let checks = item(
            &checklist(
                &ready_settings(),
                "s1",
                "Fix it",
                &[unrecorded],
                None,
                None,
                None,
            ),
            "Checks",
        );
        assert_eq!(checks.mark, Mark::Fail, "{checks:?}");
        assert!(checks.blocks);
    }
}

#[test]
fn a_pass_turns_unknown_after_the_runner_refused_a_land() {
    let refused = land_failure(None);
    let checks = item(
        &checklist(
            &ready_settings(),
            "s1",
            "Fix it",
            &[passed()],
            None,
            Some(&refused),
            None,
        ),
        "Checks",
    );
    assert_eq!(checks.mark, Mark::Unknown);
    assert!(!checks.blocks, "the runner says why; a retry may be right");
    assert!(
        checks.reason.contains("refused the last land"),
        "{checks:?}"
    );
}

/// Checks run only while the session is in review, and a refused land leaves review, so a check
/// that blocked there would lock the session for good. A scan refusal counts too: a push refusal
/// lets a bad check through, and the retry can then stop on the scan.
#[test]
fn a_bad_check_stops_blocking_after_the_runner_refused_a_land() {
    for refused in [land_failure(None), land_failure(Some(scan(1, 0, false)))] {
        for run in [
            run(Some(1), Some("t1"), Some("t1")),
            run(Some(0), Some("t1"), Some("t2")),
            run(Some(0), None, None),
        ] {
            let list = checklist(
                &ready_settings(),
                "s1",
                "Fix it",
                &[run],
                None,
                Some(&refused),
                None,
            );
            let checks = item(&list, "Checks");
            assert_eq!(checks.mark, Mark::Fail, "{checks:?}");
            assert!(!checks.blocks, "{checks:?}");
            assert!(
                checks.reason.ends_with("The pull request will say so."),
                "{checks:?}"
            );
            assert_eq!(list.blocked(), None, "{refused:?}");
        }
    }
}

#[test]
fn failures_outside_the_land_do_not_touch_the_checklist() {
    let prompt = Failure {
        stage: "prompt".into(),
        ..land_failure(Some(scan(1, 0, false)))
    };
    let list = checklist(
        &ready_settings(),
        "s1",
        "Fix it",
        &[passed()],
        None,
        Some(&prompt),
        None,
    );
    let checks = item(&list, "Checks");
    assert!(checks.reason.contains("Press Show diff"), "{checks:?}");
    assert_eq!(item(&list, "Secret scan").mark, Mark::Unknown);
}

#[test]
fn the_scan_is_unknown_until_land_runs_it() {
    let scan = item(
        &checklist(
            &ready_settings(),
            "s1",
            "Fix it",
            &[passed()],
            None,
            None,
            None,
        ),
        "Secret scan",
    );
    assert_eq!(scan.mark, Mark::Unknown);
    assert!(!scan.blocks);
    assert!(
        scan.reason.starts_with("Runs when you press Land"),
        "{scan:?}"
    );
}

#[test]
fn a_runner_older_than_the_scan_blocks() {
    for block in [RunnerBlock::Outdated, RunnerBlock::Silent] {
        let list = checklist(
            &ready_settings(),
            "s1",
            "Fix it",
            &[passed()],
            None,
            None,
            Some(block),
        );
        let scan = item(&list, "Secret scan");
        assert_eq!(scan.mark, Mark::Fail, "{block:?}");
        assert!(scan.blocks, "{block:?}");
        assert!(
            scan.reason
                .contains(&format!("`{RUNNER_INSTALL_COMMAND} --force`")),
            "{scan:?}"
        );
        assert!(list.blocked().is_some());
    }
}

#[test]
fn a_refused_scan_fails_but_leaves_land_anyway_open() {
    for (refused, expected) in [
        (scan(1, 0, false), "1 added line looks like a secret"),
        (scan(2, 3, false), "5 added lines look like secrets"),
        (scan(0, 0, true), "too large to scan"),
    ] {
        let failure = land_failure(Some(refused));
        let list = checklist(
            &ready_settings(),
            "s1",
            "Fix it",
            &[passed()],
            None,
            Some(&failure),
            None,
        );
        let scan = item(&list, "Secret scan");
        assert_eq!(scan.mark, Mark::Fail);
        assert!(!scan.blocks);
        assert!(scan.reason.contains(expected), "{scan:?}");
        let checks = item(&list, "Checks");
        assert_eq!(checks.mark, Mark::Unknown, "{checks:?}");
        assert!(!checks.blocks);
        assert_eq!(list.blocked(), None);
    }
}

#[test]
fn the_session_branch_passes() {
    let branch = item(
        &checklist(
            &ready_settings(),
            "s1700000000000",
            "Fix it",
            &[],
            None,
            None,
            None,
        ),
        "Branch",
    );
    assert_eq!(branch.mark, Mark::Pass);
    assert_eq!(
        branch.reason,
        "`ch/s1700000000000` is a name git accepts, and it is not the base branch."
    );
}

/// Each name below was run through `git check-ref-format --branch` (git 2.56). `@` passes there
/// only because git reads it as the current branch, which the runner refuses too.
#[test]
fn bad_branch_names_fail_before_git_sees_them() {
    for bad in [
        "",
        "-ch",
        "HEAD",
        "@",
        "ch/a b",
        "ch/a\tb",
        "ch/a\u{1}",
        "ch/a\u{7f}",
        "ch/a~1",
        "ch/a^",
        "ch/a:b",
        "ch/a?",
        "ch/a*",
        "ch/a[",
        "ch\\a",
        "ch/../main",
        "ch/a@{1}",
        "ch//a",
        "/ch",
        "ch/",
        "ch.",
        ".ch",
        "ch/.hidden",
        "ch/a.lock",
        "ch/a.lock/b",
    ] {
        let branch = branch_item(bad, "main");
        assert_eq!(branch.mark, Mark::Fail, "{bad:?}");
        assert!(branch.blocks, "{bad:?}");
    }
    for good in [
        "ch/s1",
        "c-harness/abc123",
        "feature/a.b",
        "ch/s1-retry",
        "a-b_c",
        "v1.2",
        "a@b",
        "ch/sửa",
    ] {
        assert_eq!(branch_item(good, "main").mark, Mark::Pass, "{good:?}");
    }
}

#[test]
fn a_branch_that_is_the_base_fails() {
    let branch = branch_item("ch/s1", " ch/s1 ");
    assert_eq!(branch.mark, Mark::Fail);
    assert!(branch.blocks);
    assert!(branch.reason.contains("base branch"), "{branch:?}");
}

#[test]
fn the_pull_request_needs_its_settings() {
    let list = checklist(&Settings::default(), "s1", "Fix it", &[], None, None, None);
    let pr = item(&list, "Pull request");
    assert_eq!(pr.mark, Mark::Fail);
    assert!(pr.blocks);
    assert!(
        pr.reason.starts_with("Finish the Landing settings first."),
        "{pr:?}"
    );
    assert!(pr.reason.contains("owner/name"), "{pr:?}");
}

#[test]
fn an_empty_title_fails() {
    let pr = pr_item(&ready_settings(), "  ");
    assert_eq!(pr.mark, Mark::Fail);
    assert!(pr.blocks);
    assert!(pr.reason.contains("no title"), "{pr:?}");
}

#[test]
fn a_ready_pull_request_names_where_it_goes() {
    let pr = pr_item(&ready_settings(), "Fix it");
    assert_eq!(pr.mark, Mark::Pass);
    assert_eq!(
        pr.reason,
        "Opens into `main` on example/demo, committed as Ada. The GitHub token is checked when \
         you press Land."
    );
}

#[test]
fn blockers_are_read_out_together() {
    let list = checklist(
        &Settings::default(),
        "s1",
        "Fix it",
        &[run(Some(1), Some("t1"), Some("t1"))],
        None,
        None,
        Some(RunnerBlock::Outdated),
    );
    let blocked = list.blocked().unwrap();
    assert!(blocked.contains("`cargo test` failed"), "{blocked}");
    assert!(blocked.contains("too old"), "{blocked}");
    assert!(blocked.contains("Landing settings"), "{blocked}");
}

#[test]
fn each_row_reads_as_name_word_and_reason() {
    let pr = pr_item(&ready_settings(), "Fix it");
    assert_eq!(
        pr.spoken(),
        "Pull request: Pass. Opens into `main` on example/demo, committed as Ada. The GitHub \
         token is checked when you press Land."
    );
    assert_eq!(Mark::Fail.word(), "Fail");
    assert_eq!(Mark::Unknown.word(), "Unknown");
}
