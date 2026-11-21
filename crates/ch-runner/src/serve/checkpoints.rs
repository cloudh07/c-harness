//! Checkpoints, restores and diffs over a range of them.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use ch_protocol::runner::{Checkpoint, MAX_OUTPUT_TAIL_BYTES, MAX_PATCH_BYTES, RunnerFrame};

use super::{CHECKPOINT, DIFF, OmpState, RESTORE, Session, error};
use crate::checkpoint::Store;
use crate::git::{Base, Diff, Git};

const BUDGETS: (usize, usize) = (MAX_OUTPUT_TAIL_BYTES, MAX_PATCH_BYTES);
const NO_CHECKPOINTS: &str = "this working tree keeps no checkpoints";

impl Session {
    /// Turns checkpoints on when the working tree is a worktree locked for `session`, and
    /// tells the client which ones it already has.
    pub(super) fn open_checkpoints(&mut self, session: &str) {
        match Store::open(&self.git, session) {
            Ok(Some((store, checkpoints))) => {
                self.checkpoints = Some(Arc::new(Mutex::new(store)));
                self.out.frame(&RunnerFrame::Checkpoints { checkpoints });
            }
            Ok(None) => {}
            Err(e) => self.out.frame(&RunnerFrame::Notice {
                kind: "checkpoints_off".into(),
                message: format!("Checkpoints are off for this session: {e}"),
            }),
        }
    }

    /// Saves the working tree as the next checkpoint. Land does not wait for it.
    pub(super) fn checkpoint(&mut self, id: String) {
        let Some(base) = self.base.clone() else {
            self.out.frame(&error(
                Some(id),
                CHECKPOINT,
                "send ch_start before ch_checkpoint",
            ));
            return;
        };
        let Some(store) = self.checkpoints.clone() else {
            self.out.frame(&error(Some(id), CHECKPOINT, NO_CHECKPOINTS));
            return;
        };
        let git = self.git.clone();
        let out = self.out.clone();
        let index = self.dir.unique_path("checkpoint-index");
        self.side_workers.push(thread::spawn(move || {
            // The lock keeps saves in turn order, and the frames with them.
            let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
            let frame = match store.save(&git, &base, &index) {
                Ok(checkpoint) => RunnerFrame::Checkpointed { id, checkpoint },
                Err(e) => error(Some(id), CHECKPOINT, e),
            };
            out.frame(&frame);
        }));
    }

    /// Saves the working tree, then puts back checkpoint `turn`. Land waits for it.
    pub(super) fn restore(&mut self, id: String, turn: u32) {
        self.workers.retain(|worker| !worker.is_finished());
        let refusal = if self.base.is_none() {
            Some("send ch_start before ch_restore")
        } else if matches!(self.omp, OmpState::Lost) {
            Some("omp could not be ended, so the working tree may still be changing")
        } else if !self.workers.is_empty() {
            Some("refused while a check, diff or restore is still running: wait for its result")
        } else {
            None
        };
        let (None, Some(base), Some(store)) =
            (refusal, self.base.clone(), self.checkpoints.clone())
        else {
            self.out
                .frame(&error(Some(id), RESTORE, refusal.unwrap_or(NO_CHECKPOINTS)));
            return;
        };
        let git = self.git.clone();
        let out = self.out.clone();
        let index = self.dir.unique_path("restore-index");
        self.workers.push(thread::spawn(move || {
            let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
            let saved = |checkpoint: &Checkpoint| {
                out.frame(&RunnerFrame::Checkpointed {
                    id: id.clone(),
                    checkpoint: checkpoint.clone(),
                });
            };
            let frame = match store.restore(&git, &base, &index, turn, saved) {
                Ok(()) => RunnerFrame::Restored { id, turn },
                Err(e) => error(Some(id), RESTORE, e),
            };
            out.frame(&frame);
        }));
    }

    /// Reports the changes from `from` to `to`, by default from the base to the working tree.
    pub(super) fn diff(&mut self, id: String, from: Option<u32>, to: Option<u32>) {
        let Some(base) = self.base.clone() else {
            self.out
                .frame(&error(Some(id), DIFF, "send ch_start before ch_diff"));
            return;
        };
        let git = self.git.clone();
        let out = self.out.clone();
        let store = self.checkpoints.clone();
        let index = self.dir.unique_path("diff-index");
        self.workers.push(thread::spawn(move || {
            // Waits for a save or restore in flight, so the diff sees the tree it left.
            let store = store
                .as_ref()
                .map(|store| store.lock().unwrap_or_else(PoisonError::into_inner));
            let range = Range {
                git: &git,
                base: &base,
                store: store.as_deref(),
            };
            let frame = match range.diff(from, to, &index) {
                Ok(diff) => RunnerFrame::DiffResult {
                    id,
                    base: base.id.clone(),
                    from,
                    to,
                    stat: diff.stat,
                    patch: diff.patch,
                    truncated: diff.truncated,
                },
                Err(e) => error(Some(id), DIFF, e),
            };
            out.frame(&frame);
        }));
    }
}

struct Range<'a> {
    git: &'a Git,
    base: &'a Base,
    store: Option<&'a Store>,
}

impl Range<'_> {
    fn diff(&self, from: Option<u32>, to: Option<u32>, index: &Path) -> Result<Diff, String> {
        let from = from.unwrap_or(0);
        match to {
            None if from == 0 => self.git.diff(self.base, index, BUDGETS.0, BUDGETS.1),
            None => {
                let since = self.resolve(from)?;
                self.git.diff_since(self.base, &since, index, BUDGETS)
            }
            Some(to) => {
                let (from, to) = (self.resolve(from)?, self.resolve(to)?);
                self.git.diff_between(&from, &to, BUDGETS)
            }
        }
    }

    /// 0 is the base; any other turn needs checkpoints on.
    fn resolve(&self, turn: u32) -> Result<String, String> {
        match self.store {
            _ if turn == 0 => Ok(self.base.id.clone()),
            Some(store) => store.resolve(self.git, self.base, turn),
            None => Err(NO_CHECKPOINTS.to_owned()),
        }
    }
}
