//! How full omp's context window is, and when omp compacts it.
//!
//! omp answers `get_session_stats` with `contextUsage`: the last reply's token count plus an
//! estimate for what came after it. Right after a compaction omp does not know the count yet and
//! sends `null`, which is shown as unknown, never as zero.

use serde_json::Value;

use crate::Notice;
use crate::redact::redact_secrets;

/// The omp command that reads the context window.
pub const STATS_COMMAND: &str = "get_session_stats";

/// The notice kind of compaction notices. A newer one replaces the older.
pub const COMPACTION_NOTICE: &str = "context_compaction";

/// Longest reason or error text kept from a compaction frame, in chars.
const MAX_DETAIL: usize = 200;

/// One reading of the context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextReading {
    pub tokens: u64,
    /// `None` when omp did not say how large the window is.
    pub window: Option<u64>,
}

/// What the session header can say about the context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextState {
    /// No answer from omp yet.
    NotRead,
    /// omp is compacting the context right now.
    Compacting,
    /// omp answered without a count, or the read failed.
    Unknown,
    Known(ContextReading),
}

/// Follows omp's frames and keeps the latest context reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextMeter {
    level: ContextState,
    compacting: bool,
}

impl Default for ContextMeter {
    fn default() -> Self {
        Self {
            level: ContextState::NotRead,
            compacting: false,
        }
    }
}

impl ContextMeter {
    pub fn state(&self) -> ContextState {
        if self.compacting {
            ContextState::Compacting
        } else {
            self.level
        }
    }

    /// Folds in one omp frame. Returns `true` when the app should send `get_session_stats` now:
    /// after each turn and after a compaction.
    pub fn on_omp(&mut self, frame: &Value) -> bool {
        match frame.get("type").and_then(Value::as_str) {
            Some("turn_end") => true,
            Some("auto_compaction_start") => {
                self.compacting = true;
                false
            }
            Some("auto_compaction_end") => {
                self.compacting = false;
                true
            }
            Some("response") if is_stats(frame) => {
                self.level = read(frame);
                false
            }
            _ => false,
        }
    }
}

/// Whether `frame` answers `get_session_stats`.
pub fn is_stats(frame: &Value) -> bool {
    frame.get("command").and_then(Value::as_str) == Some(STATS_COMMAND)
}

fn read(response: &Value) -> ContextState {
    if response.get("success").and_then(Value::as_bool) != Some(true) {
        return ContextState::Unknown;
    }
    let usage = response.get("data").and_then(|d| d.get("contextUsage"));
    let Some(tokens) = usage.and_then(|u| u.get("tokens")).and_then(Value::as_u64) else {
        return ContextState::Unknown;
    };
    let window = usage
        .and_then(|u| u.get("contextWindow"))
        .and_then(Value::as_u64)
        .filter(|w| *w > 0);
    ContextState::Known(ContextReading { tokens, window })
}

