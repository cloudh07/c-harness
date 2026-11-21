//! Transcript records: one JSON line per frame, with what must never reach a disk taken out.
//!
//! Each direction has an allowlist of frame types, and each type says which fields it keeps. A
//! frame type that is not listed is not recorded, so a new omp event or runner frame stays out
//! of the file until someone decides what of it is safe to keep. Every string that is kept, and
//! every object key, goes through [`redact_secrets`], and a value under a credential-like key
//! is replaced by [`MASK`].

use ch_protocol::runner::RunnerLine;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::context::STATS_COMMAND;
use crate::history::HISTORY_COMMAND;
use crate::redact::{MASK, is_secret_name, redact_secrets};

enum Keep {
    All,
    Fields(&'static [&'static str]),
    // `message_update` repeats the whole partial message next to every delta, which makes a
    // transcript grow with the square of the reply. The delta alone rebuilds the text.
    Delta,
}

const DELTA_EVENT_FIELDS: &[&str] = &["type", "contentIndex", "delta"];

/// Frames the app read from the runner. `rpc_chunk` is transport, reassembled before it gets
/// here. `tool_execution_update` is left out because each one repeats the output so far, and
/// `tool_execution_end` carries the final result.
const INBOUND: &[(&str, Keep)] = &[
    ("ready", Keep::All),
    ("response", Keep::All),
    ("agent_start", Keep::All),
    ("agent_end", Keep::All),
    ("message_update", Keep::Delta),
    ("tool_execution_start", Keep::All),
    ("tool_execution_end", Keep::All),
    ("prompt_result", Keep::All),
    ("session_settled", Keep::All),
    ("extension_ui_request", Keep::All),
    // `result` holds omp's summary of the conversation, which the deltas already carry.
    (
        "auto_compaction_start",
        Keep::Fields(&["type", "reason", "action"]),
    ),
    (
        "auto_compaction_end",
        Keep::Fields(&[
            "type",
            "action",
            "aborted",
            "willRetry",
            "skipped",
            "errorMessage",
        ]),
    ),
    ("ch_runner_hello", Keep::All),
    ("ch_notice", Keep::All),
    ("ch_evidence", Keep::All),
    ("ch_diff_result", Keep::All),
    ("ch_stopped", Keep::All),
    ("ch_landed", Keep::All),
    ("ch_omp_exited", Keep::All),
    ("ch_error", Keep::All),
];

/// Frames the app wrote. `ch_start` carries the model key in `env`, and `ch_land` carries the
/// git host token, so both keep a named subset rather than dropping the known secret: a field
/// added later stays out until it is listed here.
const OUTBOUND: &[(&str, Keep)] = &[
    ("prompt", Keep::Fields(&["type", "id", "message"])),
    ("abort_and_prompt", Keep::Fields(&["type", "id", "message"])),
    ("abort", Keep::Fields(&["type", "id"])),
    (
        "extension_ui_response",
        Keep::Fields(&["type", "id", "value", "cancelled"]),
    ),
    ("ch_start", Keep::Fields(&["type", "session"])),
    (
        "ch_verify",
        Keep::Fields(&["type", "id", "command", "timeout_secs"]),
    ),
    ("ch_diff", Keep::Fields(&["type", "id"])),
    ("ch_stop", Keep::Fields(&["type", "id"])),
    (
        "ch_land",
        Keep::Fields(&[
            "type",
            "id",
            "branch",
            "message",
            "author",
            "remote_url",
            "pull_request",
        ]),
    ),
];

/// Commands the app sends on its own to read state. Their answers repeat every turn and say
/// nothing about the work, and `get_session_stats` names omp's own session file.
const POLLED: &[&str] = &[STATS_COMMAND, HISTORY_COMMAND];

#[derive(Serialize)]
struct Record<'a> {
    epoch: u64,
    seq: u64,
    dir: &'static str,
    frame: &'a Value,
}

/// Numbers and formats the records of one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    next_seq: u64,
}

