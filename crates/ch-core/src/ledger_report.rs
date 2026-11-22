//! The token ledger read back: totals by model, omp's own totals beside the ledger's, and what
//! a verified task cost.
//!
//! A ledger file holds one JSON line per model turn and a few marks, all numbers. Never what the
//! model said, and never a command or its output.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::STATS_COMMAND;
use crate::usage::{UsageRow, UsageTotals};

/// Cost sums that differ by less than this, in US dollars, are the same sum added in a
/// different order.
pub const COST_TOLERANCE_USD: f64 = 1e-6;
/// Mismatches the report lists before it only counts the rest.
const SHOWN_MISMATCHES: usize = 10;
const UNKNOWN_MODEL: &str = "unknown model";

/// omp's own count for the whole session, from a `get_session_stats` reply.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OmpTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
}

impl OmpTotals {
    /// Reads a successful `get_session_stats` reply. Any other frame gives `None`.
    pub fn from_stats(frame: &Value) -> Option<Self> {
        let is_stats = frame.get("type").and_then(Value::as_str) == Some("response")
            && frame.get("command").and_then(Value::as_str) == Some(STATS_COMMAND)
            && frame.get("success").and_then(Value::as_bool) == Some(true);
        if !is_stats {
            return None;
        }
        let data = frame.get("data")?;
        let count = |key| data.get("tokens")?.get(key)?.as_u64();
        Some(Self {
            input: count("input")?,
            output: count("output")?,
            cache_read: count("cacheRead")?,
            cache_write: count("cacheWrite")?,
            cost_usd: data
                .get("cost")?
                .as_f64()
                .filter(|cost| cost.is_finite() && *cost >= 0.0)?,
        })
    }
}

/// A line of the ledger that is not a turn. `turn` is how many turn rows the session had when
/// the mark was made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LedgerMark {
    /// omp's totals, read after a turn ended or the context was compacted.
    OmpTotals {
        turn: u32,
        at_ms: u64,
        totals: OmpTotals,
    },
    /// A Verify run passed and its evidence was kept.
    Verified { turn: u32, at_ms: u64 },
}

impl LedgerMark {
    pub fn turn(&self) -> u32 {
        match *self {
            Self::OmpTotals { turn, .. } | Self::Verified { turn, .. } => turn,
        }
    }
}

/// One line of a ledger file.
#[derive(Debug, Clone, PartialEq)]
pub enum LedgerLine {
    Row(UsageRow),
    Mark(LedgerMark),
}

impl LedgerLine {
    /// Reads one line. A line with a `kind` is a mark and never a row; anything else that does
    /// not read is `None`.
    pub fn parse(line: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(line).ok()?;
        if value.get("kind").is_some() {
            serde_json::from_value(value).ok().map(Self::Mark)
        } else {
            serde_json::from_value(value).ok().map(Self::Row)
        }
    }
}

/// Where the ledger and omp disagree, read the same way: ledger first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gap {
    Tokens {
        kind: &'static str,
        ledger: u64,
        omp: u64,
    },
    Cost {
        ledger: f64,
        omp: f64,
    },
}

impl Gap {
    fn describe(&self) -> String {
        match *self {
            Self::Tokens { kind, ledger, omp } => format!("{kind} ledger {ledger}, omp {omp}"),
            Self::Cost { ledger, omp } => format!("cost ledger ${ledger:.6}, omp ${omp:.6}"),
        }
    }
}

/// Compares the ledger's totals with omp's. Token counts must match exactly; costs within a
/// rounding error. An unpriced turn counts as zero cost, as omp counts it.
pub fn reconcile(ledger: &UsageTotals, omp: &OmpTotals) -> Vec<Gap> {
    let tokens = [
        ("input", ledger.input, omp.input),
        ("output", ledger.output, omp.output),
        ("cache read", ledger.cache_read, omp.cache_read),
        ("cache write", ledger.cache_write, omp.cache_write),
    ];
    let mut gaps: Vec<Gap> = tokens
        .into_iter()
        .filter(|(_, ledger, omp)| ledger != omp)
        .map(|(kind, ledger, omp)| Gap::Tokens { kind, ledger, omp })
        .collect();
    let cost = ledger.cost_usd.unwrap_or(0.0);
    if (cost - omp.cost_usd).abs() > COST_TOLERANCE_USD {
        gaps.push(Gap::Cost {
            ledger: cost,
            omp: omp.cost_usd,
        });
    }
    gaps
}