/// The notice for an `auto_compaction_start` or `auto_compaction_end` frame.
pub fn compaction_notice(frame: &Value) -> Option<Notice> {
    let text = |key| {
        frame
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let flag = |key| frame.get(key).and_then(Value::as_bool) == Some(true);
    let message = match frame.get("type").and_then(Value::as_str)? {
        "auto_compaction_start" => match text("reason") {
            Some(reason) => format!("Compacting the context ({}).", clip(reason)),
            None => "Compacting the context.".to_string(),
        },
        "auto_compaction_end" => {
            let outcome = if flag("aborted") {
                "Context compaction was stopped.".to_string()
            } else if let Some(error) = text("errorMessage") {
                format!(
                    "Context compaction failed: {}",
                    clip(&redact_secrets(error))
                )
            } else if flag("skipped") {
                "Context compaction was skipped.".to_string()
            } else {
                "Context compacted.".to_string()
            };
            if flag("willRetry") {
                format!("{outcome} omp will retry.")
            } else {
                outcome
            }
        }
        _ => return None,
    };
    Some(Notice {
        kind: COMPACTION_NOTICE.to_string(),
        message,
    })
}

/// Shows the notice for a compaction frame in place of the previous compaction notice.
pub(crate) fn note_compaction(notices: &mut Vec<Notice>, frame: &Value) {
    if let Some(notice) = compaction_notice(frame) {
        notices.retain(|n| n.kind != COMPACTION_NOTICE);
        notices.push(notice);
    }
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_DETAIL) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stats(usage: &Value) -> Value {
        json!({
            "id": "req_3", "type": "response", "command": STATS_COMMAND, "success": true,
            "data": { "sessionId": "x", "contextUsage": usage },
        })
    }

    #[test]
    fn asks_for_stats_after_turns_and_after_compaction() {
        let mut meter = ContextMeter::default();
        for kind in ["turn_end", "auto_compaction_end"] {
            assert!(meter.on_omp(&json!({ "type": kind })), "{kind}");
        }
        for kind in [
            "ready",
            "agent_start",
            "message_update",
            "auto_compaction_start",
            "agent_end",
        ] {
            assert!(!meter.on_omp(&json!({ "type": kind })), "{kind}");
        }
        assert!(!meter.on_omp(&json!({ "no": "type" })));
    }

    #[test]
    fn reads_tokens_and_window() {
        let mut meter = ContextMeter::default();
        assert_eq!(meter.state(), ContextState::NotRead);
        let usage = json!({ "tokens": 18003, "contextWindow": 1_050_000, "percent": 1.7 });
        assert!(!meter.on_omp(&stats(&usage)));
        assert_eq!(
            meter.state(),
            ContextState::Known(ContextReading {
                tokens: 18003,
                window: Some(1_050_000),
            })
        );
    }

    #[test]
    fn a_window_of_zero_or_none_is_not_a_window() {
        for usage in [
            json!({ "tokens": 5, "contextWindow": 0 }),
            json!({ "tokens": 5 }),
        ] {
            let mut meter = ContextMeter::default();
            meter.on_omp(&stats(&usage));
            assert_eq!(
                meter.state(),
                ContextState::Known(ContextReading {
                    tokens: 5,
                    window: None,
                }),
                "{usage}"
            );
        }
    }

    #[test]
    fn a_missing_count_is_unknown_not_zero() {
        let answers = [
            stats(&json!({ "tokens": null, "contextWindow": 200_000, "percent": null })),
            stats(&json!({ "tokens": -1, "contextWindow": 200_000 })),
            stats(&json!({ "tokens": 12.5, "contextWindow": 200_000 })),
            stats(&Value::Null),
            json!({ "type": "response", "command": STATS_COMMAND, "success": true }),
            json!({
                "type": "response", "command": STATS_COMMAND, "success": false,
                "error": "no session",
            }),
        ];
        for answer in answers {
            let mut meter = ContextMeter::default();
            meter.on_omp(&stats(&json!({ "tokens": 9 })));
            meter.on_omp(&answer);
            assert_eq!(meter.state(), ContextState::Unknown, "{answer}");
        }
    }

    #[test]
    fn other_responses_leave_the_reading_alone() {
        let mut meter = ContextMeter::default();
        meter.on_omp(&stats(&json!({ "tokens": 9 })));
        let before = meter.state();
        meter.on_omp(&json!({
            "type": "response", "command": "prompt", "success": true,
            "data": { "contextUsage": { "tokens": 1 } },
        }));
        assert_eq!(meter.state(), before);
    }

    #[test]
    fn compacting_shows_until_compaction_ends() {
        let mut meter = ContextMeter::default();
        meter.on_omp(&stats(&json!({ "tokens": 9 })));
        meter.on_omp(&json!({ "type": "auto_compaction_start", "reason": "threshold" }));
        assert_eq!(meter.state(), ContextState::Compacting);
        // An answer that lands mid-compaction is kept, but the state still says compacting.
        meter.on_omp(&stats(&json!({ "tokens": 7 })));
        assert_eq!(meter.state(), ContextState::Compacting);
        meter.on_omp(&json!({ "type": "auto_compaction_end", "aborted": false }));
        assert_eq!(
            meter.state(),
            ContextState::Known(ContextReading {
                tokens: 7,
                window: None,
            })
        );
    }

    #[test]
    fn compaction_notices_say_what_happened() {
        let cases = [
            (
                json!({ "type": "auto_compaction_start", "reason": "threshold", "action": "context-full" }),
                "Compacting the context (threshold).",
            ),
            (
                json!({ "type": "auto_compaction_start" }),
                "Compacting the context.",
            ),
            (
                json!({ "type": "auto_compaction_end", "aborted": false, "willRetry": false }),
                "Context compacted.",
            ),
            (
                json!({ "type": "auto_compaction_end", "aborted": true, "willRetry": false }),
                "Context compaction was stopped.",
            ),
            (
                json!({ "type": "auto_compaction_end", "aborted": false, "skipped": true }),
                "Context compaction was skipped.",
            ),
            (
                json!({
                    "type": "auto_compaction_end", "aborted": false, "willRetry": true,
                    "errorMessage": "rate limited",
                }),
                "Context compaction failed: rate limited omp will retry.",
            ),
        ];
        for (frame, message) in cases {
            let notice = compaction_notice(&frame).expect("a notice");
            assert_eq!(notice.kind, COMPACTION_NOTICE);
            assert_eq!(notice.message, message, "{frame}");
        }
        assert_eq!(compaction_notice(&json!({ "type": "turn_end" })), None);
    }

    #[test]
    fn compaction_errors_are_clipped_and_redacted() {
        let long = "x".repeat(MAX_DETAIL * 3);
        let notice = compaction_notice(&json!({
            "type": "auto_compaction_end", "aborted": false, "errorMessage": long,
        }))
        .unwrap();
        assert!(
            notice.message.chars().count() < MAX_DETAIL + 40,
            "{}",
            notice.message
        );
        assert!(notice.message.ends_with('…'));

        let key = format!("sk-ant-api03-{}", "A".repeat(40));
        let notice = compaction_notice(&json!({
            "type": "auto_compaction_end", "aborted": false,
            "errorMessage": format!("bad key {key}"),
        }))
        .unwrap();
        assert!(!notice.message.contains(&key), "{}", notice.message);
    }
}
