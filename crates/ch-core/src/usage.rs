//! Tokens and cost per turn, read from omp's `turn_end` frames.
//!
//! omp starts every assistant message with zero usage and fills in what the provider reports.
//! So a zero here means "not reported", never "free": such fields stay `None`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How much of a turn's usage omp reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageStatus {
    /// The turn ended normally and every token count came through.
    Complete,
    /// Some counts came through, but the turn was cut short or a count is missing.
    Partial,
    /// No counts came through.
    Unavailable,
}

/// One model turn: what it used, never what it said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRow {
    pub turn: u32,
    pub at_ms: u64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    /// omp's estimate in US dollars. `None` when omp priced the turn at zero.
    pub cost_usd: Option<f64>,
    pub status: UsageStatus,
}

impl UsageRow {
    /// Reads the assistant message of a `turn_end` frame.
    pub fn from_turn_end(turn: u32, at_ms: u64, frame: &Value) -> Self {
        let message = frame.get("message");
        let text = |key| {
            message
                .and_then(|m| m.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let usage = message.and_then(|m| m.get("usage"));
        let count = |key| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);
        let counts = [
            count("input"),
            count("output"),
            count("cacheRead"),
            count("cacheWrite"),
        ];
        let reported =
            counts.iter().flatten().any(|n| *n > 0) || count("totalTokens").is_some_and(|n| n > 0);
        let mut row = Self {
            turn,
            at_ms,
            provider: text("provider"),
            model: text("model"),
            input: None,
            output: None,
            cache_read: None,
            cache_write: None,
            cost_usd: None,
            status: UsageStatus::Unavailable,
        };
        if !reported {
            return row;
        }
        [row.input, row.output, row.cache_read, row.cache_write] = counts;
        row.cost_usd = usage
            .and_then(|u| u.pointer("/cost/total"))
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost > 0.0);
        let ended = text("stopReason").is_some_and(|stop| stop != "aborted" && stop != "error");
        row.status = if ended && counts.iter().all(Option::is_some) {
            UsageStatus::Complete
        } else {
            UsageStatus::Partial
        };
        row
    }
}

/// Sums over every turn so far.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageTotals {
    pub turns: u32,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// `None` when no turn had a known cost.
    pub cost_usd: Option<f64>,
    /// Every turn is [`UsageStatus::Complete`].
    pub tokens_complete: bool,
    /// Every turn had a known cost.
    pub cost_complete: bool,
}

/// The rows of one session, in turn order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    rows: Vec<UsageRow>,
}

impl Usage {
    /// Adds a row for a `turn_end` frame and returns it; other frames add nothing.
    pub fn on_omp(&mut self, at_ms: u64, frame: &Value) -> Option<&UsageRow> {
        if frame.get("type").and_then(Value::as_str) != Some("turn_end") {
            return None;
        }
        let turn = u32::try_from(self.rows.len() + 1).unwrap_or(u32::MAX);
        self.rows.push(UsageRow::from_turn_end(turn, at_ms, frame));
        self.rows.last()
    }

    pub fn rows(&self) -> &[UsageRow] {
        &self.rows
    }

    pub fn totals(&self) -> UsageTotals {
        let mut totals = UsageTotals {
            turns: u32::try_from(self.rows.len()).unwrap_or(u32::MAX),
            tokens_complete: true,
            cost_complete: true,
            ..UsageTotals::default()
        };
        for row in &self.rows {
            totals.input += row.input.unwrap_or(0);
            totals.output += row.output.unwrap_or(0);
            totals.cache_read += row.cache_read.unwrap_or(0);
            totals.cache_write += row.cache_write.unwrap_or(0);
            if let Some(cost) = row.cost_usd {
                totals.cost_usd = Some(totals.cost_usd.unwrap_or(0.0) + cost);
            }
            totals.tokens_complete &= row.status == UsageStatus::Complete;
            totals.cost_complete &= row.cost_usd.is_some();
        }
        if self.rows.is_empty() {
            totals.tokens_complete = false;
            totals.cost_complete = false;
        }
        totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn_end(stop: &str, usage: &Value) -> Value {
        json!({
            "type": "turn_end",
            "turnIndex": 0,
            "message": {
                "role": "assistant",
                "content": [],
                "api": "anthropic-messages",
                "provider": "anthropic",
                "model": "claude-sonnet-5-5",
                "usage": usage,
                "stopReason": stop,
                "timestamp": 1
            },
            "toolResults": []
        })
    }

    fn priced(input: u64, output: u64, total: f64) -> Value {
        json!({
            "input": input, "output": output, "cacheRead": 200, "cacheWrite": 10,
            "totalTokens": input + output + 210,
            "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": total}
        })
    }

