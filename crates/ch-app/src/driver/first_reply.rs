//! The deadline for the model's first word, timed with the app's clock.
//!
//! omp has no event for "connected to the provider", so the app cannot tell a slow connection
//! from a slow model. It times both as one wait, from the prompt (or after omp's retry delay)
//! to the first text, thinking or tool call.

use ch_core::ReplyWait;

/// How long the model may stay silent before the session says so.
pub const FIRST_REPLY_MS: u64 = 90_000;

#[derive(Default)]
pub(super) struct FirstReply {
    wait: Option<ReplyWait>,
    due: u64,
    late: bool,
}

impl FirstReply {
    /// Follows the session's current wait at `now`. Returns whether [`Self::late`] changed.
    pub(super) fn update(&mut self, now: u64, wait: Option<ReplyWait>) -> bool {
        if wait != self.wait {
            self.wait = wait;
            self.due = wait.map_or(0, |w| {
                now.saturating_add(w.delay_ms)
                    .saturating_add(FIRST_REPLY_MS)
            });
        }
        let late = self.wait.is_some() && now >= self.due;
        let changed = late != self.late;
        self.late = late;
        changed
    }

    /// The deadline passed and the model has still said nothing.
    pub(super) fn late(&self) -> bool {
        self.late
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(epoch: u32, delay_ms: u64) -> ReplyWait {
        ReplyWait { epoch, delay_ms }
    }

    #[test]
    fn late_once_the_deadline_passes_and_not_before() {
        let mut timer = FirstReply::default();
        assert!(!timer.update(1_000, Some(wait(1, 0))));
        assert!(!timer.update(1_000 + FIRST_REPLY_MS - 1, Some(wait(1, 0))));
        assert!(timer.update(1_000 + FIRST_REPLY_MS, Some(wait(1, 0))));
        assert!(timer.late());
        assert!(
            !timer.update(5_000_000, Some(wait(1, 0))),
            "no change to report"
        );
    }

    #[test]
    fn a_word_from_the_model_ends_the_wait() {
        let mut timer = FirstReply::default();
        timer.update(0, Some(wait(1, 0)));
        timer.update(FIRST_REPLY_MS, Some(wait(1, 0)));
        assert!(timer.update(FIRST_REPLY_MS + 1, None));
        assert!(!timer.late());
    }

    #[test]
    fn a_new_wait_starts_a_new_deadline_after_omps_delay() {
        let mut timer = FirstReply::default();
        timer.update(0, Some(wait(1, 0)));
        timer.update(FIRST_REPLY_MS, Some(wait(1, 0)));
        assert!(timer.late());

        let start = FIRST_REPLY_MS + 10;
        assert!(
            timer.update(start, Some(wait(2, 4_000))),
            "a retry is not late yet"
        );
        assert!(!timer.update(start + FIRST_REPLY_MS + 3_999, Some(wait(2, 4_000))));
        assert!(timer.update(start + FIRST_REPLY_MS + 4_000, Some(wait(2, 4_000))));
    }

    #[test]
    fn a_paused_wait_starts_over_when_it_comes_back() {
        let mut timer = FirstReply::default();
        timer.update(0, Some(wait(1, 0)));
        timer.update(FIRST_REPLY_MS - 1, None);
        assert!(!timer.update(FIRST_REPLY_MS, Some(wait(1, 0))));
        assert!(!timer.update(2 * FIRST_REPLY_MS - 1, Some(wait(1, 0))));
        assert!(timer.update(2 * FIRST_REPLY_MS, Some(wait(1, 0))));
    }
}
