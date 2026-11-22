//! The conversation omp already had, read back after a resume.
//!
//! omp answers `get_messages_page` with `{ messages, nextCursor?, totalMessages }`, oldest first.
//! Only the text of user and assistant messages is kept, as (prompt, reply) pairs. Tool calls,
//! tool results, images, thinking and roles C-Harness does not know are skipped.

use std::collections::VecDeque;

use serde_json::{Value, json};

use crate::context::clip;
use crate::redact::redact_secrets;

/// The omp command that reads one page of the conversation.
pub const HISTORY_COMMAND: &str = "get_messages_page";

/// Messages asked for per page, as omp's own client does.
pub const HISTORY_PAGE_LIMIT: u64 = 256;

/// The most turns kept for the screen. Older ones are counted, not kept.
pub const MAX_HISTORY_TURNS: usize = 200;

/// The most characters kept of one prompt or one reply. The screen shows fewer.
pub const MAX_TURN_CHARS: usize = 32_000;

/// One page of omp's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPage {
    pub messages: Vec<Value>,
    /// `None` on the last page.
    pub next_cursor: Option<String>,
}

/// The `get_messages_page` request for the page at `cursor`, or the first page.
pub fn history_request(id: &str, cursor: Option<&str>) -> Value {
    let mut frame = json!({ "id": id, "type": HISTORY_COMMAND, "limit": HISTORY_PAGE_LIMIT });
    if let Some(cursor) = cursor {
        frame["cursor"] = json!(cursor);
    }
    frame
}

/// Reads omp's answer to a page request. An error answer becomes its message and code, clipped
/// and with secrets masked.
pub fn read_page(frame: &Value) -> Result<HistoryPage, String> {
    if frame.get("success").and_then(Value::as_bool) != Some(true) {
        let error = frame
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("omp gave no reason");
        let text = match frame.get("code").and_then(Value::as_str) {
            Some(code) => format!("{error} ({code})"),
            None => error.to_string(),
        };
        return Err(clip(&redact_secrets(&text)));
    }
    let data = frame.get("data");
    let messages = data
        .and_then(|d| d.get("messages"))
        .and_then(Value::as_array)
        .ok_or_else(|| "omp's answer had no message list".to_string())?;
    let next_cursor = data
        .and_then(|d| d.get("nextCursor"))
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    Ok(HistoryPage {
        messages: messages.clone(),
        next_cursor,
    })
}

/// Folds omp's messages, oldest first, into (prompt, reply) pairs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    turns: VecDeque<(String, String)>,
    dropped: usize,
    /// The last pair is still taking assistant text.
    open: bool,
}

impl History {
    pub fn add(&mut self, message: &Value) {
        let role = message.get("role").and_then(Value::as_str);
        let text = || text_of(message.get("content"));
        match role {
            Some("user") => {
                self.push((keep_start(text()), String::new()));
                self.open = true;
            }
            Some("assistant") => {
                let text = text();
                if text.is_empty() {
                    return;
                }
                if !self.open {
                    // A reply with no prompt, such as the first after a compaction.
                    self.push((String::new(), String::new()));
                    self.open = true;
                }
                if let Some((_, reply)) = self.turns.back_mut() {
                    if !reply.is_empty() {
                        reply.push_str("\n\n");
                    }
                    reply.push_str(&text);
                    keep_end(reply);
                }
            }
            _ => {}
        }
    }

    /// Pairs dropped because there were more than [`MAX_HISTORY_TURNS`].
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn into_turns(self) -> Vec<(String, String)> {
        self.turns.into()
    }

    fn push(&mut self, turn: (String, String)) {
        if self.turns.len() == MAX_HISTORY_TURNS {
            self.turns.pop_front();
            self.dropped += 1;
        }
        self.turns.push_back(turn);
    }
}

/// The first [`MAX_TURN_CHARS`] characters of `text`, with "…" after them when it was longer.
fn keep_start(mut text: String) -> String {
    if let Some((cut, _)) = text.char_indices().nth(MAX_TURN_CHARS) {
        text.truncate(cut);
        text.push('…');
    }
    text
}

/// Cuts `text` to its last [`MAX_TURN_CHARS`] characters, with "…" before them, when it is
/// longer.
fn keep_end(text: &mut String) {
    let cut = text
        .char_indices()
        .rev()
        .nth(MAX_TURN_CHARS - 1)
        .map(|(at, _)| at)
        .filter(|&at| at > 0);
    if let Some(cut) = cut {
        text.replace_range(..cut, "…");
    }
}

