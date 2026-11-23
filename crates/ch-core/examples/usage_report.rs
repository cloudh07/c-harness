//! Prints the token ledger report for every session file in a usage folder.
//!
//! `cargo run -p ch-core --example usage_report -- <folder>`, where the folder is the app's
//! `c-harness/usage` under the local data directory (`%LOCALAPPDATA%` on Windows,
//! `~/Library/Application Support` on macOS, `~/.local/share` on Linux).

use std::io::ErrorKind;
use std::path::Path;
use std::process::ExitCode;
use std::{env, fs};

use ch_core::{SessionLedger, report};

fn main() -> ExitCode {
    let Some(folder) = env::args_os().nth(1) else {
        eprintln!("usage: usage_report <usage folder>");
        return ExitCode::FAILURE;
    };
    match sessions(Path::new(&folder)) {
        Ok(sessions) => {
            print!("{}", report(&sessions));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Could not read {}: {e}", Path::new(&folder).display());
            ExitCode::FAILURE
        }
    }
}

/// Each `<session>.jsonl` in the folder, by name. A folder that does not exist holds none.
fn sessions(folder: &Path) -> std::io::Result<Vec<(String, SessionLedger)>> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        sessions.push((name, SessionLedger::parse(&fs::read_to_string(&path)?)));
    }
    sessions.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(sessions)
}
