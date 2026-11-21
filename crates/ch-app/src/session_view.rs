//! The session screen: the prompt, the run as it streams in, the approvals the agent is holding,
//! and the review panel that ends in a pull request.

mod composer;
mod header;
mod review;
mod thread;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use ch_core::{ApprovalDecision, SessionState};
use ch_ui::{Palette, Status};
use ch_vault::{SecretName, Vault};
use gpui_kit::base::input::{InputEvent, TextareaState};
use gpui_kit::{
    AppContext, ClickEvent, Context, Div, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Task,
    Window, div, px,
};

use crate::assets::Icon;
use crate::driver::{self, Driver};
use crate::launch::{self, Launched, Runner};
use crate::settings::{self, Problem, Settings};
use crate::view_model::{land_request, session_id, transcript_file_name};
use crate::widgets::{Tone, banner, icon_button};

const TICK: Duration = Duration::from_millis(50);
const CONTENT_WIDTH: f32 = 760.0;

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
    review_open: bool,
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

fn is_working(state: SessionState) -> bool {
    matches!(
        state,
        SessionState::Starting | SessionState::Running | SessionState::NeedsYou
    )
}

/// Whether a live session can be put away for a new one. A land in flight cannot be dropped,
/// and a working agent has to be stopped first, unless its runner is already gone.
fn may_start_new(closed: bool, landing: bool, stopping: bool, state: SessionState) -> bool {
    closed || (!landing && (stopping || !is_working(state)))
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
            review_open: true,
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

    /// Whether Settings still has something to fill in before a session can start.
    pub fn needs_setup(&self) -> bool {
        !self.settings.problems().is_empty()
    }

    /// Whether "New session" may put the current one away right now.
    pub fn can_start_new(&self) -> bool {
        match &self.phase {
            Phase::Empty | Phase::LaunchFailed { .. } => true,
            Phase::Launching { .. } => false,
            Phase::Live(live) => {
                let d = &live.driver;
                may_start_new(d.closed(), d.landing(), d.stopping(), d.session().state())
            }
        }
    }

    pub fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_start_new() {
            return;
        }
        self.close_session(cx);
        self.prompt.update(cx, |state, cx| state.focus(window, cx));
    }

    fn toggle_review(&mut self, cx: &mut Context<Self>) {
        self.review_open = !self.review_open;
        cx.notify();
    }

    /// The runner's output ended while the session was still open.
    fn disconnected(&self) -> bool {
        let Phase::Live(live) = &self.phase else {
            return false;
        };
        live.driver.closed()
            && !matches!(
                live.driver.session().state(),
                SessionState::Landed | SessionState::Failed
            )
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

    /// The result of the last action, with a button to put it away.
    fn message_banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let p = self.palette;
        let shown = match self.message.clone()? {
            Ok(text) => banner(p, p.status(Status::Done), "Done", text),
            Err(text) => banner(p, p.status(Status::Error), "Something needs fixing", text),
        };
        Some(
            div()
                .relative()
                .child(shown.pr_10())
                .child(div().absolute().top_2().right_2().child(
                    icon_button(p, "dismiss", Icon::X, "Dismiss", Tone::Ghost, true).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.message = None;
                            cx.notify();
                        }),
                    ),
                )),
        )
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

impl Render for SessionView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = match &self.phase {
            Phase::Live(live) if self.review_open => Some(self.review_panel(live, cx)),
            _ => None,
        };
        let main = div()
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
                            .px_6()
                            .py_6()
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
                    .px_6()
                    .pt_2()
                    .pb_4()
                    .children(self.message_banner(cx))
                    .children(self.approvals(cx))
                    .child(self.composer(cx)),
            );
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(main)
            .children(panel)
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

    #[test]
    fn a_working_agent_has_to_stop_before_a_new_session() {
        assert!(!may_start_new(false, false, false, SessionState::Running));
        assert!(!may_start_new(false, false, false, SessionState::NeedsYou));
        assert!(may_start_new(false, false, true, SessionState::Running));
        assert!(may_start_new(
            false,
            false,
            false,
            SessionState::NeedsReview
        ));
        assert!(may_start_new(false, false, false, SessionState::Landed));
    }

    #[test]
    fn a_land_in_flight_keeps_the_session_unless_the_runner_is_gone() {
        assert!(!may_start_new(
            false,
            true,
            false,
            SessionState::NeedsReview
        ));
        assert!(may_start_new(true, true, false, SessionState::NeedsReview));
        assert!(may_start_new(true, false, false, SessionState::Running));
    }
}