/// The text blocks of a message's content, which is a string or a list of blocks.
fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: Value) -> Value {
        let mut message = json!({ "role": role, "timestamp": 1 });
        message["content"] = content;
        message
    }

    fn user(content: Value) -> Value {
        message("user", content)
    }

    fn assistant(content: Value) -> Value {
        message("assistant", content)
    }

    fn text(text: &str) -> Value {
        json!({ "type": "text", "text": text })
    }

    fn turns(messages: &[Value]) -> Vec<(String, String)> {
        let mut history = History::default();
        for message in messages {
            history.add(message);
        }
        history.into_turns()
    }

    fn pair(prompt: &str, reply: &str) -> (String, String) {
        (prompt.to_string(), reply.to_string())
    }

    #[test]
    fn asks_for_a_page_with_omps_limit() {
        assert_eq!(
            history_request("req_4", None),
            json!({ "id": "req_4", "type": HISTORY_COMMAND, "limit": HISTORY_PAGE_LIMIT })
        );
        assert_eq!(history_request("req_5", Some("c1"))["cursor"], json!("c1"));
    }

    #[test]
    fn reads_a_page_and_its_cursor() {
        let frame = json!({
            "id": "req_4", "type": "response", "command": HISTORY_COMMAND, "success": true,
            "data": { "messages": [user(json!("hi"))], "nextCursor": "c2", "totalMessages": 9 },
        });
        let page = read_page(&frame).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.next_cursor.as_deref(), Some("c2"));

        let last = json!({
            "type": "response", "command": HISTORY_COMMAND, "success": true,
            "data": { "messages": [], "totalMessages": 0 },
        });
        assert_eq!(read_page(&last).unwrap().next_cursor, None);
    }

    #[test]
    fn an_error_answer_says_why_without_secrets() {
        let busy = json!({
            "type": "response", "command": HISTORY_COMMAND, "success": false,
            "error": "Session is busy", "code": "session_busy",
        });
        assert_eq!(
            read_page(&busy).unwrap_err(),
            "Session is busy (session_busy)"
        );

        let key = format!("sk-ant-api03-{}", "A".repeat(40));
        let leaky = json!({ "success": false, "error": format!("bad {key}") });
        assert!(!read_page(&leaky).unwrap_err().contains(&key));

        let empty = json!({ "success": true, "data": {} });
        assert!(read_page(&empty).is_err());
    }

    #[test]
    fn pairs_prompts_with_the_text_of_their_replies() {
        let messages = [
            user(json!("Fix the build")),
            assistant(json!([
                { "type": "thinking", "thinking": "hmm" },
                text("Looking."),
                { "type": "toolCall", "id": "t1", "name": "bash", "arguments": {} },
            ])),
            json!({ "role": "toolResult", "toolCallId": "t1", "content": [text("ok")] }),
            assistant(json!([text("Fixed.")])),
            user(json!([text("Thanks"), { "type": "image", "data": "..." }])),
        ];
        assert_eq!(
            turns(&messages),
            [
                pair("Fix the build", "Looking.\n\nFixed."),
                pair("Thanks", "")
            ]
        );
    }

    #[test]
    fn a_reply_without_a_prompt_gets_an_empty_one() {
        let messages = [
            json!({ "role": "compactionSummary", "summary": "earlier work" }),
            assistant(json!("Carrying on.")),
            json!({ "role": "custom", "content": "x" }),
            assistant(json!([])),
            json!({ "no": "role" }),
        ];
        assert_eq!(turns(&messages), [pair("", "Carrying on.")]);
    }

    #[test]
    fn a_long_prompt_keeps_its_start_and_a_long_reply_its_end() {
        let long = "é".repeat(MAX_TURN_CHARS + 5);
        let messages = [
            user(json!(long.clone())),
            assistant(json!(format!("start{long}"))),
            assistant(json!("end")),
        ];
        let turns = turns(&messages);
        let (prompt, reply) = &turns[0];
        assert_eq!(prompt.chars().count(), MAX_TURN_CHARS + 1);
        assert!(prompt.ends_with("é…"), "the prompt lost its start");
        assert_eq!(reply.chars().count(), MAX_TURN_CHARS + 1);
        assert!(reply.starts_with("…é"), "the cut is not marked");
        assert!(reply.ends_with("é\n\nend"), "the reply lost its end");
    }

    #[test]
    fn a_turn_of_exactly_the_limit_is_kept_whole() {
        let full = "é".repeat(MAX_TURN_CHARS);
        let messages = [user(json!(full.clone())), assistant(json!(full.clone()))];
        assert_eq!(turns(&messages), [pair(&full, &full)]);
    }

    #[test]
    fn keeps_only_the_latest_turns() {
        let mut history = History::default();
        for n in 0..MAX_HISTORY_TURNS + 3 {
            history.add(&user(json!(format!("p{n}"))));
        }
        assert_eq!(history.dropped(), 3);
        let turns = history.into_turns();
        assert_eq!(turns.len(), MAX_HISTORY_TURNS);
        assert_eq!(turns[0].0, "p3");
    }
}
