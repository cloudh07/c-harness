//! The local sessions earlier runs left behind, listed on the empty screen so they can be kept
//! or discarded.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use ch_runtime::{Leftover, LeftoverKind};
use ch_ui::{Palette, Status};
use gpui_kit::{ClickEvent, Context, Div, FontWeight, ParentElement, Styled, div, rgb};

use super::SessionView;
use super::resume::can_resume;
use crate::launch;
use crate::widgets::{Tone, banner, button, card, eyebrow, muted};

type Discarding = Receiver<Result<Option<PathBuf>, String>>;

struct Row {
    leftover: Leftover,
    /// Discard was pressed once; the next press deletes.
    confirming: bool,
    discarding: Option<Discarding>,
    error: Option<String>,
}

/// The scan for leftovers and what the user has done with each one.
pub(super) struct Leftovers {
    scan: Option<Receiver<Result<Vec<Leftover>, String>>>,
    rows: Vec<Row>,
    scan_error: Option<String>,
    /// A folder a discard left in place because git no longer listed it.
    kept_folder: Option<PathBuf>,
}

/// What a leftover is, in a sentence.
fn describe(kind: LeftoverKind) -> &'static str {
    match kind {
        LeftoverKind::Live => "Its worktree is still there, with any changes the agent made.",
        LeftoverKind::FolderGone => "Git still lists its worktree, but the folder was deleted.",
        LeftoverKind::NoWorktree => "Only its record is left; git no longer lists the worktree.",
        LeftoverKind::RepoUnreadable => {
            "Its repository was moved or deleted, so only its record can be discarded."
        }
        LeftoverKind::NoRecord => "A C-Harness worktree with no session record.",
        LeftoverKind::Damaged => "Its session record cannot be read.",
    }
}

/// What a second press of Discard deletes.
fn warning(session: &str) -> String {
    format!(
        "This deletes the worktree with any uncommitted changes, and the session's checkpoints. \
         The branch ch/{session} stays."
    )
}

impl Leftovers {
    pub(super) fn scanning(scan: Receiver<Result<Vec<Leftover>, String>>) -> Self {
        Self {
            scan: Some(scan),
            rows: Vec::new(),
            scan_error: None,
            kept_folder: None,
        }
    }

    /// Takes in what finished since the last call. True when something changed.
    pub(super) fn poll(&mut self) -> bool {
        let scanned = self.poll_scan();
        let discarded = self.poll_discards();
        scanned || discarded
    }

    fn poll_scan(&mut self) -> bool {
        let Some(rx) = &self.scan else {
            return false;
        };
        let result = match rx.try_recv() {
            Err(TryRecvError::Empty) => return false,
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => {
                Err("The search stopped without saying why.".to_string())
            }
        };
        self.scan = None;
        match result {
            Ok(found) => {
                self.rows = found
                    .into_iter()
                    .map(|leftover| Row {
                        leftover,
                        confirming: false,
                        discarding: None,
                        error: None,
                    })
                    .collect();
                self.scan_error = None;
            }
            Err(message) => self.scan_error = Some(message),
        }
        true
    }

    fn poll_discards(&mut self) -> bool {
        let mut changed = false;
        let mut kept = None;
        self.rows.retain_mut(|row| {
            let Some(rx) = &row.discarding else {
                return true;
            };
            let result = match rx.try_recv() {
                Err(TryRecvError::Empty) => return true,
                Ok(result) => result,
                Err(TryRecvError::Disconnected) => {
                    Err("The discard stopped without saying why.".to_string())
                }
            };
            changed = true;
            row.discarding = None;
            match result {
                Ok(folder) => {
                    kept = kept.take().or(folder);
                    false
                }
                Err(message) => {
                    row.error = Some(message);
                    true
                }
            }
        });
        if kept.is_some() {
            self.kept_folder = kept;
        }
        changed
    }

    fn row_mut(&mut self, session: &str) -> Option<&mut Row> {
        self.rows
            .iter_mut()
            .find(|row| row.leftover.session == session)
    }

    /// Hides `session` until the next start.
    pub(super) fn keep(&mut self, session: &str) {
        self.rows.retain(|row| row.leftover.session != session);
    }

    /// Shows why `session` could not start again.
    pub(super) fn failed(&mut self, session: &str, message: String) {
        if let Some(row) = self.row_mut(session) {
            row.confirming = false;
            row.error = Some(message);
        }
    }

