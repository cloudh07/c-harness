//! The session's cost, its cost cap and how much of its input came from the cache.

use ch_core::{UsageTotals, dollars};

use crate::driver::CapReached;

const CAP_FORMAT: &str =
    "Write the cost cap in dollars, like 5 or 2.50. Leave it empty for no cap.";

/// Reads the cost cap field as cents. Empty is no cap.
pub fn parse_cap(text: &str) -> Result<Option<u32>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let amount = text.strip_prefix('$').unwrap_or(text).trim();
    let (whole, part) = amount.split_once('.').unwrap_or((amount, ""));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    if !digits(whole) || !digits(part) || part.len() > 2 || whole.len() + part.len() == 0 {
        return Err(CAP_FORMAT.into());
    }
    let whole: u32 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| too_big())?
    };
    let part: u32 = format!("{part:0<2}")
        .parse()
        .map_err(|_| CAP_FORMAT.to_string())?;
    let cents = whole
        .checked_mul(100)
        .and_then(|cents| cents.checked_add(part))
        .ok_or_else(too_big)?;
    if cents == 0 {
        return Err("A cost cap must be more than $0. Leave it empty for no cap.".into());
    }
    Ok(Some(cents))
}

fn too_big() -> String {
    "That cost cap is too large. Leave it empty for no cap.".into()
}

/// `$5.00` for 500 cents.
pub fn cap_text(cents: u32) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// What the session says when its priced turns reach the cap. It claims only what was sent.
pub fn cap_stopped(reached: CapReached) -> String {
    let abort = if reached.abort_sent {
        " Sent an abort to the running prompt."
    } else {
        ""
    };
    format!(
        "This session reached its {} cost cap.{abort} No new prompt is sent until you raise or \
         clear the cap in Settings.",
        cap_text(reached.cents)
    )
}

/// The composer's lasting note while the session is held at its cap.
pub fn cap_hint(cents: u32) -> String {
    format!(
        "At the {} cost cap: raise or clear it in Settings to send a prompt.",
        cap_text(cents)
    )
}

/// The header's cost, or `None` before the first turn ends. Turns omp could not price are
/// named, never priced at $0, and the cap does not count them.
pub fn cost_label(totals: &UsageTotals, cap: Option<u32>) -> Option<String> {
    if totals.turns == 0 {
        return None;
    }
    let unpriced = match totals.unpriced_turns {
        1 => "1 unpriced turn".to_string(),
        n => format!("{n} unpriced turns"),
    };
    let label = match (totals.cost_usd, cap) {
        (None, None) => "Cost unknown".to_string(),
        (None, Some(cap)) => format!("Cost unknown, not counted toward the {} cap", cap_text(cap)),
        (Some(cost), None) if totals.cost_complete => dollars(cost),
        (Some(cost), None) => format!("{} + {unpriced}", dollars(cost)),
        (Some(cost), Some(cap)) if totals.cost_complete => {
            format!("{} of {} cap", dollars(cost), cap_text(cap))
        }
        (Some(cost), Some(cap)) => format!(
            "{} of {} cap + {unpriced}, not counted",
            dollars(cost),
            cap_text(cap)
        ),
    };
    Some(label)
}

/// `99% cached`: the share of input read from the cache. Never rounds to 0% or 100% when it
/// is not exactly that. With turns omp could not count, it is the share of the counted ones,
/// marked `~`.
pub fn cache_share(totals: &UsageTotals) -> Option<String> {
    let share = match totals.cache_percent()? {
        percent if !totals.tokens_complete => format!("~{percent}%"),
        0 if totals.cache_read > 0 => "<1%".to_string(),
        100 if totals.input > 0 => ">99%".to_string(),
        percent => format!("{percent}%"),
    };
    Some(format!("{share} cached"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(cost: Option<f64>, unpriced: u32) -> UsageTotals {
        UsageTotals {
            turns: 3,
            input: 100,
            output: 10,
            cost_usd: cost,
            tokens_complete: true,
            cost_complete: unpriced == 0,
            unpriced_turns: unpriced,
            ..UsageTotals::default()
        }
    }

    #[test]
    fn the_cap_reads_as_whole_cents_and_empty_is_off() {
        for (text, cents) in [
            ("", None),
            ("  ", None),
            ("5", Some(500)),
            ("$5", Some(500)),
            (" $ 2.5 ", Some(250)),
            ("0.25", Some(25)),
            (".5", Some(50)),
            ("12.", Some(1200)),
        ] {
            assert_eq!(parse_cap(text), Ok(cents), "{text:?}");
        }
        for text in [
            "0",
            "$0.00",
            "-5",
            "5.555",
            "five",
            "1e3",
            "inf",
            "$",
            ".",
            "99999999999",
        ] {
            assert!(parse_cap(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn cost_says_unknown_and_partial_instead_of_zero() {
        assert_eq!(cost_label(&UsageTotals::default(), Some(500)), None);
        let label = |cost, unpriced, cap| cost_label(&totals(cost, unpriced), cap).unwrap();
        assert_eq!(label(Some(0.126), 0, None), "$0.13");
        assert_eq!(label(Some(0.004), 0, None), "<$0.01");
        assert_eq!(label(Some(0.12), 2, None), "$0.12 + 2 unpriced turns");
        assert_eq!(label(Some(0.12), 1, None), "$0.12 + 1 unpriced turn");
        assert_eq!(label(None, 3, None), "Cost unknown");
        assert_eq!(label(Some(1.5), 0, Some(500)), "$1.50 of $5.00 cap");
        assert_eq!(
            label(Some(1.5), 2, Some(500)),
            "$1.50 of $5.00 cap + 2 unpriced turns, not counted"
        );
        assert_eq!(
            label(None, 3, Some(1250)),
            "Cost unknown, not counted toward the $12.50 cap"
        );
    }

    #[test]
    fn cache_share_keeps_the_edges_honest() {
        let share = |input, cache_read| {
            cache_share(&UsageTotals {
                input,
                cache_read,
                tokens_complete: true,
                ..UsageTotals::default()
            })
        };
        assert_eq!(share(0, 0), None, "nothing reported");
        assert_eq!(share(100, 0).as_deref(), Some("0% cached"));
        assert_eq!(share(0, 100).as_deref(), Some("100% cached"));
        assert_eq!(share(25, 75).as_deref(), Some("75% cached"));
        assert_eq!(share(1_000, 1).as_deref(), Some("<1% cached"));
        assert_eq!(share(1, 1_000).as_deref(), Some(">99% cached"));
        let partial = cache_share(&UsageTotals {
            input: 1_000,
            cache_read: 1,
            tokens_complete: false,
            ..UsageTotals::default()
        });
        assert_eq!(partial.as_deref(), Some("~0% cached"), "counted turns only");
    }

    #[test]
    fn the_cap_message_names_the_cap_and_the_way_on() {
        for abort_sent in [true, false] {
            let text = cap_stopped(CapReached {
                cents: 500,
                abort_sent,
            });
            assert!(text.contains("$5.00 cost cap"), "{text}");
            assert!(
                text.contains("raise or clear the cap in Settings"),
                "{text}"
            );
            assert_eq!(text.contains("Sent an abort"), abort_sent, "{text}");
            assert!(!text.contains("Aborted"), "{text}");
        }
        let hint = cap_hint(1250);
        assert!(hint.starts_with("At the $12.50 cost cap"), "{hint}");
        assert!(hint.contains("Settings"), "{hint}");
    }
}
