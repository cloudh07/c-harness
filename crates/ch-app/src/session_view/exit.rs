//! What the live session does on its way out, whether the user closes it, closes the window or
//! the OS quits the app: it saves the usage lines a failed write left and takes down its
//! notification. omp stops with the app, so the request goes too, and the OS would otherwise
//! keep showing it.

use ch_core::UsageRow;
use gpui_kit::{App, Context, Subscription};

use super::{Live, Phase, SessionView, waiting};
use crate::ledger::Ledger;

/// Closing the window releases the view before the app shuts down, so a quit handler alone never
/// sees it. A quit from the OS runs the quit handlers while the window is still open, and may end
/// the process before anything is released. Whichever comes first does the work; the other finds
/// nothing left to do.
pub(super) fn save_on_exit(cx: &mut Context<SessionView>) -> [Subscription; 2] {
    [
        cx.on_app_quit(|view, cx| {
            view.leave_on_exit(cx);
            async {}
        }),
        cx.on_release(SessionView::leave_on_exit),
    ]
}

impl SessionView {
    fn leave_on_exit(&mut self, cx: &mut App) {
        let Phase::Live(live) = &mut self.phase else {
            return;
        };
        if let Some(problem) = live.leave(cx) {
            // The window is gone, so the terminal is the only place left to say it.
            eprintln!("c-harness: {problem}");
        }
    }
}

impl Live {
    /// Saves the usage lines still unsaved and takes down the notification, if any. Returns why
    /// the lines could not be saved. Calling it again does nothing new.
    pub(super) fn leave(&mut self, cx: &mut App) -> Option<String> {
        let left = leave(
            &mut self.waiting,
            &self.session,
            &mut self.ledger,
            self.driver.usage().rows(),
        );
        if let Some(tag) = left.dismiss {
            cx.dismiss_system_notification(&tag);
        }
        left.problem
    }
}

/// What leaving asks of the app.
#[derive(Debug, PartialEq, Eq)]
struct Leaving {
    /// The tag of the notification still on screen.
    dismiss: Option<String>,
    /// Why the last usage lines could not be saved.
    problem: Option<String>,
}

fn leave(waiting: &mut bool, session: &str, ledger: &mut Ledger, rows: &[UsageRow]) -> Leaving {
    Leaving {
        dismiss: std::mem::take(waiting).then(|| waiting::tag(session)),
        problem: ledger.flush(rows),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::json;

    use super::*;

    fn row(turn: u32) -> UsageRow {
        UsageRow::from_turn_end(turn, u64::from(turn) * 10, &json!({}))
    }

    fn turns(path: &Path) -> Vec<u32> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<UsageRow>(line).unwrap().turn)
            .collect()
    }

    /// A failed write is only tried again after the next turn, and there is none after a quit.
    #[test]
    fn leaving_saves_the_lines_a_failed_write_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        // A folder where the file should be makes the write fail.
        fs::create_dir(&path).unwrap();
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        let rows = [row(1)];
        assert!(ledger.sync(&rows).is_some());
        fs::remove_dir(&path).unwrap();

        let mut waiting = true;
        let left = leave(&mut waiting, "s1", &mut ledger, &rows);

        assert_eq!(
            left,
            Leaving {
                dismiss: Some(waiting::tag("s1")),
                problem: None,
            }
        );
        assert!(!waiting);
        assert_eq!(turns(&path), [1]);
    }

    /// A quit and the window closing can both reach the session; the second finds nothing left.
    #[test]
    fn leaving_twice_saves_and_dismisses_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        let rows = [row(1), row(2)];
        let mut waiting = true;

        let first = leave(&mut waiting, "s1", &mut ledger, &rows);
        let second = leave(&mut waiting, "s1", &mut ledger, &rows);

        assert_eq!(first.dismiss, Some(waiting::tag("s1")));
        assert_eq!(
            second,
            Leaving {
                dismiss: None,
                problem: None,
            }
        );
        assert_eq!(turns(&path), [1, 2]);
    }

    #[test]
    fn a_session_that_is_not_waiting_has_nothing_to_dismiss() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::new(Some(dir.path()), "s1");
        let mut waiting = false;
        let left = leave(&mut waiting, "s1", &mut ledger, &[]);
        assert_eq!(left.dismiss, None);
    }
}