    /// The first press asks for a second; the second returns true, meaning delete now.
    fn press_discard(&mut self, session: &str) -> bool {
        let Some(row) = self.row_mut(session) else {
            return false;
        };
        if row.discarding.is_some() {
            return false;
        }
        row.error = None;
        if row.confirming {
            row.confirming = false;
            return true;
        }
        row.confirming = true;
        false
    }

    fn cancel(&mut self, session: &str) {
        if let Some(row) = self.row_mut(session) {
            row.confirming = false;
        }
    }

    /// The repository the scan found `session`'s worktree in.
    fn seen_in(&self, session: &str) -> Option<PathBuf> {
        let row = self
            .rows
            .iter()
            .find(|row| row.leftover.session == session)?;
        row.leftover.entry.as_ref().map(|entry| entry.repo.clone())
    }

    fn discarding(&mut self, session: &str, rx: Discarding) {
        if let Some(row) = self.row_mut(session) {
            row.discarding = Some(rx);
        }
    }
}

impl SessionView {
    fn keep_leftover(&mut self, session: &str, cx: &mut Context<Self>) {
        self.leftovers.keep(session);
        cx.notify();
    }

    fn discard_leftover(&mut self, session: &str, cx: &mut Context<Self>) {
        if self.leftovers.press_discard(session) {
            let seen_in = self.leftovers.seen_in(session);
            let rx = launch::discard(&self.settings, session.to_string(), seen_in);
            self.leftovers.discarding(session, rx);
        }
        cx.notify();
    }

    fn cancel_discard(&mut self, session: &str, cx: &mut Context<Self>) {
        self.leftovers.cancel(session);
        cx.notify();
    }

    /// The leftovers card, or `None` when there is nothing to show.
    pub(super) fn leftovers_card(&self, cx: &mut Context<Self>) -> Option<Div> {
        let p = self.palette;
        let state = &self.leftovers;
        let error = state.scan_error.as_ref().map(|message| {
            banner(
                p,
                p.status(Status::Error),
                "Could not look for earlier sessions",
                message.clone(),
            )
        });
        let kept = state.kept_folder.as_ref().map(|folder| {
            banner(
                p,
                p.status(Status::NeedsYou),
                "A folder was left in place",
                format!(
                    "Git no longer listed {} as a worktree, so C-Harness did not delete it. \
                     Delete it yourself if you do not need it.",
                    folder.display()
                ),
            )
        });
        let list = (!state.rows.is_empty()).then(|| {
            card(p)
                .gap_3()
                .child(eyebrow(p, "Left from earlier sessions"))
                .child(muted(
                    p,
                    "An earlier run did not clean these up. Resume starts the agent again in \
                     its worktree. Keep hides one until the next start.",
                ))
                .children(state.rows.iter().map(|row| {
                    let resumable = can_resume(row.leftover.kind, &self.settings.location);
                    leftover_row(p, row, resumable, cx)
                }))
        });
        if error.is_none() && kept.is_none() && list.is_none() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .children(error)
                .children(kept)
                .children(list),
        )
    }
}

fn leftover_row(p: Palette, row: &Row, resumable: bool, cx: &mut Context<SessionView>) -> Div {
    let session = &row.leftover.session;
    let path = row
        .leftover
        .worktree()
        .map(|path| muted(p, path.display().to_string()));
    let note = row
        .confirming
        .then(|| muted(p, warning(session)))
        .or_else(|| {
            row.error.as_ref().map(|e| {
                div()
                    .text_xs()
                    .text_color(rgb(p.status(Status::Error)))
                    .child(e.clone())
            })
        });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(rgb(p.border))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(session.clone()),
                        )
                        .child(muted(p, describe(row.leftover.kind)))
                        .children(path),
                )
                .child(row_actions(p, row, resumable, cx)),
        )
        .children(note)
}