impl Default for Transcript {
    fn default() -> Self {
        Self { next_seq: 1 }
    }
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// The record for a line the app read from the runner, or `None` when its type is not
    /// recorded.
    ///
    /// `epoch_ms` is the caller's clock, in milliseconds since the Unix epoch. The record is
    /// one line of JSON without the trailing newline.
    pub fn incoming(&mut self, epoch_ms: u64, line: &RunnerLine) -> Option<String> {
        match line {
            RunnerLine::Omp(frame) if answers_a_poll(frame) => None,
            RunnerLine::Omp(frame) => self.record(epoch_ms, "in", INBOUND, frame),
            RunnerLine::Runner(frame) => {
                let frame = serde_json::to_value(frame).ok()?;
                self.record(epoch_ms, "in", INBOUND, &frame)
            }
        }
    }

    /// The record for a frame the app wrote, omp command or runner frame alike, or `None` when
    /// its type is not recorded. A `ClientFrame` goes in as `serde_json::to_value(&frame)`.
    pub fn outgoing(&mut self, epoch_ms: u64, frame: &Value) -> Option<String> {
        self.record(epoch_ms, "out", OUTBOUND, frame)
    }

    fn record(
        &mut self,
        epoch: u64,
        dir: &'static str,
        policy: &[(&str, Keep)],
        frame: &Value,
    ) -> Option<String> {
        let fields = frame.as_object()?;
        let kind = fields.get("type")?.as_str()?;
        let (_, keep) = policy.iter().find(|(listed, _)| *listed == kind)?;
        let kept = match keep {
            Keep::All => fields.iter().map(|(n, v)| redacted_field(n, v)).collect(),
            Keep::Fields(names) => pick(fields, names),
            Keep::Delta => {
                let mut kept = pick(fields, &["type"]);
                if let Some(Value::Object(event)) = fields.get("assistantMessageEvent") {
                    kept["assistantMessageEvent"] = pick(event, DELTA_EVENT_FIELDS);
                }
                kept
            }
        };
        let line = serde_json::to_string(&Record {
            epoch,
            seq: self.next_seq,
            dir,
            frame: &kept,
        })
        .ok()?;
        // Numbered after filtering, so the numbers run without gaps and a missing one means the
        // file lost a line.
        self.next_seq += 1;
        Some(line)
    }
}

fn answers_a_poll(frame: &Value) -> bool {
    frame.get("type").and_then(Value::as_str) == Some("response")
        && frame
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| POLLED.contains(&command))
}

fn pick(fields: &Map<String, Value>, names: &[&str]) -> Value {
    names
        .iter()
        .filter_map(|name| fields.get_key_value(*name))
        .map(|(n, v)| redacted_field(n, v))
        .collect()
}

fn redacted_field(name: &str, value: &Value) -> (String, Value) {
    let value = if is_secret_name(name) && !value.is_null() {
        Value::String(MASK.to_owned())
    } else {
        redacted(value)
    };
    (redact_secrets(name), value)
}

