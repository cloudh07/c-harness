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
use crate::view_model::Place;

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

/// The earlier conversation, then this run's turns, and the title: this run's first prompt, or
/// the earlier conversation's when this run has none yet.
fn after_earlier(
    earlier: Vec<(String, String)>,
    current: Vec<(String, String)>,
    title: String,
) -> (Vec<(String, String)>, String) {
    let title = match earliest_prompt(&earlier) {
        Some(prompt) if title.is_empty() => prompt,
        _ => title,
    };
    let mut turns = earlier;
    turns.extend(current);
    (turns, title)
}

impl SessionView {
    pub(super) fn resume_leftover(&mut self, session: &str, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Empty) {
            self.message = None;
            self.phase = Phase::Resuming {
                session: session.to_string(),
                place: Place::of(&self.settings),
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
        let Phase::Resuming { session, place, .. } =
            std::mem::replace(&mut self.phase, Phase::Empty)
        else {
            return false;
        };
        match result.and_then(|resumed| go_live_resumed(&session, place, resumed)) {
            Ok(live) => {
                self.leftovers.keep(&session);
                self.phase = Phase::Live(Box::new(live));
            }
            Err(message) => self.leftovers.failed(&session, message),
        }
        true
    }
}

fn go_live_resumed(session: &str, place: Place, resumed: Resumed) -> Result<Live, String> {
    let Resumed {
        launched:
            Launched {
                link,
                runner,
                runner_version,
                env,
            },
        omp_args,
        needs_you,
    } = resumed;
    let (ledger, rows) = Ledger::resumed(settings::usage_dir().as_deref(), session);
    let mut driver = Driver::new(link, driver::epoch_ms);
    if !driver.resume(session, omp_args, env, rows) {
        runner.retire(false);
        return Err("The runner exited before the session could start again.".to_string());
    }
    Ok(Live {
        place,
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
        waiting: needs_you,
    })
}

impl Live {
    /// Puts the earlier conversation before this run's turns once it has been read, and saves
    /// where omp keeps the conversation. After a restore the conversation read back replaces
    /// the thread instead, and a reload that failed leaves it as it was. Returns whether
    /// anything changed, and any problem.
    pub(super) fn catch_up(&mut self) -> (bool, Option<String>) {
        let noted = self.note_session_file();
        let Some(earlier) = self.driver.take_history() else {
            return (false, noted);
        };
        let EarlierTurns {
            turns,
            hidden,
            problem,
            replaces,
        } = earlier;
        if !replaces || problem.is_none() {
            let current = if replaces {
                self.prompt.clear();
                Vec::new()
            } else {
                std::mem::take(&mut self.turns)
            };
            (self.turns, self.first_prompt) =
                after_earlier(turns, current, std::mem::take(&mut self.first_prompt));
            self.hidden = hidden;
        }
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

    #[test]
    fn the_earlier_conversation_goes_before_this_runs_turns() {
        let earlier = vec![pair("", "Hello."), pair("Fix it", "Fixed.")];
        let current = vec![pair("Now test it", "Tested.")];
        let (turns, title) = after_earlier(earlier.clone(), current.clone(), String::new());
        assert_eq!(
            turns,
            [earlier[0].clone(), earlier[1].clone(), current[0].clone()]
        );
        assert_eq!(title, "Fix it");

        let (_, title) = after_earlier(earlier, current, "Now test it".to_string());
        assert_eq!(title, "Now test it", "this run already has a title");
        let (turns, title) = after_earlier(Vec::new(), Vec::new(), String::new());
        assert_eq!((turns.len(), title.as_str()), (0, ""));
    }
}
