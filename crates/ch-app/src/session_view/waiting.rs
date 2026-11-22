//! Telling the user a session needs them: an OS notification while it waits, and a mark on its
//! record so the next start can say it was waiting.

use ch_core::SessionState;
use gpui_kit::{Context, SystemNotification};

use super::{Phase, SessionView};

/// The new waiting value when `state` crosses into or out of needing an answer.
pub(super) fn edge(was_waiting: bool, state: SessionState) -> Option<bool> {
    let waiting = state == SessionState::NeedsYou;
    (waiting != was_waiting).then_some(waiting)
}

/// One notification per session, so a later one replaces it and leaving `NeedsYou` removes it.
pub(super) fn tag(session: &str) -> String {
    format!("needs-you-{session}")
}

/// Says nothing of the request itself: it can hold paths or commands, and notifications show
/// on the lock screen.
pub(super) fn notification(session: &str) -> SystemNotification {
    SystemNotification {
        tag: tag(session).into(),
        title: "C-Harness needs you".into(),
        body: "A session is waiting for your answer.".into(),
        actions: Vec::new(),
    }
}

impl SessionView {
    /// Shows or takes down the notification when the live session starts or stops waiting, and
    /// saves the change to its record. Returns whether anything changed.
    pub(super) fn track_waiting(&mut self, cx: &mut Context<Self>) -> bool {
        let Phase::Live(live) = &mut self.phase else {
            return false;
        };
        let Some(waiting) = edge(live.waiting, live.driver.session().state()) else {
            return false;
        };
        // Set before the write, so a record that cannot be written is not retried every tick.
        live.waiting = waiting;
        if waiting {
            cx.show_system_notification(notification(&live.session));
        } else {
            cx.dismiss_system_notification(&tag(&live.session));
        }
        let saved = match &live.runner {
            Some(runner) => runner.note_needs_you(waiting),
            None => Ok(false),
        };
        if let Err(e) = saved {
            self.message = Some(Err(format!(
                "Could not note on the session record that it is waiting, so a restart will not \
                 say so: {e}"
            )));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_crossing_into_or_out_of_needs_you_counts() {
        assert_eq!(edge(false, SessionState::NeedsYou), Some(true));
        assert_eq!(edge(true, SessionState::Running), Some(false));
        assert_eq!(edge(true, SessionState::Failed), Some(false));
        assert_eq!(edge(true, SessionState::NeedsYou), None);
        assert_eq!(edge(false, SessionState::Running), None);
        assert_eq!(edge(false, SessionState::Starting), None);
    }

    #[test]
    fn a_resumed_session_marked_waiting_clears_the_mark_once_it_runs() {
        // A resume seeds `waiting` from the record; omp starts with nothing to answer.
        assert_eq!(edge(true, SessionState::Starting), Some(false));
    }

    #[test]
    fn the_notification_keeps_the_request_to_itself() {
        let shown = notification("s1");
        assert_eq!(shown.tag.as_ref(), tag("s1"));
        assert_ne!(tag("s1"), tag("s2"));
        assert!(!shown.title.contains("s1") && !shown.body.contains("s1"));
        assert_eq!(shown.actions, Vec::new());
    }
}
