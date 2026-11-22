//! The settings screen. Plain settings go to a JSON file; the model key and the GitHub token
//! go to the system keychain and are never shown again once stored.

use ch_ui::{Palette, Status};
use ch_vault::{SecretName, Vault};
use gpui_kit::base::Input;
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    App, AppContext, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px, rgb,
};

use crate::assets::provider_logo;
use crate::settings::{self, ApprovalMode, Location, Settings, ThemePref};
use crate::view_model::{cap_text, parse_cap, parse_port};
use crate::widgets::{Tone, banner, button, field, muted, section};

const PROVIDER_LOGO: f32 = 16.0;

/// Emitted after the settings file and any new secrets are written.
pub struct SettingsSaved(pub Settings);

struct Fields {
    repo: Entity<InputState>,
    runner: Entity<InputState>,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_workdir: Entity<InputState>,
    ssh_runner: Entity<InputState>,
    provider: Entity<InputState>,
    key_env: Entity<InputState>,
    model: Entity<InputState>,
    verify: Entity<InputState>,
    author_name: Entity<InputState>,
    author_email: Entity<InputState>,
    github_repo: Entity<InputState>,
    base_branch: Entity<InputState>,
    cost_cap: Entity<InputState>,
}

impl Fields {
    fn new(s: &Settings, window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let (host, port, workdir, remote_runner) = match &s.location {
            Location::Ssh {
                host,
                port,
                remote_workdir,
                remote_runner,
            } => (
                host.clone(),
                port.map(|p| p.to_string()).unwrap_or_default(),
                remote_workdir.clone(),
                remote_runner.clone(),
            ),
            Location::Local => (
                String::new(),
                String::new(),
                String::new(),
                ch_runtime::DEFAULT_REMOTE_RUNNER.to_string(),
            ),
        };
        let mut text = |value: String, placeholder: &'static str| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        Self {
            repo: text(s.repo.clone(), "/path/to/your/repo"),
            runner: text(s.runner.clone(), "ch-runner"),
            ssh_host: text(host, "devbox or user@host"),
            ssh_port: text(port, "22"),
            ssh_workdir: text(workdir, "/home/you/repo"),
            ssh_runner: text(remote_runner, "ch-runner"),
            provider: text(s.provider.clone(), "openai"),
            key_env: text(s.key_env.clone(), "OPENAI_API_KEY"),
            model: text(s.model.clone(), "omp default"),
            verify: text(s.verify.clone(), "cargo test"),
            author_name: text(s.author_name.clone(), "Your Name"),
            author_email: text(s.author_email.clone(), "you@example.com"),
            github_repo: text(s.github_repo.clone(), "owner/name"),
            base_branch: text(s.base_branch.clone(), "main"),
            cost_cap: text(s.cost_cap_cents.map(cap_text).unwrap_or_default(), "No cap"),
        }
    }

    fn all(&self) -> [&Entity<InputState>; 15] {
        [
            &self.repo,
            &self.runner,
            &self.ssh_host,
            &self.ssh_port,
            &self.ssh_workdir,
            &self.ssh_runner,
            &self.provider,
            &self.key_env,
            &self.model,
            &self.verify,
            &self.author_name,
            &self.author_email,
            &self.github_repo,
            &self.base_branch,
            &self.cost_cap,
        ]
    }
}

fn secret_input(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder(placeholder)
    })
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

/// Whether a secret is in the keychain. Only presence is read here; the value is dropped.
fn stored(name: Result<SecretName, String>) -> Result<bool, String> {
    let name = name?;
    Vault::os()
        .get(&name)
        .map(|secret| secret.is_some())
        .map_err(|e| format!("Could not read the keychain: {e}"))
}

fn model_secret(provider: &str) -> Result<SecretName, String> {
    SecretName::model(provider.trim()).map_err(|e| e.to_string())
}