    fn zeros() -> Value {
        priced(0, 0, 0.0)
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), if v.is_object() { v.clone() } else { json!(0) }))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }

    #[test]
    fn a_finished_turn_is_complete() {
        let row = UsageRow::from_turn_end(3, 99, &turn_end("toolUse", &priced(1200, 300, 0.012)));
        assert_eq!(
            row,
            UsageRow {
                turn: 3,
                at_ms: 99,
                provider: Some("anthropic".into()),
                model: Some("claude-sonnet-5-5".into()),
                input: Some(1200),
                output: Some(300),
                cache_read: Some(200),
                cache_write: Some(10),
                cost_usd: Some(0.012),
                status: UsageStatus::Complete,
            }
        );
    }

    #[test]
    fn zero_cost_with_tokens_is_unknown_cost() {
        let row = UsageRow::from_turn_end(1, 0, &turn_end("stop", &priced(50, 5, 0.0)));
        assert_eq!(row.status, UsageStatus::Complete);
        assert_eq!(row.input, Some(50));
        assert_eq!(row.cost_usd, None);
    }

    #[test]
    fn zero_filled_usage_is_unavailable() {
        let row = UsageRow::from_turn_end(1, 0, &turn_end("stop", &zeros()));
        assert_eq!(row.status, UsageStatus::Unavailable);
        assert_eq!(
            (row.input, row.output, row.cache_read, row.cache_write),
            (None, None, None, None)
        );
        assert_eq!(row.cost_usd, None);
        assert_eq!(row.model.as_deref(), Some("claude-sonnet-5-5"));
    }

    #[test]
    fn a_cut_short_turn_is_partial() {
        for stop in ["aborted", "error"] {
            let row = UsageRow::from_turn_end(1, 0, &turn_end(stop, &priced(10, 2, 0.001)));
            assert_eq!(row.status, UsageStatus::Partial, "{stop}");
            assert_eq!(row.output, Some(2));
        }
    }

    #[test]
    fn a_missing_count_is_partial_and_stays_null() {
        let usage = json!({"input": 10, "output": 2, "totalTokens": 12});
        let row = UsageRow::from_turn_end(1, 0, &turn_end("stop", &usage));
        assert_eq!(row.status, UsageStatus::Partial);
        assert_eq!((row.cache_read, row.cache_write), (None, None));
        assert_eq!(row.cost_usd, None);
    }

    #[test]
    fn a_frame_without_usage_is_unavailable() {
        let frame = json!({"type": "turn_end", "message": {"role": "assistant"}});
        let row = UsageRow::from_turn_end(2, 7, &frame);
        assert_eq!(row.status, UsageStatus::Unavailable);
        assert_eq!((row.provider, row.model, row.input), (None, None, None));

        let bare = UsageRow::from_turn_end(2, 7, &json!({"type": "turn_end"}));
        assert_eq!(bare.status, UsageStatus::Unavailable);
    }

    #[test]
    fn missing_values_are_written_as_null() {
        let row = UsageRow::from_turn_end(1, 5, &json!({"type": "turn_end"}));
        let line = serde_json::to_value(&row).unwrap();
        assert_eq!(line["input"], Value::Null);
        assert_eq!(line["cost_usd"], Value::Null);
        assert_eq!(line["status"], "unavailable");
        let back: UsageRow = serde_json::from_value(line).unwrap();
        assert_eq!(back, row);
    }

    #[test]
    fn only_turn_end_adds_a_row_and_turns_count_up() {
        let mut usage = Usage::default();
        assert!(usage.on_omp(1, &json!({"type": "message_end"})).is_none());
        assert!(usage.on_omp(1, &json!({"type": "agent_end"})).is_none());
        let first = usage
            .on_omp(10, &turn_end("toolUse", &priced(100, 10, 0.01)))
            .cloned()
            .unwrap();
        let second = usage
            .on_omp(20, &turn_end("stop", &priced(200, 20, 0.02)))
            .cloned()
            .unwrap();
        assert_eq!((first.turn, first.at_ms), (1, 10));
        assert_eq!((second.turn, second.at_ms), (2, 20));
        assert_eq!(usage.rows().len(), 2);
    }

    #[test]
    fn totals_sum_known_values_and_flag_gaps() {
        let mut usage = Usage::default();
        assert_eq!(usage.totals(), UsageTotals::default());

        usage.on_omp(1, &turn_end("stop", &priced(100, 10, 0.25)));
        let clean = usage.totals();
        assert_eq!((clean.turns, clean.input, clean.output), (1, 100, 10));
        assert_eq!((clean.cache_read, clean.cache_write), (200, 10));
        assert_eq!(clean.cost_usd, Some(0.25));
        assert!(clean.tokens_complete && clean.cost_complete);

        usage.on_omp(2, &turn_end("stop", &priced(50, 5, 0.0)));
        usage.on_omp(3, &turn_end("stop", &zeros()));
        let gaps = usage.totals();
        assert_eq!((gaps.turns, gaps.input, gaps.output), (3, 150, 15));
        assert_eq!(gaps.cost_usd, Some(0.25));
        assert!(!gaps.tokens_complete);
        assert!(!gaps.cost_complete);
    }

    #[test]
    fn totals_have_no_cost_when_none_is_known() {
        let mut usage = Usage::default();
        usage.on_omp(1, &turn_end("stop", &priced(100, 10, 0.0)));
        let totals = usage.totals();
        assert_eq!(totals.cost_usd, None);
        assert!(totals.tokens_complete);
        assert!(!totals.cost_complete);
    }
}
