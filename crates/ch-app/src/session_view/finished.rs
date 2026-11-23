//! Telling the user a run finished while C-Harness was in the background, so they can come back
//! to review it.
//!
//! No notice when the user stopped the run or asked to land it (they are already here), when it
//! failed or never started (the session screen says why), or while the window has focus. A run
//! the cost cap ended is told like any other: nobody here stopped it.

use ch_core::SessionState;
use gpui_kit::{Context, SystemNotification};

use super::{Phase, SessionView};

/// What one session has seen of its current run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Run {
    /// It was running since it last settled.
    armed: bool,
    /// The user stopped this run, or asked to land, which stops it first.
    stopped: bool,
    /// A notice for an earlier run may still be on screen.
    shown: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Change {
    Nothing,
    /// The run ended on its own: say so, if the window is in the background.
    Ended,
    /// The session moved on, so the old notice is stale.
    TakeDown,
}

impl Run {
    /// Called before the driver reads new frames. A stop has to be caught here: once the end
    /// lands, the driver no longer says it is stopping.
    pub(super) fn before(self, state: SessionState, stopping: bool) -> Self {
        let armed = self.armed || state == SessionState::Running;
        Self {
            armed,
            stopped: armed && (self.stopped || stopping),
            shown: self.shown,
        }
    }

    /// Called after the driver read new frames.
    pub(super) fn after(self, state: SessionState) -> (Self, Change) {
        match state {
            SessionState::Running | SessionState::NeedsYou => {
                let armed = self.armed || state == SessionState::Running;
                let next = Self {
                    armed,
                    stopped: armed && self.stopped,
                    shown: false,
                };
                (
                    next,
                    if self.shown {
                        Change::TakeDown
                    } else {
                        Change::Nothing
                    },
                )
            }
            SessionState::NeedsReview | SessionState::Idle if self.armed => {
                let next = Self {
                    shown: self.shown,
                    ..Self::default()
                };
                (
                    next,
                    if self.stopped {
                        Change::Nothing
                    } else {
                        Change::Ended
                    },
                )
            }
            SessionState::NeedsReview | SessionState::Idle => (self, Change::Nothing),
            SessionState::Starting | SessionState::Failed | SessionState::Landed => {
                let change = if self.shown {
                    Change::TakeDown
                } else {
                    Change::Nothing
                };
                (Self::default(), change)
            }
        }
    }

    pub(super) fn noticed(self) -> Self {
        Self {
            shown: true,
            ..self
        }
    }

