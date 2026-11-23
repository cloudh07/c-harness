//! Guesses the test command from the project files at the top of the working directory.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Project files bigger than this are skipped rather than read.
const MAX_PROJECT_FILE: u64 = 1024 * 1024;

/// Test commands the project files point to, in a fixed order. Empty when none match.
pub fn verify_commands(workdir: &Path) -> Vec<&'static str> {
    detect(
        |name| read_small(&workdir.join(name)),
        |name| workdir.join(name).is_file(),
    )
}

fn detect(
    read: impl Fn(&str) -> Option<String>,
    exists: impl Fn(&str) -> bool,
) -> Vec<&'static str> {
    let mut found = Vec::new();
    if exists("Cargo.toml") {
        found.push("cargo test");
    }
    if read("package.json").is_some_and(|text| has_test_script(&text)) {
        found.push(if exists("pnpm-lock.yaml") {
            "pnpm test"
        } else if exists("yarn.lock") {
            "yarn test"
        } else {
            "npm test"
        });
    }
    if exists("go.mod") {
        found.push("go test ./...");
    }
    if read("pyproject.toml").is_some_and(|text| configures_pytest(&text)) {
        found.push("pytest");
    }
    found
}

/// `npm init` writes a test script that only fails, so it does not count.
fn has_test_script(package_json: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(package_json) else {
        return false;
    };
    value["scripts"]["test"]
        .as_str()
        .is_some_and(|script| !script.trim().is_empty() && !script.contains("no test specified"))
}

fn configures_pytest(pyproject: &str) -> bool {
    pyproject
        .lines()
        .any(|line| line.trim_start().starts_with("[tool.pytest"))
}

fn read_small(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_PROJECT_FILE {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_PROJECT_FILE).read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn detect_in(files: &[(&str, &str)]) -> Vec<&'static str> {
        let files: BTreeMap<&str, &str> = files.iter().copied().collect();
        detect(
            |name| files.get(name).map(|text| (*text).to_owned()),
            |name| files.contains_key(name),
        )
    }

    #[test]
    fn cargo_project_runs_cargo_test() {
        assert_eq!(detect_in(&[("Cargo.toml", "[package]")]), ["cargo test"]);
    }

    #[test]
    fn package_json_with_a_test_script_defaults_to_npm() {
        let package = r#"{"scripts":{"test":"vitest run"}}"#;
        assert_eq!(detect_in(&[("package.json", package)]), ["npm test"]);
    }

    #[test]
    fn lockfile_picks_the_package_manager() {
        let package = r#"{"scripts":{"test":"jest"}}"#;
        assert_eq!(
            detect_in(&[("package.json", package), ("pnpm-lock.yaml", "")]),
            ["pnpm test"]
        );
        assert_eq!(
            detect_in(&[("package.json", package), ("yarn.lock", "")]),
            ["yarn test"]
        );
    }

    #[test]
    fn package_json_without_a_real_test_script_is_skipped() {
        for package in [
            r#"{"name":"x"}"#,
            r#"{"scripts":{"build":"tsc"}}"#,
            r#"{"scripts":{"test":""}}"#,
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
            r#"{"scripts":{"test":42}}"#,
            "not json",
        ] {
            assert!(
                detect_in(&[("package.json", package)]).is_empty(),
                "{package}"
            );
        }
    }

    #[test]
    fn go_module_runs_every_package() {
        assert_eq!(detect_in(&[("go.mod", "module x")]), ["go test ./..."]);
    }

    #[test]
    fn pyproject_counts_only_with_pytest_configured() {
        let configured = "[project]\nname = \"x\"\n\n[tool.pytest.ini_options]\naddopts = \"-q\"\n";
        assert_eq!(detect_in(&[("pyproject.toml", configured)]), ["pytest"]);
        let plain = "[project]\nname = \"x\"\n[tool.ruff]\n";
        assert_eq!(detect_in(&[("pyproject.toml", plain)]), Vec::<&str>::new());
    }

    #[test]
    fn nothing_matches_an_empty_folder() {
        assert_eq!(detect_in(&[("README.md", "# x")]), Vec::<&str>::new());
    }

    #[test]
    fn several_matches_keep_a_fixed_order() {
        let found = detect_in(&[
            ("pyproject.toml", "[tool.pytest]\n"),
            ("go.mod", "module x"),
            ("package.json", r#"{"scripts":{"test":"jest"}}"#),
            ("Cargo.toml", "[workspace]"),
        ]);
        assert_eq!(found, ["cargo test", "npm test", "go test ./...", "pytest"]);
    }

    #[test]
    fn every_command_is_one_the_app_accepts() {
        let package = ("package.json", r#"{"scripts":{"test":"jest"}}"#);
        let mut seen = std::collections::BTreeSet::new();
        for lockfile in [None, Some("pnpm-lock.yaml"), Some("yarn.lock")] {
            let mut files = vec![
                ("Cargo.toml", ""),
                package,
                ("go.mod", ""),
                ("pyproject.toml", "[tool.pytest]"),
            ];
            files.extend(lockfile.map(|name| (name, "")));
            seen.extend(detect_in(&files));
        }
        let allowed = ch_protocol::preflight::VERIFY_COMMANDS;
        assert_eq!(seen, allowed.into_iter().collect());
    }

    #[test]
    fn reads_the_real_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"jest"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();
        // A folder named like a project file is not one.
        std::fs::create_dir(dir.path().join("go.mod")).unwrap();
        assert_eq!(verify_commands(dir.path()), ["cargo test", "yarn test"]);
    }

    #[test]
    fn oversized_project_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("package.json");
        let padding = " ".repeat(usize::try_from(MAX_PROJECT_FILE).unwrap());
        std::fs::write(
            &path,
            format!(r#"{{"scripts":{{"test":"jest"}}}}{padding}"#),
        )
        .unwrap();
        assert_eq!(read_small(&path), None);
        assert_eq!(verify_commands(dir.path()), Vec::<&str>::new());
    }
}
