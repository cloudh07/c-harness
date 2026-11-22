//! Short labels for the header and the lists: a live session's title, how full its context is,
//! and how long ago something happened.

use ch_core::{ContextReading, ContextState};

use super::session_title;

/// A live session whose first prompt is not known yet: a resumed one before its history loads,
/// or one whose history held no prompt.
const RESUMED_TITLE: &str = "Resumed session";

/// From this share of the window up, the context reading is drawn as a warning.
pub const CONTEXT_WARN_PERCENT: u64 = 80;

pub fn live_title(first_prompt: &str) -> String {
    let title = session_title(first_prompt);
    if title.is_empty() {
        RESUMED_TITLE.to_string()
    } else {
        title
    }
}

/// `tokens` as a share of `window`, rounded to the nearest percent, and 0 for a window of 0.
pub(super) fn context_percent(tokens: u64, window: u64) -> u64 {
    tokens
        .saturating_mul(100)
        .saturating_add(window / 2)
        .checked_div(window)
        .unwrap_or(0)
}

/// The window is known and at least `CONTEXT_WARN_PERCENT` full.
pub fn context_near_full(state: ContextState) -> bool {
    matches!(
        state,
        ContextState::Known(ContextReading { tokens, window: Some(window) })
            if context_percent(tokens, window) >= CONTEXT_WARN_PERCENT
    )
}

/// How long ago `then` was, both in seconds since the Unix epoch. A time ahead of `now`, from
/// a runner whose clock runs fast, reads as just now.
pub fn age_label(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => ago(secs / 3_600, "hour"),
        _ => ago(secs / 86_400, "day"),
    }
}

fn ago(count: u64, unit: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} ago")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60;
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;

    #[test]
    fn a_live_session_without_a_prompt_is_called_resumed() {
        assert_eq!(live_title(""), "Resumed session");
        assert_eq!(live_title("  \n"), "Resumed session");
        assert_eq!(live_title("Fix the login\nmore"), "Fix the login");
    }

    #[test]
    fn the_context_warns_from_80_percent() {
        let known = |tokens| {
            ContextState::Known(ContextReading {
                tokens,
                window: Some(200_000),
            })
        };
        assert!(!context_near_full(known(158_999)), "79.5% rounds to 79");
        assert!(context_near_full(known(159_000)));
        assert!(context_near_full(known(210_000)), "over the window");
        let no_window = ContextState::Known(ContextReading {
            tokens: 1_000_000,
            window: None,
        });
        let zero_window = ContextState::Known(ContextReading {
            tokens: 5,
            window: Some(0),
        });
        for state in [
            no_window,
            zero_window,
            ContextState::NotRead,
            ContextState::Unknown,
            ContextState::Compacting,
        ] {
            assert!(!context_near_full(state), "{state:?}");
        }
    }

    #[test]
    fn ages_read_in_the_largest_whole_unit() {
        let now = 10 * DAY;
        let cases = [
            (0, "just now"),
            (MIN - 1, "just now"),
            (MIN, "1 min ago"),
            (HOUR - 1, "59 min ago"),
            (HOUR, "1 hour ago"),
            (2 * HOUR, "2 hours ago"),
            (DAY - 1, "23 hours ago"),
            (DAY, "1 day ago"),
            (3 * DAY + HOUR, "3 days ago"),
        ];
        for (ago, label) in cases {
            assert_eq!(age_label(now, now - ago), label, "{ago}s");
        }
        assert_eq!(age_label(now, now + HOUR), "just now", "a clock ahead");
    }
}