/// Resume when it can, then Keep (or Cancel while confirming) and Discard.
fn row_actions(p: Palette, row: &Row, resumable: bool, cx: &mut Context<SessionView>) -> Div {
    let session = row.leftover.session.clone();
    let busy = row.discarding.is_some();
    let resume = (resumable && !busy && !row.confirming).then(|| {
        let id = session.clone();
        button(
            p,
            format!("leftover-resume-{session}"),
            "Resume",
            Tone::Primary,
            true,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.resume_leftover(&id, cx);
        }))
    });
    let first = if row.confirming {
        let id = session.clone();
        button(
            p,
            format!("leftover-cancel-{session}"),
            "Cancel",
            Tone::Plain,
            true,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.cancel_discard(&id, cx);
        }))
    } else {
        let id = session.clone();
        button(
            p,
            format!("leftover-keep-{session}"),
            "Keep",
            Tone::Plain,
            !busy,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.keep_leftover(&id, cx)))
    };
    let (label, tone) = match (busy, row.confirming) {
        (true, _) => ("Discarding", Tone::Plain),
        (false, true) => ("Discard for good", Tone::Danger),
        (false, false) => ("Discard", Tone::Plain),
    };
    let id = session.clone();
    let second = button(p, format!("leftover-discard-{session}"), label, tone, !busy).on_click(
        cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.discard_leftover(&id, cx);
        }),
    );
    div()
        .flex()
        .flex_none()
        .gap_2()
        .children(resume)
        .child(first)
        .child(second)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn leftover(session: &str, kind: LeftoverKind) -> Leftover {
        Leftover {
            session: session.to_string(),
            kind,
            record: None,
            entry: None,
        }
    }

    fn found(sessions: &[&str]) -> Leftovers {
        let (tx, rx) = mpsc::channel();
        let mut state = Leftovers::scanning(rx);
        let leftovers = sessions
            .iter()
            .map(|s| leftover(s, LeftoverKind::Live))
            .collect();
        tx.send(Ok(leftovers)).unwrap();
        assert!(state.poll());
        state
    }

    fn sessions(state: &Leftovers) -> Vec<&str> {
        state
            .rows
            .iter()
            .map(|row| row.leftover.session.as_str())
            .collect()
    }

    #[test]
    fn each_kind_reads_differently() {
        let kinds = [
            LeftoverKind::Live,
            LeftoverKind::FolderGone,
            LeftoverKind::NoWorktree,
            LeftoverKind::RepoUnreadable,
            LeftoverKind::NoRecord,
            LeftoverKind::Damaged,
        ];
        let mut texts: Vec<_> = kinds.into_iter().map(describe).collect();
        texts.sort_unstable();
        texts.dedup();
        assert_eq!(texts.len(), kinds.len());
        assert!(warning("s1").contains("ch/s1"));
    }

    #[test]
    fn nothing_shows_until_the_scan_answers() {
        let (tx, rx) = mpsc::channel();
        let mut state = Leftovers::scanning(rx);
        assert!(!state.poll());
        tx.send(Err("git is missing".to_string())).unwrap();
        assert!(state.poll());
        assert_eq!(state.scan_error.as_deref(), Some("git is missing"));
        assert!(!state.poll());
    }

    #[test]
    fn keep_hides_only_that_session() {
        let mut state = found(&["s1", "s2"]);
        state.keep("s1");
        assert_eq!(sessions(&state), ["s2"]);
    }

    #[test]
    fn a_failed_resume_shows_its_reason_on_the_row() {
        let mut state = found(&["s1", "s2"]);
        assert!(!state.press_discard("s1"));
        state.failed("s1", "omp is missing".to_string());
        state.failed("missing", "ignored".to_string());
        assert_eq!(sessions(&state), ["s1", "s2"]);
        assert_eq!(state.rows[0].error.as_deref(), Some("omp is missing"));
        assert!(!state.rows[0].confirming);
        assert!(state.rows[1].error.is_none());
    }

    #[test]
    fn discard_needs_a_second_press_and_cancel_resets_it() {
        let mut state = found(&["s1"]);
        assert!(!state.press_discard("s1"));
        state.cancel("s1");
        assert!(!state.press_discard("s1"));
        assert!(state.press_discard("s1"));
        assert!(!state.press_discard("missing"));
    }

    #[test]
    fn a_finished_discard_drops_the_row_and_names_a_kept_folder() {
        let mut state = found(&["s1", "s2", "s3"]);
        let mut senders = Vec::new();
        for session in ["s1", "s2", "s3"] {
            let (tx, rx) = mpsc::channel();
            state.discarding(session, rx);
            senders.push(tx);
        }
        assert!(!state.press_discard("s1"), "a busy row ignores presses");

        senders[0].send(Ok(None)).unwrap();
        senders[1]
            .send(Ok(Some(PathBuf::from("/kept/s2"))))
            .unwrap();
        senders[2].send(Err("locked".to_string())).unwrap();
        assert!(state.poll());

        assert_eq!(sessions(&state), ["s3"]);
        assert_eq!(state.rows[0].error.as_deref(), Some("locked"));
        assert!(state.rows[0].discarding.is_none());
        assert_eq!(state.kept_folder, Some(PathBuf::from("/kept/s2")));
    }
}
