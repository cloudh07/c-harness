//! The session screen: the prompt, the run as it streams in, the approvals omp is holding, and
//! the review steps that end in a pull request.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use ch_core::{ApprovalDecision, SessionState};
use ch_ui::{Palette, Status};
use ch_vault::{SecretName, Vault};
use gpui_kit::base::input::{InputEvent, TextareaState};
use gpui_kit::base::{SelectableText, Textarea};
use gpui_kit::{
    AppContext, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, ScrollHandle, Stateful, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, div, px, rgb,
};

use crate::approval_card::approval_card;
use crate::driver::{self, Driver};
use crate::launch::{self, Launched, Runner};
use crate::panels;
use crate::settings::{self, Problem, Settings};
use crate::view_model::{land_request, last_chars, session_id, state_label, transcript_file_name};
use crate::widgets::{Tone, banner, button, card, muted};

const TICK: Duration = Duration::from_millis(50);
const APPROVALS_HEIGHT: f32 = 360.0;
const CONTENT_WIDTH: f32 = 860.0;

/// Asks the shell to show the settings screen.
pub struct OpenSettings;

enum Phase {
    Empty,
    Launching {
        session: String,
        prompt: String,
        rx: Receiver<Result<Launched, String>>,
    },
    Live(Box<Live>),
    LaunchFailed {
        prompt: String,
        message: String,
    },
}

struct Live {
    session: String,
    driver: Driver,
    runner: Option<Runner>,
    version: String,
    worktree: Option<PathBuf>,
    first_prompt: String,
    /// Earlier prompts and the full reply each one got.
    turns: Vec<(String, String)>,
    prompt: String,
}

