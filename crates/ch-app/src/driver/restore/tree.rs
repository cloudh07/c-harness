//! Reading omp's conversation tree: its answers, the labels this app writes, and where each
//! checkpoint's run ended.

use serde_json::Value;

use super::{Asked, Rewind};

pub(super) const LABEL_PREFIX: &str = "c-harness:turn:";

/// omp's answer to one request.
pub(super) enum Reply<'a> {
    Done(&'a Value),
    /// omp does not know the command it was sent.
    Unknown(&'static str),
    Failed {
        error: String,
        code: Option<String>,
    },
}

impl Reply<'_> {
    pub(super) fn error(self) -> String {
        match self {
            Self::Done(_) => "it cancelled the move".to_owned(),
            Self::Unknown(command) => format!("it does not know the {command} command"),
            Self::Failed { error, .. } => error,
        }
    }
}

/// omp's answer to `asked`, or `None` if `frame` answers something else.
pub(super) fn reply_to<'a>(frame: &'a Value, asked: &Asked) -> Option<Reply<'a>> {
    let answers = frame.get("type").and_then(Value::as_str) == Some("response")
        && frame.get("id").and_then(Value::as_str) == Some(asked.id.as_str());
    if !answers {
        return None;
    }
    if frame.get("success").and_then(Value::as_bool) == Some(true) {
        return Some(Reply::Done(frame.get("data").unwrap_or(&Value::Null)));
    }
    let error = frame
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("no reason given");
    if error.to_ascii_lowercase().starts_with("unknown command") {
        return Some(Reply::Unknown(asked.command));
    }
    Some(Reply::Failed {
        error: error.to_owned(),
        code: frame.get("code").and_then(Value::as_str).map(str::to_owned),
    })
}

/// The label that marks an entry as the end of these checkpoints.
pub(super) fn label_for(turns: &[u32]) -> String {
    let mut turns = turns.to_vec();
    turns.sort_unstable();
    turns.dedup();
    let list: Vec<String> = turns.iter().map(u32::to_string).collect();
    format!("{LABEL_PREFIX}{}", list.join(","))
}

/// The checkpoints a label marks, or `None` for a label this app did not write.
pub(super) fn turns_in(label: &str) -> Option<Vec<u32>> {
    label
        .strip_prefix(LABEL_PREFIX)?
        .split(',')
        .map(|turn| turn.parse().ok())
        .collect()
}

impl Rewind {
    /// Folds in entries omp listed in the order it appended them, and returns its leaf.
    pub(super) fn read(&mut self, data: &Value) -> Option<String> {
        let entries = data.get("entries").and_then(Value::as_array);
        for entry in entries.into_iter().flatten() {
            let Some(id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            self.cursor = Some(id.to_owned());
            match entry.get("type").and_then(Value::as_str) {
                Some("label") => {
                    let Some(target) = entry.get("targetId").and_then(Value::as_str) else {
                        continue;
                    };
                    // One label per entry: a newer one, ours or not, replaces the older.
                    let turns = entry
                        .get("label")
                        .and_then(Value::as_str)
                        .and_then(turns_in);
                    self.place(target, &turns.unwrap_or_default());
                }
                Some("message")
                    if entry.pointer("/message/role").and_then(Value::as_str) == Some("user") =>
                {
                    self.first_user.get_or_insert_with(|| id.to_owned());
                    let parent = entry.get("parentId").and_then(Value::as_str);
                    self.prompts
                        .insert(id.to_owned(), parent.map(str::to_owned));
                }
                _ => {}
            }
        }
        data.get("leafId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    pub(super) fn place(&mut self, entry: &str, turns: &[u32]) {
        self.places.retain(|_, at| at != entry);
        for turn in turns {
            self.places.insert(*turn, entry.to_owned());
        }
    }

    pub(super) fn turns_at(&self, entry: &str) -> Vec<u32> {
        let mut turns: Vec<u32> = self
            .places
            .iter()
            .filter(|(_, at)| *at == entry)
            .map(|(turn, _)| *turn)
            .collect();
        turns.sort_unstable();
        turns
    }

    /// Where omp's leaf is after a move to `entry`. A move to a prompt lands on the entry before
    /// it, and `None` is an empty conversation.
    pub(super) fn landing(&self, entry: &str) -> Option<String> {
        match self.prompts.get(entry) {
            Some(parent) => parent.clone(),
            None => Some(entry.to_owned()),
        }
    }

    /// Drops what was read, when omp no longer knows the last entry read.
    pub(super) fn forget(&mut self) {
        self.places.clear();
        self.first_user = None;
        self.prompts.clear();
        self.cursor = None;
    }
}
