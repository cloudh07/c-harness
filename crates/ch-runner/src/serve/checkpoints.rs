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
        self.side_workers.retain(|worker| !worker.is_finished());
        let refusal = if self.base.is_none() {
            Some("send ch_start before ch_restore")
        } else if matches!(self.omp, OmpState::Lost) {
            Some("omp could not be ended, so the working tree may still be changing")
        } else if !self.workers.is_empty() {
            Some("refused while a check, diff or restore is still running: wait for its result")
        } else if !self.side_workers.is_empty() {
            // The store lock is not fair, so a restore could take its turn number first.
            Some("refused while a checkpoint is still being saved: wait for its result")
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{self, Write};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    use serde_json::Value;

    use super::*;
    use crate::out::Out;
    use crate::pr::CurlPrOpener;
    use crate::scratch::PrivateDir;

    #[derive(Clone, Default)]
    struct Written(Arc<Mutex<Vec<u8>>>);

    impl Write for Written {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Written {
        fn messages(&self) -> Vec<String> {
            let bytes = self.0.lock().unwrap();
            String::from_utf8_lossy(&bytes)
                .lines()
                .map(|line| {
                    let frame: Value = serde_json::from_str(line).unwrap();
                    frame["message"].as_str().unwrap_or_default().to_owned()
                })
                .collect()
        }
    }

    fn started(out: &Written, dir: &PrivateDir) -> Session {
        Session {
            out: Out::new(out.clone()),
            git: Git::new(dir.path(), dir.path().join("no-hooks")),
            dir: PrivateDir::create("ch-runner-test").unwrap(),
            omp_program: "omp".into(),
            opener: Box::new(CurlPrOpener),
            base: Some(Base {
                id: "0".repeat(40),
                is_commit: true,
            }),
            omp: OmpState::NotStarted,
            landed: HashMap::new(),
            workers: Vec::new(),
            side_workers: Vec::new(),
            checkpoints: None,
            shutdown: Arc::new(AtomicBool::new(false)),
            checked: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn a_restore_waits_for_a_checkpoint_save_in_flight() {
        let written = Written::default();
        let dir = PrivateDir::create("ch-runner-test").unwrap();
        let mut session = started(&written, &dir);
        let (release, held) = mpsc::channel::<()>();
        session.side_workers.push(thread::spawn(move || {
            let _ = held.recv();
        }));

        session.restore("r1".to_owned(), 1);
        drop(release);
        for save in session.side_workers.drain(..) {
            save.join().unwrap();
        }
        // The save is done, so the next refusal is only that this tree keeps none.
        session.restore("r2".to_owned(), 1);

        assert_eq!(
            written.messages(),
            [
                "refused while a checkpoint is still being saved: wait for its result",
                NO_CHECKPOINTS
            ]
        );
    }
}