/// One check of the ledger against omp, after `turn` rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub turn: u32,
    pub gaps: Vec<Gap>,
}

/// The ledger of one session, read back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionLedger {
    /// One row per turn, in file order. A turn a retry wrote twice is kept once.
    pub rows: Vec<UsageRow>,
    pub marks: Vec<LedgerMark>,
}

impl SessionLedger {
    /// Reads a ledger file. Lines that do not read are skipped.
    pub fn parse(text: &str) -> Self {
        let mut ledger = Self::default();
        let mut seen = HashSet::new();
        for line in text.lines().filter_map(LedgerLine::parse) {
            match line {
                LedgerLine::Row(row) if seen.insert(row.turn) => ledger.rows.push(row),
                LedgerLine::Row(_) => {}
                LedgerLine::Mark(mark) => ledger.marks.push(mark),
            }
        }
        ledger
    }

    pub fn totals(&self) -> UsageTotals {
        UsageTotals::of(&self.rows)
    }

    /// Totals per `provider/model`, in name order.
    pub fn by_model(&self) -> BTreeMap<String, UsageTotals> {
        let mut groups: BTreeMap<String, Vec<&UsageRow>> = BTreeMap::new();
        for row in &self.rows {
            groups.entry(model_name(row)).or_default().push(row);
        }
        groups
            .into_iter()
            .map(|(model, rows)| (model, UsageTotals::of(rows)))
            .collect()
    }

    /// What the session had cost when its last passing Verify was kept: the cost of one
    /// verified task. `None` when no Verify passed.
    pub fn verified_cost(&self) -> Option<UsageTotals> {
        let last = self
            .marks
            .iter()
            .filter_map(|mark| match mark {
                LedgerMark::Verified { turn, .. } => Some(*turn),
                LedgerMark::OmpTotals { .. } => None,
            })
            .next_back()?;
        Some(self.totals_to(last))
    }

    /// Every omp reading against the ledger rows up to the same turn.
    pub fn readings(&self) -> Vec<Reading> {
        self.marks
            .iter()
            .filter_map(|mark| match mark {
                LedgerMark::OmpTotals { turn, totals, .. } => Some(Reading {
                    turn: *turn,
                    gaps: reconcile(&self.totals_to(*turn), totals),
                }),
                LedgerMark::Verified { .. } => None,
            })
            .collect()
    }

    fn totals_to(&self, turn: u32) -> UsageTotals {
        UsageTotals::of(self.rows.iter().filter(|row| row.turn <= turn))
    }
}

fn model_name(row: &UsageRow) -> String {
    match (&row.provider, &row.model) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        (None, Some(model)) => model.clone(),
        (_, None) => UNKNOWN_MODEL.to_string(),
    }
}

/// US dollars to the cent, and `<$0.01` for less than a cent.
pub fn dollars(cost: f64) -> String {
    if cost < 0.01 {
        "<$0.01".into()
    } else {
        format!("${cost:.2}")
    }
}