fn redacted(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_secrets(text)),
        Value::Array(items) => items.iter().map(redacted).collect(),
        Value::Object(fields) => fields.iter().map(|(n, v)| redacted_field(n, v)).collect(),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ch_protocol::runner::{
        ClientFrame, GitAuthor, LandRequest, PullRequestSpec, StartSession, route_runner_line,
    };
    use serde_json::json;

    use super::*;

    const FIXTURE: &str = include_str!("../../../testdata/omp/approval-session.jsonl");
    const FAKE_KEY: &str = "sk-ant-api03-FAKEFAKEFAKEFAKEFAKEFAKE";
    const FAKE_TOKEN: &str = "FAKE-GIT-HOST-TOKEN-FOR-TESTS";

    fn frame_of(record: &str) -> Value {
        let record: Value = serde_json::from_str(record).expect("a record is JSON");
        record["frame"].clone()
    }

    fn incoming(transcript: &mut Transcript, line: &str) -> Option<String> {
        let line = route_runner_line(line).expect("a routable line");
        transcript.incoming(0, &line)
    }

    #[test]
    fn writes_epoch_seq_dir_and_frame_in_that_order() {
        let mut transcript = Transcript::new();
        let first = incoming(&mut transcript, r#"{"type":"ready"}"#);
        let second = transcript.outgoing(
            1_700_000_000_123,
            &json!({"type":"prompt","id":"req_1","message":"fix it"}),
        );
        assert_eq!(
            first.as_deref(),
            Some(r#"{"epoch":0,"seq":1,"dir":"in","frame":{"type":"ready"}}"#)
        );
        // Key order inside `frame` follows serde_json's map, which a workspace feature can change.
        let second = second.expect("recorded");
        assert!(
            second.starts_with(r#"{"epoch":1700000000123,"seq":2,"dir":"out","frame":{"#),
            "{second}"
        );
        assert_eq!(
            frame_of(&second),
            json!({"type":"prompt","id":"req_1","message":"fix it"})
        );
    }

    #[test]
    fn a_start_keeps_only_its_type_and_session() {
        let start = ClientFrame::Start(StartSession {
            session: "s1".into(),
            omp_args: vec![
                "--mode".into(),
                "rpc".into(),
                "--resume=/home/u/.omp/s.jsonl".into(),
            ],
            config_overlay: Some("providers: {}\n".into()),
            env: BTreeMap::from([("ANTHROPIC_API_KEY".into(), FAKE_KEY.into())]),
        });
        let frame = serde_json::to_value(&start).expect("serializes");
        let record = Transcript::new().outgoing(0, &frame).expect("recorded");
        assert_eq!(frame_of(&record), json!({"type":"ch_start","session":"s1"}));
        assert!(!record.contains("FAKE"));
        assert!(!record.contains(".omp"));
    }

    #[test]
    fn a_land_drops_the_token_and_the_credentials_in_the_remote() {
        let land = ClientFrame::Land(LandRequest {
            id: "l1".into(),
            branch: "ch/fix".into(),
            message: "fix: handle empty input".into(),
            author: GitAuthor {
                name: "Ada".into(),
                email: "ada@example.com".into(),
            },
            remote_url: format!("https://x-access-token:{FAKE_TOKEN}@github.com/octo/repo.git"),
            token: Some(FAKE_TOKEN.into()),
            pull_request: Some(PullRequestSpec {
                repo: "octo/repo".into(),
                base: "main".into(),
                title: "Fix empty input".into(),
                body: "Handles an empty file.".into(),
            }),
        });
        let frame = serde_json::to_value(&land).expect("serializes");
        let record = Transcript::new().outgoing(0, &frame).expect("recorded");
        let kept = frame_of(&record);
        assert!(!record.contains(FAKE_TOKEN), "{record}");
        assert_eq!(kept.get("token"), None);
        assert_eq!(kept["branch"], "ch/fix");
        assert_eq!(kept["pull_request"]["title"], "Fix empty input");
        assert!(
            kept["remote_url"]
                .as_str()
                .unwrap()
                .contains("github.com/octo/repo")
        );
    }

    #[test]
    fn unlisted_frames_are_not_recorded_and_leave_no_gap() {
        let mut transcript = Transcript::new();
        for line in [
            r#"{"type":"rpc_chunk","id":"c1","index":0,"count":2,"data":"{"}"#,
            r#"{"type":"tool_execution_update","toolCallId":"c1","partialResult":{}}"#,
            r#"{"type":"some_future_event","payload":"x"}"#,
        ] {
            assert_eq!(incoming(&mut transcript, line), None, "{line}");
        }
        for frame in [
            json!({"type":"set_model","provider":"anthropic"}),
            json!({"type":"bash","command":"env"}),
            json!({"no_type":true}),
            json!("prompt"),
        ] {
            assert_eq!(transcript.outgoing(0, &frame), None, "{frame}");
        }
        let next = incoming(&mut transcript, r#"{"type":"agent_start"}"#).expect("recorded");
        assert!(next.contains(r#""seq":1,"#), "{next}");
    }

    #[test]
    fn redacts_every_kept_string_and_credential_keys() {
        let line = format!(
            r#"{{"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","isError":false,"result":{{"content":[{{"type":"text","text":"ok\nANTHROPIC_API_KEY={FAKE_KEY}\ndone"}}],"details":{{"apiKey":12345,"token":null,"input_tokens":812}}}}}}"#
        );
        let record = incoming(&mut Transcript::new(), &line).expect("recorded");
        assert!(!record.contains("FAKEFAKE"), "{record}");
        assert!(!record.contains("12345"), "{record}");
        let kept = frame_of(&record);
        let text = kept["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("ok\nANTHROPIC_API_KEY="), "{text}");
        assert!(text.ends_with("\ndone"), "{text}");
        let details = &kept["result"]["details"];
        assert_eq!(details["apiKey"], MASK);
        assert_eq!(details["token"], Value::Null);
        assert_eq!(details["input_tokens"], 812);
    }

    #[test]
    fn redacts_runner_frames_too() {
        let line = format!(
            r#"{{"type":"ch_evidence","id":"v1","command":"cargo test","exit_code":1,"duration_ms":9,"output_tail":"OPENAI_API_KEY={FAKE_KEY}","truncated":false,"timed_out":false}}"#
        );
        let record = incoming(&mut Transcript::new(), &line).expect("recorded");
        assert!(!record.contains("FAKEFAKE"), "{record}");
        assert_eq!(frame_of(&record)["command"], "cargo test");
    }

    #[test]
    fn a_message_update_keeps_the_delta_without_the_partial_message() {
        let line = r#"{"type":"message_update","message":{"role":"assistant","content":"Hel"},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"l","partial":{"content":"Hel"}}}"#;
        let record = incoming(&mut Transcript::new(), line).expect("recorded");
        assert_eq!(
            frame_of(&record),
            json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "l"},
            })
        );
    }

    #[test]
    fn stats_answers_are_not_recorded_and_take_no_number() {
        let mut transcript = Transcript::new();
        let stats = r#"{"id":"req_2","type":"response","command":"get_session_stats","success":true,"data":{"sessionFile":"/home/u/.omp/s.jsonl"}}"#;
        assert_eq!(incoming(&mut transcript, stats), None);
        let ask = json!({"id": "req_2", "type": STATS_COMMAND});
        assert_eq!(transcript.outgoing(0, &ask), None);
        let prompt = r#"{"id":"req_1","type":"response","command":"prompt","success":true}"#;
        let record = incoming(&mut transcript, prompt).expect("recorded");
        assert!(record.contains(r#""seq":1"#), "{record}");
    }

    #[test]
    fn history_pages_are_not_recorded() {
        let mut transcript = Transcript::new();
        let ask = json!({"id": "req_3", "type": HISTORY_COMMAND, "limit": 256});
        assert_eq!(transcript.outgoing(0, &ask), None);
        let page = r#"{"id":"req_3","type":"response","command":"get_messages_page","success":true,"data":{"messages":[{"role":"user","content":"old secret plan"}],"totalMessages":1}}"#;
        assert_eq!(incoming(&mut transcript, page), None);
        let busy = r#"{"id":"req_4","type":"response","command":"get_messages_page","success":false,"error":"Session is busy","code":"session_busy"}"#;
        assert_eq!(incoming(&mut transcript, busy), None);
        let ready = r#"{"type":"ready"}"#;
        let record = incoming(&mut transcript, ready).expect("recorded");
        assert!(record.contains(r#""seq":1"#), "{record}");
    }

    #[test]
    fn compaction_frames_keep_their_outcome_without_the_summary() {
        let start =
            r#"{"type":"auto_compaction_start","reason":"threshold","action":"context-full"}"#;
        let record = incoming(&mut Transcript::new(), start).expect("recorded");
        assert_eq!(
            frame_of(&record),
            json!({"type": "auto_compaction_start", "reason": "threshold", "action": "context-full"})
        );
        let end = r#"{"type":"auto_compaction_end","action":"context-full","aborted":false,"willRetry":false,"result":{"summary":"the user asked for secret things"}}"#;
        let record = incoming(&mut Transcript::new(), end).expect("recorded");
        assert_eq!(
            frame_of(&record),
            json!({
                "type": "auto_compaction_end", "action": "context-full",
                "aborted": false, "willRetry": false,
            })
        );
    }

    #[test]
    fn records_the_fixture_session_except_progress_updates() {
        let mut transcript = Transcript::new();
        let records: Vec<String> = FIXTURE
            .lines()
            .filter_map(|line| incoming(&mut transcript, line))
            .collect();
        assert_eq!(records.len(), FIXTURE.lines().count() - 1);
        for (n, record) in records.iter().enumerate() {
            let record: Value = serde_json::from_str(record).expect("JSON");
            assert_eq!(record["seq"], n + 1);
            assert_eq!(record["dir"], "in");
        }
    }
}