pub struct SessionView {
    palette: Palette,
    settings: Settings,
    phase: Phase,
    prompt: Entity<TextareaState>,
    message: Option<Result<String, String>>,
    scroll: ScrollHandle,
    _ticker: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OpenSettings> for SessionView {}

fn list(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|p| p.message.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn status_for(state: SessionState) -> Status {
    match state {
        SessionState::Idle => Status::Idle,
        SessionState::Starting | SessionState::Running => Status::Running,
        SessionState::NeedsYou => Status::NeedsYou,
        SessionState::NeedsReview => Status::Review,
        SessionState::Landed => Status::Done,
        SessionState::Failed => Status::Error,
    }
}

impl SessionView {
    pub fn new(
        settings: Settings,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Describe the change you want")
                .auto_grow(2, 8)
                .submit_on_enter(true)
        });
        let subscriptions = vec![cx.subscribe_in(&prompt, window, Self::on_prompt_event)];
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, Self::tick).is_err() {
                    break;
                }
            }
        });
        Self {
            palette,
            settings,
            phase: Phase::Empty,
            prompt,
            message: None,
            scroll: ScrollHandle::new(),
            _ticker: ticker,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_palette(&mut self, palette: Palette, cx: &mut Context<Self>) {
        self.palette = palette;
        cx.notify();
    }

    pub fn set_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.settings = settings;
        self.message = None;
        cx.notify();
    }

    /// What the inbox counts this session as, or `None` when there is no session.
    pub fn state(&self) -> Option<SessionState> {
        match &self.phase {
            Phase::Empty => None,
            Phase::Launching { .. } => Some(SessionState::Starting),
            Phase::Live(live) => Some(live.driver.session().state()),
            Phase::LaunchFailed { .. } => Some(SessionState::Failed),
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let changed = match &mut self.phase {
            Phase::Launching { .. } => self.poll_launch(),
            Phase::Live(live) => live.driver.pump(),
            Phase::Empty | Phase::LaunchFailed { .. } => false,
        };
        if changed {
            cx.notify();
        }
    }

    fn poll_launch(&mut self) -> bool {
        let Phase::Launching { rx, .. } = &self.phase else {
            return false;
        };
        let result = match rx.try_recv() {
            Err(TryRecvError::Empty) => return false,
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => {
                Err("The launch stopped without saying why.".to_string())
            }
        };
        let Phase::Launching {
            session, prompt, ..
        } = std::mem::replace(&mut self.phase, Phase::Empty)
        else {
            return false;
        };
        self.phase = match result {
            Ok(launched) => self.go_live(session, prompt, launched),
            Err(message) => Phase::LaunchFailed { prompt, message },
        };
        true
    }

    fn go_live(&self, session: String, prompt: String, launched: Launched) -> Phase {
        let Launched {
            link,
            runner,
            runner_version,
            env,
        } = launched;
        let mut driver = Driver::new(link, driver::epoch_ms);
        if !driver.start(&session, self.settings.model_args(), env, prompt.clone()) {
            runner.retire(false);
            return Phase::LaunchFailed {
                prompt,
                message: "The runner exited before the session could start.".to_string(),
            };
        }
        Phase::Live(Box::new(Live {
            worktree: runner.worktree(),
            session,
            driver,
            runner: Some(runner),
            version: runner_version,
            first_prompt: prompt.clone(),
            turns: Vec::new(),
            prompt,
        }))
    }

    fn on_prompt_event(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::PressEnter { shift: false, .. } = event {
            self.submit(window, cx);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.prompt.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let accepted = if let Phase::Live(live) = &mut self.phase {
            let previous = live.driver.session().reply().to_string();
            let sent = live.driver.prompt(&text);
            if sent {
                let earlier = std::mem::replace(&mut live.prompt, text);
                live.turns.push((earlier, previous));
            }
            sent
        } else {
            self.launch(text)
        };
        if accepted {
            self.prompt
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    fn launch(&mut self, prompt: String) -> bool {
        if matches!(self.phase, Phase::Launching { .. } | Phase::Live(_)) {
            return false;
        }
        let problems = self.settings.problems();
        if !problems.is_empty() {
            self.message = Some(Err(format!("Finish Settings first. {}", list(&problems))));
            return false;
        }
        let session = session_id(driver::epoch_ms());
        let rx = launch::spawn(self.settings.clone(), session.clone());
        self.message = None;
        self.phase = Phase::Launching {
            session,
            prompt,
            rx,
        };
        true
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if let Phase::LaunchFailed { prompt, .. } = &self.phase {
            let prompt = prompt.clone();
            self.launch(prompt);
            cx.notify();
        }
    }

    pub fn answer(&mut self, id: &str, decision: ApprovalDecision, cx: &mut Context<Self>) {
        if let Phase::Live(live) = &mut self.phase
            && let Err(message) = live.driver.answer(id, decision)
        {
            self.message = Some(Err(message));
        }
        cx.notify();
    }

    fn verify(&mut self, cx: &mut Context<Self>) {
        let command = self.settings.verify.trim().to_string();
        if let Phase::Live(live) = &mut self.phase
            && !command.is_empty()
            && live.driver.can_review()
            && !live.driver.verify(&command)
        {
            self.message = Some(Err(
                "The runner is gone, so the check did not run.".to_string()
            ));
        }
        cx.notify();
    }

    fn diff(&mut self, cx: &mut Context<Self>) {
        if let Phase::Live(live) = &mut self.phase
            && live.driver.can_review()
            && !live.driver.diff()
        {
            self.message = Some(Err("The runner is gone, so no diff came back.".to_string()));
        }
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Phase::Live(live) = &mut self.phase
            && !live.driver.stop()
        {
            self.message = Some(Err(
                "The runner is gone, so there is nothing to stop.".to_string()
            ));
        }
        cx.notify();
    }

    fn land(&mut self, cx: &mut Context<Self>) {
        self.message = self.try_land().err().map(Err);
        cx.notify();
    }

    fn try_land(&mut self) -> Result<(), String> {
        let Phase::Live(live) = &mut self.phase else {
            return Ok(());
        };
        if !live.driver.can_land() {
            return Ok(());
        }
        let problems = self.settings.land_problems();
        if !problems.is_empty() {
            return Err(format!(
                "Finish the Landing settings first. {}",
                list(&problems)
            ));
        }
        let token = Vault::os()
            .get(&SecretName::github())
            .map_err(|e| format!("Could not read the GitHub token from the keychain: {e}"))?
            .ok_or_else(|| "Store a GitHub token in Settings before landing.".to_string())?;
        let request = land_request(
            &self.settings,
            &live.session,
            &live.first_prompt,
            token.expose().to_string(),
            live.driver.session().evidence(),
        );
        if live.driver.land(request) {
            Ok(())
        } else {
            Err("The runner is gone, so the land was not sent.".to_string())
        }
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        if let Phase::Live(live) = &self.phase {
            self.message = Some(write_transcript(&live.session, &live.driver.transcript()));
        }
        cx.notify();
    }

    fn close_session(&mut self, cx: &mut Context<Self>) {
        if let Phase::Live(live) = std::mem::replace(&mut self.phase, Phase::Empty) {
            let Live {
                mut driver, runner, ..
            } = *live;
            let landed = driver.session().state() == SessionState::Landed;
            driver.close();
            if let Some(runner) = runner {
                runner.retire(landed);
            }
        }
        self.message = None;
        cx.notify();
    }
}

fn write_transcript(session: &str, text: &str) -> Result<String, String> {
    let dir = settings::transcripts_dir()
        .ok_or_else(|| "This system has no data folder to save the transcript in.".to_string())?;
    let path = dir.join(transcript_file_name(session));
    std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|e| format!("Could not save the transcript: {e}"))?;
    Ok(format!("Transcript saved to {}", path.display()))
}

impl SessionView {
    fn header(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let state = self.state();
        let (label, status) = state.map_or(("No session", Status::Idle), |s| {
            (state_label(s), status_for(s))
        });
        let detail = match &self.phase {
            Phase::Live(live) => {
                let mut parts = vec![live.session.clone(), live.version.clone()];
                if let Some(path) = &live.worktree {
                    parts.push(path.display().to_string());
                }
                parts.join("  ·  ")
            }
            Phase::Launching { session, .. } => format!("{session}  ·  checking the runner"),
            Phase::Empty | Phase::LaunchFailed { .. } => String::new(),
        };
        let live = matches!(self.phase, Phase::Live(_));
        let closable = matches!(self.phase, Phase::Live(_) | Phase::LaunchFailed { .. });
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(p.border))
            .child(div().size_2().rounded_full().bg(rgb(status.color())))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(label))
                    .children((!detail.is_empty()).then(|| muted(p, detail))),
            )
            .child(
                button(p, "export", "Export transcript", Tone::Plain, live)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.export(cx))),
            )
            .child(
                button(p, "new-session", "New session", Tone::Plain, closable)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_session(cx))),
            )
    }

    fn review_bar(&self, live: &Live, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let driver = &live.driver;
        let state = driver.session().state();
        let review = driver.can_review();
        let has_check = !self.settings.verify.trim().is_empty();
        let running = matches!(
            state,
            SessionState::Starting | SessionState::Running | SessionState::NeedsYou
        );
        let can_stop = running && !driver.stopping() && !driver.closed();
        let busy = if driver.landing() {
            Some("Landing. omp is stopped first, then the runner commits and pushes.")
        } else if driver.stopping() {
            Some("Stopping omp.")
        } else {
            None
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                button(p, "verify", "Run checks", Tone::Plain, review && has_check)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.verify(cx))),
            )
            .child(
                button(p, "diff", "Show diff", Tone::Plain, review)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.diff(cx))),
            )
            .child(
                button(
                    p,
                    "land",
                    "Land and open PR",
                    Tone::Primary,
                    driver.can_land(),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.land(cx))),
            )
            .child(
                button(p, "stop", "Stop", Tone::Danger, can_stop)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx))),
            )
            .children(
                (!has_check).then(|| muted(p, "Set a check command in Settings to run checks.")),
            )
            .children(busy.map(|text| muted(p, text)))
    }

    fn turn(&self, id: String, prompt: &str, reply: &str, waiting: bool) -> Div {
        let p = self.palette;
        let (shown, clipped) = last_chars(reply, panels::SHOWN_CHARS);
        let body = if shown.is_empty() {
            muted(
                p,
                if waiting {
                    "Waiting for omp."
                } else {
                    "No reply."
                },
            )
        } else {
            div()
                .text_sm()
                .child(SelectableText::new(id, shown.to_string()))
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .self_end()
                    .max_w(px(CONTENT_WIDTH * 0.8))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(p.elevated))
                    .text_sm()
                    .child(prompt.to_string()),
            )
            .children(clipped.then(|| muted(p, "Only the end of this reply is shown.")))
            .child(body)
    }

    fn live_body(&self, live: &Live, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let session = live.driver.session();
        let waiting = matches!(
            session.state(),
            SessionState::Starting | SessionState::Running
        );
        div()
            .flex()
            .flex_col()
            .gap_4()
            .children(
                live.turns.iter().enumerate().map(|(i, (prompt, reply))| {
                    self.turn(format!("turn-{i}"), prompt, reply, false)
                }),
            )
            .child(self.turn(
                "turn-now".to_string(),
                &live.prompt,
                session.reply(),
                waiting,
            ))
            .children(panels::failure(p, session.failure()))
            .children(panels::warnings(p, session.warnings()))
            .children(panels::notices(p, session.notices()))
            .children(panels::tool_calls(p, session.tool_calls()))
            .child(card(p).child(self.review_bar(live, cx)))
            .children(panels::evidence(p, session.evidence()))
            .children(panels::diff(p, session.last_diff()))
            .children(panels::landing(
                p,
                session.landing(),
                &self.settings.github_repo,
            ))
            .children(panels::log(p, live.driver.log()))
    }

    fn empty_body(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let problems = self.settings.problems();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .pt_8()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Start a session"),
            )
            .child(muted(
                p,
                "Describe one change below. omp works on it in a fresh git worktree, asks before \
                 risky tool calls, and you review the diff and checks before anything is pushed.",
            ))
            .children((!problems.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(banner(
                        p,
                        Status::NeedsYou.color(),
                        "Finish Settings first",
                        list(&problems),
                    ))
                    .child(
                        button(p, "open-settings", "Open Settings", Tone::Primary, true).on_click(
                            cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(OpenSettings)),
                        ),
                    )
            }))
    }

    fn body(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        match &self.phase {
            Phase::Empty => self.empty_body(cx),
            Phase::Launching { prompt, .. } => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(self.turn("turn-now".to_string(), prompt, "", true))
                .child(muted(p, "Checking the runner and creating the worktree.")),
            Phase::LaunchFailed { prompt, message } => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(self.turn("turn-now".to_string(), prompt, "", false))
                .child(banner(
                    p,
                    Status::Error.color(),
                    "The session could not start",
                    message.clone(),
                ))
                .child(
                    div().flex().gap_2().child(
                        button(p, "retry", "Try again", Tone::Primary, true)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.retry(cx))),
                    ),
                ),
            Phase::Live(live) => self.live_body(live, cx),
        }
    }

    fn approvals(&self, cx: &Context<Self>) -> Option<Stateful<Div>> {
        let Phase::Live(live) = &self.phase else {
            return None;
        };
        let pending = live.driver.session().pending_approvals();
        if pending.is_empty() {
            return None;
        }
        let p = self.palette;
        let cards: Vec<_> = pending.iter().map(|r| approval_card(p, r, cx)).collect();
        Some(
            div()
                .id("approvals")
                .flex()
                .flex_col()
                .gap_2()
                .max_h(px(APPROVALS_HEIGHT))
                .overflow_y_scroll()
                .children(cards),
        )
    }

    fn composer(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let can_send = match &self.phase {
            Phase::Live(live) => live.driver.can_prompt(),
            Phase::Launching { .. } => false,
            Phase::Empty | Phase::LaunchFailed { .. } => true,
        };
        let hint = match &self.phase {
            Phase::Live(live) if !live.driver.can_prompt() => {
                "omp is busy or the session has ended. A new prompt can go out once it is ready \
                 for review."
            }
            _ => "Enter sends. Shift+Enter adds a line.",
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .px_2()
                            .py(px(6.0))
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(p.border))
                            .bg(rgb(p.elevated))
                            .text_sm()
                            .child(Textarea::new(&self.prompt)),
                    )
                    .child(button(p, "send", "Send", Tone::Primary, can_send).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx)),
                    )),
            )
            .child(muted(p, hint))
    }
}

impl Render for SessionView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let message = self.message.clone().map(|m| match m {
            Ok(text) => banner(p, Status::Done.color(), "Done", text),
            Err(text) => banner(p, Status::Error.color(), "Something needs fixing", text),
        });
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.header(cx))
            .child(
                div()
                    .id("session-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .mx_auto()
                            .w_full()
                            .max_w(px(CONTENT_WIDTH))
                            .p_4()
                            .child(self.body(cx)),
                    ),
            )
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .max_w(px(CONTENT_WIDTH))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .children(message)
                    .children(self.approvals(cx))
                    .child(self.composer(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_a_distinct_meaning_in_the_header() {
        assert_eq!(status_for(SessionState::NeedsYou), Status::NeedsYou);
        assert_eq!(status_for(SessionState::NeedsReview), Status::Review);
        assert_eq!(status_for(SessionState::Failed), Status::Error);
        assert_eq!(status_for(SessionState::Landed), Status::Done);
    }

    #[test]
    fn problem_lists_read_as_sentences() {
        let problems = Settings::default().problems();
        assert!(!problems.is_empty(), "{problems:?}");
        let text = list(&problems);
        assert!(text.contains("repository"), "{text}");
    }
}