pub struct SettingsView {
    palette: Palette,
    fields: Fields,
    model_key: Entity<InputState>,
    github_token: Entity<InputState>,
    use_ssh: bool,
    approval_mode: ApprovalMode,
    saved: Settings,
    load_error: Option<String>,
    status: Option<Result<String, String>>,
    key_stored: Result<bool, String>,
    token_stored: Result<bool, String>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsSaved> for SettingsView {}

impl SettingsView {
    pub fn new(
        saved: Settings,
        load_error: Option<String>,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fields = Fields::new(&saved, window, cx);
        let subscriptions = fields
            .all()
            .into_iter()
            .map(|input| cx.subscribe_in(input, window, Self::on_input))
            .collect();
        let mut view = Self {
            palette,
            model_key: secret_input("Paste a new key to store it", window, cx),
            github_token: secret_input("Paste a new token to store it", window, cx),
            use_ssh: matches!(saved.location, Location::Ssh { .. }),
            approval_mode: saved.approval_mode,
            fields,
            saved,
            load_error,
            status: None,
            key_stored: Ok(false),
            token_stored: Ok(false),
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        view.refresh_stored();
        view
    }

    pub fn set_palette(&mut self, palette: Palette, cx: &mut Context<Self>) {
        self.palette = palette;
        cx.notify();
    }

    /// Saves the theme on its own, so picking one never saves half-edited fields with it. A
    /// file that failed to load is left alone; the theme then lasts until the app closes.
    pub fn set_theme(&mut self, theme: ThemePref, cx: &mut Context<Self>) {
        self.saved.theme = theme;
        if self.load_error.is_some() {
            return;
        }
        let Some(path) = settings::default_path() else {
            return;
        };
        if let Err(e) = settings::save(&path, &self.saved) {
            self.status = Some(Err(format!("Could not save the theme: {e}")));
            cx.notify();
        }
    }

    pub fn theme(&self) -> ThemePref {
        self.saved.theme
    }

    fn refresh_stored(&mut self) {
        self.key_stored = stored(model_secret(&self.saved.provider));
        self.token_stored = stored(Ok(SecretName::github()));
    }

    fn on_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        if *input == self.fields.provider {
            let provider = value(&self.fields.provider, cx);
            let current = value(&self.fields.key_env, cx);
            if let Some(env) = settings::key_env_after_provider_change(&provider, &current) {
                self.fields
                    .key_env
                    .update(cx, |state, cx| state.set_value(env, window, cx));
            }
        }
        self.status = None;
        cx.notify();
    }

    fn draft(&self, cx: &App) -> Result<Settings, String> {
        let f = &self.fields;
        let location = if self.use_ssh {
            let remote_runner = value(&f.ssh_runner, cx);
            Location::Ssh {
                host: value(&f.ssh_host, cx),
                port: parse_port(&value(&f.ssh_port, cx))?,
                remote_workdir: value(&f.ssh_workdir, cx),
                remote_runner: if remote_runner.is_empty() {
                    ch_runtime::DEFAULT_REMOTE_RUNNER.to_string()
                } else {
                    remote_runner
                },
            }
        } else {
            Location::Local
        };
        Ok(Settings {
            repo: value(&f.repo, cx),
            location,
            runner: value(&f.runner, cx),
            provider: value(&f.provider, cx),
            key_env: value(&f.key_env, cx),
            model: value(&f.model, cx),
            approval_mode: self.approval_mode,
            verify: value(&f.verify, cx),
            author_name: value(&f.author_name, cx),
            author_email: value(&f.author_email, cx),
            github_repo: value(&f.github_repo, cx),
            base_branch: value(&f.base_branch, cx),
            theme: self.saved.theme,
            cost_cap_cents: parse_cap(&value(&f.cost_cap, cx))?,
        })
    }

    fn store_secrets(&self, settings: &Settings, cx: &App) -> Result<(), String> {
        let vault = Vault::os();
        let key = value(&self.model_key, cx);
        if !key.is_empty() {
            let name = model_secret(&settings.provider)
                .map_err(|e| format!("The model key was not stored: {e}"))?;
            vault
                .set(&name, &key)
                .map_err(|e| format!("Could not store the model key in the keychain: {e}"))?;
        }
        let token = value(&self.github_token, cx);
        if !token.is_empty() {
            vault
                .set(&SecretName::github(), &token)
                .map_err(|e| format!("Could not store the GitHub token in the keychain: {e}"))?;
        }
        Ok(())
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status = Some(self.try_save(window, cx));
        cx.notify();
    }

    fn try_save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        let settings = self.draft(cx)?;
        let path = settings::default_path()
            .ok_or("This system has no config folder, so settings cannot be saved.")?;
        settings::save(&path, &settings).map_err(|e| format!("Could not save settings: {e}"))?;
        self.saved = settings.clone();
        self.load_error = None;
        self.store_secrets(&settings, cx)?;
        // New entities rather than clearing, so undo in the old field cannot bring a secret back.
        self.model_key = secret_input("Paste a new key to store it", window, cx);
        self.github_token = secret_input("Paste a new token to store it", window, cx);
        self.refresh_stored();
        cx.emit(SettingsSaved(settings));
        Ok("Saved.".to_string())
    }

