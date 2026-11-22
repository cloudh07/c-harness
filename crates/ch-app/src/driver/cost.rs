//! The session's cost cap, and the marks the usage ledger keeps beside its turn rows.

use ch_core::{LedgerMark, OmpTotals, SessionState, UsageTotals};
use ch_protocol::runner::{RunnerFrame, RunnerLine};
use serde_json::json;

use super::{Driver, OmpState};

/// Whether the priced turns cost `cents` or more. Turns without a cost add nothing. The cap is
/// turned into dollars rather than the cost into cents: `0.29 * 100.0` is just under 29.
fn cap_reached(totals: &UsageTotals, cents: u32) -> bool {
    totals
        .cost_usd
        .is_some_and(|cost| cost >= f64::from(cents) / 100.0)
}

impl Driver {
    /// Holds the session at `cap` (in cents): once the priced turns reach it, the running prompt
    /// is aborted and no new one goes out until the cap is raised or cleared. Returns the cap
    /// the first time it is reached, so the window can say why the run stopped.
    pub fn enforce_cost_cap(&mut self, cap: Option<u32>) -> Option<u32> {
        let reached = cap.filter(|&cents| cap_reached(&self.usage.totals(), cents));
        if reached.is_none() || reached == self.cap_hit {
            self.cap_hit = reached;
            return None;
        }
        self.cap_hit = reached;
        self.abort_run();
        reached
    }

    /// The cap the session is held at, if it reached one.
    pub fn cap_hit(&self) -> Option<u32> {
        self.cap_hit
    }

    /// omp's own totals and the passing checks, in the order they arrived.
    pub fn marks(&self) -> &[LedgerMark] {
        &self.marks
    }

    fn abort_run(&mut self) {
        let working = matches!(
            self.session.state(),
            SessionState::Running | SessionState::NeedsYou
        );
        if self.closed || self.omp != OmpState::Running || !working {
            return;
        }
        let frame = json!({ "type": "abort", "id": self.next_id() });
        if self.send_omp(&frame, None) {
            self.note("Aborted the run: the session reached its cost cap.");
        }
    }

    /// Notes omp's totals and each passing check against the turns counted so far. A session
    /// with no turns yet has nothing to check omp against.
    pub(super) fn mark_ledger(&mut self, line: &RunnerLine) {
        let turn = u32::try_from(self.usage.rows().len()).unwrap_or(u32::MAX);
        let at_ms = (self.clock)();
        let mark = match line {
            RunnerLine::Omp(frame) if turn > 0 => {
                OmpTotals::from_stats(frame).map(|totals| LedgerMark::OmpTotals {
                    turn,
                    at_ms,
                    totals,
                })
            }
            RunnerLine::Runner(RunnerFrame::Evidence(evidence)) if evidence.passed() => {
                Some(LedgerMark::Verified { turn, at_ms })
            }
            _ => None,
        };
        self.marks.extend(mark);
    }
}
