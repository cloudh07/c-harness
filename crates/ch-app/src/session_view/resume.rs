//! Resuming a session an earlier run left open: the runner starts again in its worktree and the
//! earlier conversation comes back onto the screen.

use std::path::Path;
use std::sync::mpsc::TryRecvError;

use ch_runtime::LeftoverKind;
use gpui_kit::Context;

use super::{Live, Phase, SessionView};
use crate::driver::{self, Driver, EarlierTurns};
use crate::launch::{self, Launched, Resumed};
use crate::ledger::Ledger;
use crate::settings::{self, Location};

/// Only a leftover whose worktree is still there, run on this computer, can start again.
pub(super) fn can_resume(kind: LeftoverKind, location: &Location) -> bool {
    kind == LeftoverKind::Live && matches!(location, Location::Local)
}

/// The first prompt of the earlier conversation that has any text.
fn earliest_prompt(turns: &[(String, String)]) -> Option<String> {
    turns
        .iter()
        .map(|(prompt, _)| prompt)
        .find(|prompt| !prompt.is_empty())
        .cloned()
}

impl SessionView {
    pub(super) fn resume_leftover(&mut self, session: &str, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Empty) {
            self.message = None;
            self.phase = Phase::Resuming {
                session: session.to_string(),
                rx: launch::resume(self.settings.clone(), session.to_string()),
            };
        }
        cx.notify();
    }

    pub(super) fn poll_resume(&mut self) -> bool {
        let Phase::Resuming { rx, .. } = &self.phase else {
            return false;
        };
        let result = match rx.try_recv() {
            Err(TryRecvError::Empty) => return false,
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => {
                Err("The resume stopped without saying why.".to_string())
            }
        };
        let Phase::Resuming { session, .. } = std::mem::replace(&mut self.phase, Phase::Empty)
        else {
            return false;
        };
        match result.and_then(|resumed| go_live_resumed(&session, resumed)) {
            Ok(live) => {
                self.leftovers.keep(&session);
                self.phase = Phase::Live(Box::new(live));
            }
            Err(message) => self.leftovers.failed(&session, message),
        }
        true
    }
}

fn go_live_resumed(session: &str, resumed: Resumed) -> Result<Live, String> {
    let Resumed {
        launched:
            Launched {
                link,
                runner,
                runner_version,
                env,
            },
        omp_args,
    } = resumed;
    let (ledger, rows) = Ledger::resumed(settings::usage_dir().as_deref(), session);
    let mut driver = Driver::new(link, driver::epoch_ms);
    if !driver.resume(session, omp_args, env, rows) {
        runner.retire(false);
        return Err("The runner exited before the session could start again.".to_string());
    }
    Ok(Live {
        worktree: runner.worktree(),
        ledger,
        session: session.to_string(),
        driver,
        runner: Some(runner),
        version: runner_version,
        first_prompt: String::new(),
        turns: Vec::new(),
        prompt: String::new(),
        hidden: 0,
        noted_file: None,
    })
}

impl Live {
    /// Puts the earlier conversation before this run's turns once it has been read, and saves
    /// where omp keeps the conversation. Returns whether anything changed, and any problem.
    pub(super) fn catch_up(&mut self) -> (bool, Option<String>) {
        let noted = self.note_session_file();
        let Some(earlier) = self.driver.take_history() else {
            return (false, noted);
        };
        let EarlierTurns {
            turns,
            hidden,
            problem,
        } = earlier;
        if self.first_prompt.is_empty()
            && let Some(prompt) = earliest_prompt(&turns)
        {
            self.first_prompt = prompt;
        }
        self.hidden = hidden;
        self.turns.splice(0..0, turns);
        let problems: Vec<String> = [problem, noted].into_iter().flatten().collect();
        (true, (!problems.is_empty()).then(|| problems.join(" ")))
    }

    /// Saves the omp session file to the record the first time omp names it, or when it changes.
    fn note_session_file(&mut self) -> Option<String> {
        let file = self.driver.omp_session_file()?;
        if self.noted_file.as_deref() == Some(file) {
            return None;
        }
        let file = file.to_string();
        let result = match &self.runner {
            Some(runner) => runner.note_omp_session_file(Path::new(&file)),
            None => Ok(false),
        };
        self.noted_file = Some(file);
        result.err().map(|e| {
            format!(
                "Could not save where omp keeps this conversation, so a resume may start fresh: \
                 {e}"
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(prompt: &str, reply: &str) -> (String, String) {
        (prompt.to_string(), reply.to_string())
    }

    #[test]
    fn only_a_live_local_leftover_resumes() {
        assert!(can_resume(LeftoverKind::Live, &Location::Local));
        let ssh = Location::Ssh {
            host: "box".to_string(),
            port: None,
            remote_workdir: "/w".to_string(),
            remote_runner: "ch-runner".to_string(),
        };
        assert!(!can_resume(LeftoverKind::Live, &ssh));
        for kind in [
            LeftoverKind::FolderGone,
            LeftoverKind::NoWorktree,
            LeftoverKind::RepoUnreadable,
            LeftoverKind::NoRecord,
            LeftoverKind::Damaged,
        ] {
            assert!(!can_resume(kind, &Location::Local), "{kind:?}");
        }
    }

    #[test]
    fn the_title_comes_from_the_first_prompt_with_text() {
        assert_eq!(
            earliest_prompt(&[pair("", "Hello."), pair("Fix it", ""), pair("Thanks", "")]),
            Some("Fix it".to_string())
        );
        assert_eq!(earliest_prompt(&[pair("", "Hello.")]), None);
        assert_eq!(earliest_prompt(&[]), None);
    }
}