    fn remove_model_key(&mut self, cx: &mut Context<Self>) {
        let result = model_secret(&self.saved.provider).and_then(|name| {
            Vault::os()
                .delete(&name)
                .map_err(|e| format!("Could not remove the model key: {e}"))
        });
        self.after_remove(result, "Model key removed.", cx);
    }

    fn remove_github_token(&mut self, cx: &mut Context<Self>) {
        let result = Vault::os()
            .delete(&SecretName::github())
            .map_err(|e| format!("Could not remove the GitHub token: {e}"));
        self.after_remove(result, "GitHub token removed.", cx);
    }

    fn after_remove(&mut self, result: Result<(), String>, done: &str, cx: &mut Context<Self>) {
        self.status = Some(result.map(|()| done.to_string()));
        self.refresh_stored();
        cx.notify();
    }

    fn location_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let tone = |on: bool| if on { Tone::Primary } else { Tone::Plain };
        div()
            .flex()
            .gap_2()
            .child(
                button(p, "where-local", "This computer", tone(!self.use_ssh), true).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.use_ssh = false;
                        cx.notify();
                    }),
                ),
            )
            .child(
                button(p, "where-ssh", "SSH host", tone(self.use_ssh), true).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.use_ssh = true;
                        cx.notify();
                    },
                )),
            )
    }

    fn approval_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let choice = |id, label, mode: ApprovalMode| {
            let tone = if self.approval_mode == mode {
                Tone::Primary
            } else {
                Tone::Plain
            };
            button(p, id, label, tone, true).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    this.approval_mode = mode;
                    this.status = None;
                    cx.notify();
                },
            ))
        };
        let buttons = div()
            .flex()
            .gap_2()
            .child(choice(
                "approve-write",
                "Ask before edits and commands",
                ApprovalMode::Write,
            ))
            .child(choice(
                "approve-always",
                "Ask before every tool",
                ApprovalMode::AlwaysAsk,
            ));
        div().flex().flex_col().gap_3().child(buttons).child(muted(
            p,
            "Applies to new sessions and wins over a repository's own omp config. Approval is \
             not a sandbox: an approved command runs with your full access.",
        ))
    }

    fn stored_row(
        &self,
        id: &'static str,
        state: &Result<bool, String>,
        what: String,
        remove: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.palette;
        let row = div().flex().items_center().gap_2();
        match state {
            Ok(true) => row
                .child(
                    div()
                        .size_2()
                        .rounded_full()
                        .bg(rgb(p.status(Status::Done))),
                )
                .child(div().flex_1().text_xs().child(what))
                .child(
                    button(p, id, "Remove", Tone::Danger, true)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| remove(this, cx))),
                ),
            Ok(false) => row.child(muted(p, "Nothing stored yet.")),
            Err(message) => row.child(
                div()
                    .text_xs()
                    .text_color(rgb(p.status(Status::Error)))
                    .child(message.clone()),
            ),
        }
    }

    fn problem_list(&self, draft: &Result<Settings, String>) -> impl IntoElement {
        let p = self.palette;
        let (start, land): (Vec<String>, Vec<String>) = match draft {
            Ok(s) => (
                s.problems().into_iter().map(|x| x.message).collect(),
                s.land_problems().into_iter().map(|x| x.message).collect(),
            ),
            Err(message) => (vec![message.clone()], Vec::new()),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when(!start.is_empty(), |d| {
                d.child(banner(
                    p,
                    p.status(Status::NeedsYou),
                    "Before a session can start",
                    start.join("\n"),
                ))
            })
            .when(!land.is_empty(), |d| {
                d.child(banner(
                    p,
                    p.status(Status::Idle),
                    "Before a session can land",
                    land.join("\n"),
                ))
            })
    }

    fn footer(&self, draft: &Result<Settings, String>, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let unsaved = match draft {
            Ok(s) => *s != self.saved,
            Err(_) => true,
        };
        let status = self.status.clone().map(|status| {
            let (color, text) = match status {
                Ok(text) => (p.status(Status::Done), text),
                Err(text) => (p.status(Status::Error), text),
            };
            div().text_sm().text_color(rgb(color)).child(text)
        });
        div()
            .flex()
            .items_center()
            .gap_3()
            .pt_2()
            .child(
                button(p, "save-settings", "Save", Tone::Primary, true).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)),
                ),
            )
            .when(unsaved && self.status.is_none(), |d| {
                d.child(muted(p, "Unsaved changes"))
            })
            .children(status)
    }

    /// Provider, key and model, and what a session may spend.
    fn model_fields(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette;
        let f = &self.fields;
        let provider = f.provider.read(cx).value().to_string();
        let key_what = format!(
            "A key for `{}` is stored in the system keychain.",
            self.saved.provider
        );
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(field(
                p,
                "Provider",
                Some("omp's provider id, such as openai, anthropic or openrouter."),
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(provider_logo(&provider, px(PROVIDER_LOGO), p.text))
                    .child(div().flex_1().min_w_0().child(Input::new(&f.provider))),
            ))
            .child(field(
                p,
                "Key variable",
                Some("The variable omp reads the key from. It follows the provider until you type your own."),
                Input::new(&f.key_env),
            ))
            .child(field(p, "Model", Some("Leave empty for omp's default."), Input::new(&f.model)))
            .child(field(
                p,
                "API key",
                Some("Goes to the system keychain, never to the settings file. Leave empty to keep what is stored."),
                Input::new(&self.model_key),
            ))
            .child(self.stored_row("remove-model-key", &self.key_stored, key_what, Self::remove_model_key, cx))
            .child(field(
                p,
                "Cost cap per session",
                Some("In dollars. Once a session's priced turns reach it, the run is aborted. Turns omp cannot price are not counted. Leave empty for no cap."),
                Input::new(&f.cost_cap),
            ))
    }

    fn where_fields(&self) -> Div {
        let p = self.palette;
        let f = &self.fields;
        let column = div().flex().flex_col().gap_3();
        if !self.use_ssh {
            return column.child(field(
                p,
                "Repository",
                Some("A local git repository. Each session works in its own worktree cut from it."),
                Input::new(&f.repo),
            ));
        }
        column
            .child(field(
                p,
                "SSH host",
                Some("A host from your SSH config, or user@host."),
                Input::new(&f.ssh_host),
            ))
            .child(field(
                p,
                "Port",
                Some("Leave empty for the SSH default."),
                Input::new(&f.ssh_port),
            ))
            .child(field(
                p,
                "Checkout on the host",
                Some("A git checkout that already exists on that host."),
                Input::new(&f.ssh_workdir),
            ))
            .child(field(
                p,
                "ch-runner on the host",
                None,
                Input::new(&f.ssh_runner),
            ))
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let draft = self.draft(cx);
        let f = &self.fields;
        let path = settings::default_path().map_or_else(
            || "No config folder on this system.".to_string(),
            |path| format!("Saved to {}", path.display()),
        );
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .max_w(px(640.0))
            .mx_auto()
            .p_6()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Settings"))
            .child(muted(p, path))
            .children(self.load_error.as_ref().map(|err| {
                banner(
                    p,
                    p.status(Status::Error),
                    "Your settings file could not be read",
                    format!("{err}. Saving will replace it."),
                )
            }))
            .child(section("Where sessions run"))
            .child(self.location_toggle(cx))
            .child(self.where_fields())
            .child(field(
                p,
                "ch-runner",
                Some("On this computer: a name on PATH or a full path. Also used to reach SSH hosts."),
                Input::new(&f.runner),
            ))
            .child(section("Model"))
            .child(self.model_fields(cx))
            .child(section("Approvals"))
            .child(self.approval_toggle(cx))
            .child(section("Checks"))
            .child(field(
                p,
                "Verify command",
                Some("Runs in the session's checkout when you press Verify. Leave empty to hide the button."),
                Input::new(&f.verify),
            ))
            .child(section("Landing"))
            .child(field(p, "Commit author name", None, Input::new(&f.author_name)))
            .child(field(p, "Commit author email", None, Input::new(&f.author_email)))
            .child(field(p, "GitHub repository", Some("owner/name. Pull requests open here."), Input::new(&f.github_repo)))
            .child(field(p, "Base branch", Some("The branch pull requests target."), Input::new(&f.base_branch)))
            .child(field(
                p,
                "GitHub token",
                Some("Needs permission to push branches and open pull requests in that repository. Goes to the system keychain."),
                Input::new(&self.github_token),
            ))
            .child(self.stored_row(
                "remove-github-token",
                &self.token_stored,
                "A GitHub token is stored in the system keychain.".to_string(),
                Self::remove_github_token,
                cx,
            ))
            .child(self.problem_list(&draft))
            .child(self.footer(&draft, cx));
        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_provider_cannot_name_a_secret() {
        assert!(model_secret("openai").is_ok());
        assert!(model_secret("not a provider").is_err());
    }
}