    pub(super) fn has_notice(self) -> bool {
        self.shown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tell {
    Nothing,
    Notice,
    NoticeAndSound,
}

/// The sound only comes with a notice, so a focused window stays quiet too.
pub(super) fn tell(notify: bool, sound: bool, window_active: bool) -> Tell {
    match (notify && !window_active, sound) {
        (false, _) => Tell::Nothing,
        (true, false) => Tell::Notice,
        (true, true) => Tell::NoticeAndSound,
    }
}

/// One per session, so the next run's notice replaces this one.
pub(super) fn tag(session: &str) -> String {
    format!("finished-{session}")
}

/// Nothing about the work: the prompt and the diff can hold paths, commands or code, and
/// notifications show on the lock screen.
pub(super) fn notification(session: &str) -> SystemNotification {
    SystemNotification {
        tag: tag(session).into(),
        title: "C-Harness finished".into(),
        body: "A session is ready for review.".into(),
        actions: Vec::new(),
    }
}

impl SessionView {
    /// Shows the notice when the live session's run ends, and takes it down once the session
    /// moves on.
    pub(super) fn track_finished(&mut self, cx: &mut Context<Self>) {
        let Phase::Live(live) = &mut self.phase else {
            return;
        };
        let (run, change) = live.finished.after(live.driver.session().state());
        live.finished = run;
        match change {
            Change::Nothing => {}
            Change::TakeDown => cx.dismiss_system_notification(&tag(&live.session)),
            Change::Ended => {
                let (notify, sound) = (self.settings.notify_finished, self.settings.finished_sound);
                // A window that is gone has nobody to tell.
                let told = self
                    .window
                    .update(cx, |_, window, _| {
                        let told = tell(notify, sound, window.is_window_active());
                        if told == Tell::NoticeAndSound {
                            window.play_system_bell();
                        }
                        told
                    })
                    .unwrap_or(Tell::Nothing);
                if told != Tell::Nothing {
                    cx.show_system_notification(notification(&live.session));
                    live.finished = live.finished.noticed();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ch_core::SessionState::{Failed, Idle, Landed, NeedsReview, NeedsYou, Running, Starting};

    /// Plays one tick per state: `stopping` is what the driver said before reading frames.
    fn play(mut run: Run, ticks: &[(SessionState, bool)]) -> (Run, Vec<Change>) {
        let mut seen = Running;
        let mut changes = Vec::new();
        for &(state, stopping) in ticks {
            run = run.before(seen, stopping);
            let (next, change) = run.after(state);
            run = next;
            changes.push(change);
            seen = state;
        }
        (run, changes)
    }

    fn ended(changes: &[Change]) -> usize {
        changes.iter().filter(|c| **c == Change::Ended).count()
    }

    #[test]
    fn a_run_that_settles_is_told_once() {
        let ticks = [
            (Running, false),
            (Running, false),
            (NeedsReview, false),
            (NeedsReview, false),
            (NeedsReview, false),
        ];
        let (run, changes) = play(Run::default(), &ticks);
        assert_eq!(changes[2], Change::Ended);
        assert_eq!(ended(&changes), 1);
        assert!(!run.armed);
    }

    #[test]
    fn a_whole_run_inside_one_tick_still_counts() {
        let run = Run::default().before(Running, false);
        assert_eq!(run.after(NeedsReview).1, Change::Ended);
    }

    #[test]
    fn a_run_the_user_stopped_is_not_told() {
        // The stop is seen before the end is read; on the tick the end lands, the driver no
        // longer says it is stopping.
        let ticks = [(Running, false), (Running, true), (NeedsReview, false)];
        let (_, changes) = play(Run::default(), &ticks);
        assert_eq!(ended(&changes), 0);
    }

    #[test]
    fn a_stop_does_not_carry_over_to_the_next_run() {
        let ticks = [
            (Running, true),
            (NeedsReview, false),
            (Running, false),
            (NeedsReview, false),
        ];
        let (_, changes) = play(Run::default(), &ticks);
        assert_eq!(changes[1], Change::Nothing);
        assert_eq!(changes[3], Change::Ended);
    }

    #[test]
    fn a_failed_start_or_a_crash_is_not_told() {
        let start = Run::default().before(Starting, false);
        assert_eq!(start.after(Failed), (Run::default(), Change::Nothing));
        assert_eq!(start.after(Idle), (Run::default(), Change::Nothing));
        let ticks = [(Running, false), (Failed, false)];
        let (run, changes) = play(Run::default(), &ticks);
        assert_eq!(ended(&changes), 0);
        assert_eq!(run, Run::default());
    }

    #[test]
    fn waiting_on_an_approval_keeps_the_run() {
        let ticks = [
            (Running, false),
            (NeedsYou, false),
            (Running, false),
            (NeedsReview, false),
        ];
        let (_, changes) = play(Run::default(), &ticks);
        assert_eq!(changes[3], Change::Ended);
        assert_eq!(ended(&changes), 1);
    }

    #[test]
    fn the_next_run_or_a_land_takes_the_old_notice_down() {
        let told = Run::default().noticed();
        assert!(told.has_notice());
        let (run, change) = told.before(NeedsReview, false).after(Running);
        assert_eq!(change, Change::TakeDown);
        assert!(!run.has_notice() && run.armed);
        assert_eq!(told.after(Landed), (Run::default(), Change::TakeDown));
        assert_eq!(told.after(NeedsReview), (told, Change::Nothing));
    }

    #[test]
    fn only_a_background_window_hears_about_it() {
        assert_eq!(tell(true, false, false), Tell::Notice);
        assert_eq!(tell(true, true, false), Tell::NoticeAndSound);
        assert_eq!(tell(true, true, true), Tell::Nothing);
        assert_eq!(tell(false, true, false), Tell::Nothing);
        assert_eq!(tell(false, false, true), Tell::Nothing);
    }

    #[test]
    fn the_notice_keeps_the_work_to_itself() {
        let shown = notification("s1");
        assert_eq!(shown.tag.as_ref(), tag("s1"));
        assert_ne!(tag("s1"), tag("s2"));
        assert_ne!(tag("s1"), super::super::waiting::tag("s1"));
        assert!(!shown.title.contains("s1") && !shown.body.contains("s1"));
        assert_eq!(shown.actions, Vec::new());
    }

    #[test]
    fn notices_are_on_and_sound_is_off_until_changed() {
        let old_file: crate::settings::Settings = serde_json::from_str("{}").unwrap();
        assert!(old_file.notify_finished);
        assert!(!old_file.finished_sound);
    }
}
