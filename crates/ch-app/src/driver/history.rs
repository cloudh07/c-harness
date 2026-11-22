//! Resuming a session: omp starts again on its saved conversation, and the driver reads that
//! conversation back a page at a time so the window can show it.

use std::collections::{BTreeMap, HashSet};

use ch_core::{History, SessionState, Usage, UsageRow, history_request, read_page, session_file};
use serde_json::Value;

use super::{Driver, OmpState};

/// How long omp has to answer one page request.
const PAGE_TIMEOUT_MS: u64 = 30_000;
/// Pages come oldest first, so a cap keeps the oldest. It sits far above any real session and
/// only stops a cursor that never ends.
const MAX_HISTORY_PAGES: usize = 1024;
const PARTIAL: &str = "Only part of the earlier conversation was loaded.";

/// The earlier conversation, as (prompt, reply) pairs for the screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EarlierTurns {
    pub turns: Vec<(String, String)>,
    /// Older turns left out to keep the screen short.
    pub hidden: usize,
    /// Why some or all of the conversation is missing.
    pub problem: Option<String>,
}

#[derive(Default)]
pub(super) enum Earlier {
    #[default]
    Off,
    Loading(Load),
    Done(EarlierTurns),
}

#[derive(Default)]
pub(super) struct Load {
    history: History,
    /// The id of the page request omp has not answered, and when it went out.
    waiting: Option<(String, u64)>,
    cursor: Option<String>,
    seen: HashSet<String>,
    pages: usize,
}

/// What the load should do next.
enum Step {
    Wait,
    Send(Option<String>),
    Stop(&'static str),
}

impl Driver {
    /// Starts omp again on a session an earlier run left open. `extra_args` must point omp at
    /// the saved conversation; `rows` is the usage ledger so far, so turns number on.
    /// The conversation is read back once omp is ready, and no prompt goes out until then.
    pub fn resume(
        &mut self,
        session: &str,
        extra_args: Vec<String>,
        env: BTreeMap<String, String>,
        rows: Vec<UsageRow>,
    ) -> bool {
        self.usage = Usage::with_rows(rows);
        let sent = self.start(session, extra_args, env, String::new());
        if sent {
            self.earlier = Earlier::Loading(Load::default());
        }
        sent
    }

    /// The earlier conversation is still being read.
    pub fn loading_history(&self) -> bool {
        matches!(self.earlier, Earlier::Loading(_))
    }

    /// The earlier conversation, once, when it has been read.
    pub fn take_history(&mut self) -> Option<EarlierTurns> {
        if !matches!(self.earlier, Earlier::Done(_)) {
            return None;
        }
        match std::mem::take(&mut self.earlier) {
            Earlier::Done(turns) => Some(turns),
            _ => None,
        }
    }

    /// The file omp keeps this conversation in, as its last stats answer named it.
    pub fn omp_session_file(&self) -> Option<&str> {
        self.omp_session_file.as_deref()
    }

    /// Reads what an omp frame says about the resumed conversation.
    pub(super) fn on_resume_frame(&mut self, frame: &Value) {
        if let Some(file) = session_file(frame) {
            self.omp_session_file = Some(file.to_owned());
        }
        let Earlier::Loading(load) = &mut self.earlier else {
            return;
        };
        let Some((id, _)) = &load.waiting else {
            return;
        };
        let answers = frame.get("type").and_then(Value::as_str) == Some("response")
            && frame.get("id").and_then(Value::as_str) == Some(id.as_str());
        if !answers {
            return;
        }
        load.waiting = None;
        let problem = match read_page(frame) {
            Err(err) => format!("Could not load the earlier conversation: {err}"),
            Ok(page) => {
                for message in &page.messages {
                    load.history.add(message);
                }
                load.pages += 1;
                match page.next_cursor {
                    None => return self.finish_history(None),
                    Some(cursor)
                        if load.pages < MAX_HISTORY_PAGES && load.seen.insert(cursor.clone()) =>
                    {
                        load.cursor = Some(cursor);
                        return;
                    }
                    Some(_) => PARTIAL.to_string(),
                }
            }
        };
        self.finish_history(Some(problem));
    }

    /// Sends the next page request when omp is free for it, and gives up on one it took too
    /// long to answer or that can no longer come.
    pub(super) fn page_history(&mut self) {
        match self.history_step() {
            Step::Wait => {}
            Step::Stop(problem) => self.finish_history(Some(problem.to_string())),
            Step::Send(cursor) => {
                let id = self.next_id();
                if !self.send_omp(&history_request(&id, cursor.as_deref()), None) {
                    return self.finish_history(Some(
                        "The earlier conversation could not be asked for.".to_string(),
                    ));
                }
                let now = (self.clock)();
                if let Earlier::Loading(load) = &mut self.earlier {
                    load.waiting = Some((id, now));
                }
            }
        }
    }

    fn history_step(&self) -> Step {
        let Earlier::Loading(load) = &self.earlier else {
            return Step::Wait;
        };
        if self.closed {
            return Step::Stop("The runner closed before the earlier conversation was loaded.");
        }
        if self.omp != OmpState::Running {
            return Step::Stop("omp stopped before the earlier conversation was loaded.");
        }
        if let Some((_, sent)) = &load.waiting {
            return if (self.clock)().saturating_sub(*sent) >= PAGE_TIMEOUT_MS {
                Step::Stop("omp did not send the earlier conversation in time.")
            } else {
                Step::Wait
            };
        }
        match self.session.state() {
            SessionState::Starting => Step::Wait,
            SessionState::Idle | SessionState::NeedsReview => Step::Send(load.cursor.clone()),
            _ => Step::Stop("The earlier conversation was not loaded."),
        }
    }

    fn finish_history(&mut self, problem: Option<String>) {
        if !matches!(self.earlier, Earlier::Loading(_)) {
            return;
        }
        if let Earlier::Loading(load) = std::mem::take(&mut self.earlier) {
            let hidden = load.history.dropped();
            self.earlier = Earlier::Done(EarlierTurns {
                turns: load.history.into_turns(),
                hidden,
                problem,
            });
        }
    }
}