/// The baseline report over the ledgers of `sessions`, named by session id. With no ledgers it
/// says the report was not run, and gives no numbers.
pub fn report(sessions: &[(String, SessionLedger)]) -> String {
    if sessions.iter().all(|(_, ledger)| ledger.rows.is_empty()) {
        return "Token ledger report: not run. No ledger file holds a turn yet.\n".to_string();
    }
    let all: Vec<UsageRow> = sessions
        .iter()
        .flat_map(|(_, ledger)| ledger.rows.iter().cloned())
        .collect();
    let mut lines = vec!["Token ledger report".to_string(), String::new()];
    lines.extend(overview(sessions.len(), &UsageTotals::of(&all)));
    lines.extend(models(&SessionLedger {
        rows: all,
        marks: Vec::new(),
    }));
    lines.extend(verified(sessions));
    lines.extend(checks(sessions));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn overview(sessions: usize, totals: &UsageTotals) -> Vec<String> {
    let share = totals
        .cache_percent()
        .map_or("not reported".to_string(), |p| format!("{p}%"));
    vec![
        format!(
            "Sessions: {sessions}. Turns: {}, {} without a cost.",
            totals.turns, totals.unpriced_turns
        ),
        format!(
            "Tokens: input {}, output {}, cache read {}, cache write {}.",
            totals.input, totals.output, totals.cache_read, totals.cache_write
        ),
        format!("Cache share (cache read / (input + cache read)): {share}."),
        format!("Cost: {}.", cost_text(totals)),
        String::new(),
    ]
}

fn models(all: &SessionLedger) -> Vec<String> {
    let mut lines = vec!["By model".to_string()];
    for (model, totals) in all.by_model() {
        lines.push(format!(
            "  {model}: {}, input {}, output {}, cache read {}, cache write {}, {}.",
            counted(totals.turns, "turn"),
            totals.input,
            totals.output,
            totals.cache_read,
            totals.cache_write,
            cost_text(&totals)
        ));
    }
    lines.push(String::new());
    lines
}

fn verified(sessions: &[(String, SessionLedger)]) -> Vec<String> {
    let costs: Vec<UsageTotals> = sessions
        .iter()
        .filter_map(|(_, ledger)| ledger.verified_cost())
        .collect();
    let mut lines = vec!["Cost per verified task".to_string()];
    if costs.is_empty() {
        lines.push("  No session has a passing Verify in its ledger yet.".to_string());
    } else {
        let priced: f64 = costs.iter().filter_map(|t| t.cost_usd).sum();
        let unpriced = costs.iter().filter(|t| t.unpriced_turns > 0).count();
        let count = u32::try_from(costs.len()).unwrap_or(u32::MAX);
        lines.push(format!(
            "  Verified tasks: {count}. Average {}, total {}.",
            dollars(priced / f64::from(count)),
            dollars(priced)
        ));
        if unpriced > 0 {
            lines.push(format!(
                "  Of these, {unpriced} had turns without a cost, so the real cost is higher."
            ));
        }
    }
    lines.push(String::new());
    lines
}

fn checks(sessions: &[(String, SessionLedger)]) -> Vec<String> {
    let readings: Vec<(&str, Reading)> = sessions
        .iter()
        .flat_map(|(id, ledger)| ledger.readings().into_iter().map(move |r| (id.as_str(), r)))
        .collect();
    let mut lines = vec!["Ledger against omp's totals".to_string()];
    if readings.is_empty() {
        lines.push(
            "  Not checked: no ledger holds omp's totals yet. Ledgers written before this check have none."
                .to_string(),
        );
        return lines;
    }
    let off: Vec<&(&str, Reading)> = readings
        .iter()
        .filter(|(_, r)| !r.gaps.is_empty())
        .collect();
    lines.push(format!(
        "  Readings: {}, matching: {}.",
        readings.len(),
        readings.len() - off.len()
    ));
    for (id, reading) in off.iter().take(SHOWN_MISMATCHES) {
        for gap in &reading.gaps {
            lines.push(format!(
                "  {id} after turn {}: {}. Unexplained.",
                reading.turn,
                gap.describe()
            ));
        }
    }
    if off.len() > SHOWN_MISMATCHES {
        lines.push(format!(
            "  {} more readings do not match.",
            off.len() - SHOWN_MISMATCHES
        ));
    }
    lines
}

fn cost_text(totals: &UsageTotals) -> String {
    match (totals.cost_usd, totals.unpriced_turns) {
        (None, _) => "cost unknown".to_string(),
        (Some(cost), 0) => dollars(cost),
        (Some(cost), n) => format!(
            "{} plus {} without a cost",
            dollars(cost),
            counted(n, "turn")
        ),
    }
}

fn counted(n: u32, noun: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {noun}{plural}")
}

#[cfg(test)]
mod tests;
