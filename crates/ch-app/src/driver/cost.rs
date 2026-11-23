//! The session's cost cap, and the marks the usage ledger keeps beside its turn rows.

use ch_core::{COST_TOLERANCE_USD, LedgerMark, OmpTotals, SessionState, UsageTotals};
use ch_protocol::runner::{RunnerFrame, RunnerLine};
use serde_json::json;

use super::{Driver, OmpState};

/// A cost cap the session just reached, and whether an abort went out to a running prompt.
/// Nothing checks omp's reply, so a sent abort is not proof the run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapReached {
    pub cents: u32,
    pub abort_sent: bool,
}

/// Whether the priced turns cost `cents` or more. Turns without a cost add nothing. Summed
/// costs carry float error (0.7 + 0.1 is just under 0.8), so a sum within the tolerance of the
/// cap counts as reaching it.
fn cap_reached(totals: &UsageTotals, cents: u32) -> bool {
    totals
        .cost_usd
        .is_some_and(|cost| cost + COST_TOLERANCE_USD >= f64::from(cents) / 100.0)
}

impl Driver {
    /// Holds the session at `cap` (in cents): once the priced turns reach it, an abort goes to
    /// the running prompt and no new one goes out until the cap is raised or cleared. Reports
    /// the cap the first time it is reached, so the window can say what happened.
    pub fn enforce_cost_cap(&mut self, cap: Option<u32>) -> Option<CapReached> {
        let reached = cap.filter(|&cents| cap_reached(&self.usage.totals(), cents));
        let fresh = reached.filter(|_| reached != self.cap_hit);
        self.cap_hit = reached;
        let cents = fresh?;
        let abort_sent = self.abort_run();
        Some(CapReached { cents, abort_sent })
    }

    /// The cap the session is held at, if it reached one.
    pub fn cap_hit(&self) -> Option<u32> {
        self.cap_hit
    }

    /// The cap, when it is the only thing that stops a prompt: raising it lets one go out.
    pub fn held_at_cap(&self) -> Option<u32> {
        self.cap_hit.filter(|_| self.ready_for_prompt())
    }

    /// omp's own totals and the passing checks, in the order they arrived.
    pub fn marks(&self) -> &[LedgerMark] {
        &self.marks
    }

    /// Sends an abort to a prompt that is running. A sent stop or a queued land already ends the
    /// run, so neither gets a second frame.
    fn abort_run(&mut self) -> bool {
        let working = matches!(
            self.session.state(),
            SessionState::Running | SessionState::NeedsYou
        );
        if self.closed || self.omp != OmpState::Running || !working {
            return false;
        }
        let frame = json!({ "type": "abort", "id": self.next_id() });
        let sent = self.send_omp(&frame, None);
        if sent {
            self.note("Sent an abort: the session reached its cost cap.");
        }
        sent
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
