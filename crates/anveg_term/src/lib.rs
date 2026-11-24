//! The terminal used by Anveg Code.
//!
//! The rest of the app talks only to [`TerminalView`], [`TerminalSettings`]
//! and [`TerminalViewEvent`]. The engine behind them is gpui-cn's terminal
//! on libghostty-vt; nothing outside this crate names gpui-cn's terminal
//! types, so the engine can be swapped (for example for `alacritty_terminal`
//! when Windows support is added) by reimplementing this crate's API.
//!
//! What the engine gives us, and the build plan asks for:
//!
//! - one PTY and one parser thread per terminal; output is parsed even while
//!   a terminal is hidden, but frames are only published (and drawn) while it
//!   is visible ([`TerminalView::set_visible`]);
//! - synchronized output, bracketed paste, the kitty keyboard protocol,
//!   mouse reporting, ⌘-click on links, selection and ⌘C;
//! - stopping a program sends SIGHUP, then SIGTERM, then SIGKILL to its
//!   process group.

mod meta;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use anveg_core::files::{PathLink, path_link_at};
use anveg_core::launcher::LaunchSpec;
use gpui_cn::terminal::{
    EngineOptions, LocalTerminalOptions, Terminal, TerminalAppearance, TerminalColors,
    TerminalConfig, TerminalEvent, TerminalState, TerminalStatus, WorkingDirectory,
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Render, SharedString, Styled as _, Subscription,
    Window, WindowAppearance, canvas, div, px,
};

/// The terminal's inner padding, set explicitly so ⌘-click can map a
/// pointer position to a cell.
const PADDING: f32 = 6.0;

/// Set up the terminal engine and its UI. Call once, after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    gpui_cn::init(cx);
    cx.bind_keys(key_bindings());
}

/// Terminal actions on ⌘ keys only. gpui-cn's defaults also take Shift+↑/↓,
/// Shift+Page Up/Down and Shift+Home/End for scrolling; we leave those to
/// the program, because every non-⌘ key must reach Claude Code.
fn key_bindings() -> Vec<KeyBinding> {
    use gpui_cn::terminal::KEY_CONTEXT;
    use gpui_cn::terminal::actions::{
        Copy, DecreaseFontSize, IncreaseFontSize, Paste, ResetFontSize, ScrollPageDown,
        ScrollPageUp, ScrollToBottom, ScrollToTop, SelectAll,
    };
    let context = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-up", ScrollToTop, context),
        KeyBinding::new("cmd-down", ScrollToBottom, context),
        KeyBinding::new("cmd-pageup", ScrollPageUp, context),
        KeyBinding::new("cmd-pagedown", ScrollPageDown, context),
        KeyBinding::new("cmd-=", IncreaseFontSize, context),
        KeyBinding::new("cmd-+", IncreaseFontSize, context),
        KeyBinding::new("cmd--", DecreaseFontSize, context),
        KeyBinding::new("cmd-0", ResetFontSize, context),
    ]
}

/// User-facing terminal settings (section 6.8 of the build plan).
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalSettings {
    /// Font family.
    pub font_family: String,
    /// Font size in points.
    pub font_size: f32,
    /// Scrollback limit in lines.
    pub scrollback_lines: usize,
    /// Blink the cursor. Off saves idle CPU.
    pub cursor_blink: bool,
    /// Send Option+key as Meta (ESC + key) instead of the composed character.
    pub option_as_meta: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: "Menlo".to_owned(),
            font_size: 13.0,
            scrollback_lines: 10_000,
            cursor_blink: false,
            option_as_meta: true,
        }
    }
}

/// How a terminal's program ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    /// The exit code, when the program exited normally.
    pub code: Option<i32>,
    /// The signal's name, when a signal ended it.
    pub signal: Option<String>,
}

impl ExitInfo {
    /// Whether this was a clean exit (code 0).
    pub fn is_success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Events a [`TerminalView`] emits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalViewEvent {
    /// The program changed the terminal title.
    TitleChanged(SharedString),
    /// The program ended.
    Exited(ExitInfo),
    /// The program could not be started.
    Failed(SharedString),
    /// ⌘-click on `path[:line[:col]]` in the output. `cwd` is the shell's
    /// working directory when it reports one (OSC 7).
    OpenPath { link: PathLink, cwd: Option<PathBuf> },
}

/// Where a terminal's program is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Starting up.
    Starting,
    /// Running.
    Running,
    /// Ended.
    Exited(ExitInfo),
    /// Could not start.
    Failed(String),
}

/// A terminal running one program in a pseudo-terminal.
pub struct TerminalView {
    id: ElementId,
    state: Entity<TerminalState>,
    option_as_meta: bool,
    /// Where the terminal was last drawn.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl TerminalView {
    /// Start `spec` in a new terminal.
    pub fn spawn(
        id: impl Into<ElementId>,
        spec: &LaunchSpec,
        settings: &TerminalSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let options = local_options(spec, settings);
        let config = TerminalConfig::default()
            .with_appearance(appearance(settings))
            .with_colors(colors_for(window.appearance()));
        tracing::info!(program = ?spec.program, args = ?spec.args, cwd = ?spec.cwd, "starting terminal");
        let state = cx.new(|cx| TerminalState::local(options, config, window, cx));
        let events = cx.subscribe(&state, |this, state, event, cx| {
            this.forward(&state, event, cx);
        });
        // Redraw this view when a new frame arrives.
        let frames = cx.observe(&state, |_, _, cx| cx.notify());
        // Follow the system's light or dark mode, live.
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            let colors = colors_for(window.appearance());
            this.state
                .update(cx, |state, cx| state.set_colors(colors, cx));
        });
        Self {
            id: id.into(),
            state,
            option_as_meta: settings.option_as_meta,
            bounds: Rc::default(),
            _subscriptions: [events, frames, appearance],
        }
    }

    fn forward(
        &mut self,
        state: &Entity<TerminalState>,
        event: &TerminalEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::TitleChanged => {
                let title = state.read(cx).title().clone();
                cx.emit(TerminalViewEvent::TitleChanged(title));
            }
            TerminalEvent::Exited(status) => {
                let info = ExitInfo {
                    code: status.code(),
                    signal: status.signal().map(str::to_owned),
                };
                tracing::info!(?info, "terminal program exited");
                cx.emit(TerminalViewEvent::Exited(info));
            }
            TerminalEvent::Failed(message) => {
                tracing::error!(%message, "terminal failed");
                cx.emit(TerminalViewEvent::Failed(message.clone()));
            }
            _ => {}
        }
        cx.notify();
    }

    /// Where the program is in its life.
    pub fn run_state(&self, cx: &App) -> RunState {
        match self.state.read(cx).status() {
            TerminalStatus::Starting => RunState::Starting,
            TerminalStatus::Live => RunState::Running,
            TerminalStatus::Exited(status) => RunState::Exited(ExitInfo {
                code: status.code(),
                signal: status.signal().map(str::to_owned),
            }),
            TerminalStatus::Failed(message) => RunState::Failed(message.clone()),
            other => RunState::Failed(format!("unknown terminal status: {other:?}")),
        }
    }

    /// Whether the program is starting or running.
    pub fn is_running(&self, cx: &App) -> bool {
        matches!(self.run_state(cx), RunState::Starting | RunState::Running)
    }

    /// The process in the terminal's foreground (the program itself while
    /// it runs), for memory sampling.
    pub fn foreground_pid(&self, cx: &App) -> Option<u32> {
        self.state.read(cx).foreground().map(|p| p.pid())
    }

    /// Whether a program other than the shell is in the foreground (a
    /// command is still running).
    pub fn is_busy(&self, cx: &App) -> bool {
        self.state
            .read(cx)
            .foreground()
            .is_some_and(|p| !p.is_shell())
    }

    /// Apply new font settings to a running terminal.
    pub fn apply_settings(&mut self, settings: &TerminalSettings, cx: &mut Context<Self>) {
        self.option_as_meta = settings.option_as_meta;
        let appearance = appearance(settings);
        self.state
            .update(cx, |state, cx| state.set_appearance(appearance, cx));
    }

    /// The title the program set.
    pub fn title(&self, cx: &App) -> SharedString {
        self.state.read(cx).title().clone()
    }

    /// Show or hide the terminal. Hidden terminals keep parsing output but
    /// publish no frames, so they cost no drawing.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_visible(visible, cx));
    }

    /// Send text to the program as if typed (not as a paste).
    pub fn send_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        self.state.update(cx, |state, cx| state.send_text(text, cx));
    }

    /// Type `text` as a paste (bracketed when the program asks for it), so
    /// a newline in it is never taken as Enter.
    pub fn paste_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        self.state.update(cx, |state, cx| state.paste_text(text, cx));
    }

    /// Stop the program: SIGHUP, then SIGTERM, then SIGKILL to its process
    /// group. Idempotent.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.close(cx));
    }

    /// Give the terminal keyboard focus.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.state.read(cx).focus_handle().clone();
        window.focus(&handle, cx);
    }

    fn on_capture_key_down(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.option_as_meta {
            return;
        }
        let m = &event.keystroke.modifiers;
        if !m.alt || m.platform || m.control || m.function {
            return;
        }
        if let Some(bytes) = meta::meta_sequence(&event.keystroke.key, m.shift) {
            self.send_text(bytes, cx);
            cx.stop_propagation();
        }
    }
}

impl TerminalView {
    /// ⌘-click: a `path:line` under the pointer opens in the editor. URLs
    /// are left to the engine, which opens them in the browser.
    fn on_capture_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left || !event.modifiers.platform {
            return;
        }
        let Some(bounds) = self.bounds.get() else {
            tracing::debug!("terminal bounds unknown");
            return;
        };
        let state = self.state.read(cx);
        let frame = state.frame();
        let scale = window.scale_factor().max(1.0);
        let cell_w = frame.viewport.cell_width() as f32 / scale;
        let cell_h = frame.viewport.cell_height() as f32 / scale;
        if cell_w <= 0.0 || cell_h <= 0.0 {
            tracing::debug!(?bounds, "no cell size yet");
            return;
        }
        let x = f32::from(event.position.x - bounds.origin.x) - PADDING;
        let y = f32::from(event.position.y - bounds.origin.y) - PADDING;
        if x < 0.0 || y < 0.0 {
            tracing::debug!(x, y, ?bounds, "outside the grid");
            return;
        }
        let (col, row) = ((x / cell_w) as u16, (y / cell_h) as usize);
        tracing::debug!(col, row, cell_w, cell_h, "terminal cmd-click");
        let Some(row) = frame.rows.get(row) else {
            return;
        };
        // The cell's character index in the row text.
        let Some(cell) = row
            .cells
            .iter()
            .rev()
            .find(|c| !c.spacer && c.column <= col)
        else {
            return;
        };
        if cell.hyperlink.is_some() {
            return;
        }
        let start = cell.text.start.min(row.text.len());
        let Some(prefix) = row.text.get(..start) else {
            return;
        };
        let index = prefix.chars().count();
        let Some(link) = path_link_at(&row.text, index) else {
            tracing::debug!(text = %row.text, index, "no path under the pointer");
            return;
        };
        let cwd = state.cwd().map(PathBuf::from);
        cx.emit(TerminalViewEvent::OpenPath { link, cwd });
        cx.stop_propagation();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle().clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds.clone();
        div()
            .size_full()
            .relative()
            .capture_key_down(cx.listener(Self::on_capture_key_down))
            .capture_any_mouse_down(cx.listener(Self::on_capture_mouse_down))
            .child(Terminal::new(self.id.clone(), &self.state))
            .child(
                canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
    }
}

/// A light palette (the dark one is Ghostty's default).
const LIGHT_THEME: &str = "\
background = #fbfbfa
foreground = #262626
cursor-color = #3a3a3a
selection-background = #cfe3ff
selection-foreground = #1a1a1a
palette = 0=#2b2b2b
palette = 1=#c4352f
palette = 2=#2f8a3c
palette = 3=#9a6a00
palette = 4=#2a62c9
palette = 5=#a23fb0
palette = 6=#16808a
palette = 7=#bdbdbd
palette = 8=#6e6e6e
palette = 9=#de4a43
palette = 10=#3aa44a
palette = 11=#b88200
palette = 12=#3f7ae0
palette = 13=#bb52c9
palette = 14=#1f9aa5
palette = 15=#e8e8e8
";

/// The terminal colors for a window appearance.
fn colors_for(appearance: WindowAppearance) -> TerminalColors {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => {
            TerminalColors::from_ghostty_theme(LIGHT_THEME).unwrap_or_default()
        }
        WindowAppearance::Dark | WindowAppearance::VibrantDark => TerminalColors::default(),
    }
}

fn appearance(settings: &TerminalSettings) -> TerminalAppearance {
    TerminalAppearance::default()
        .with_font_family(settings.font_family.clone())
        .with_font_size(px(settings.font_size))
        .with_cursor_blink(Some(settings.cursor_blink))
        .with_padding(px(PADDING))
}

fn local_options(spec: &LaunchSpec, settings: &TerminalSettings) -> LocalTerminalOptions {
    let engine =
        EngineOptions::default().with_scrollback_lines(Some(settings.scrollback_lines.max(100)));
    let cwd = match &spec.cwd {
        Some(path) => WorkingDirectory::Path(path.clone()),
        None => WorkingDirectory::Home,
    };
    let mut options = LocalTerminalOptions::default()
        .with_engine(engine)
        .with_cwd(cwd)
        .with_login(spec.login);
    if let Some(program) = &spec.program {
        options = options.with_program(program.clone(), spec.args.iter().cloned());
    }
    for (name, value) in &spec.env {
        options = options.with_env(name.clone(), value.clone());
    }
    for name in &spec.env_remove {
        options = options.without_env(name.clone());
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_light_theme_parses() {
        assert!(TerminalColors::from_ghostty_theme(LIGHT_THEME).is_ok());
        assert_ne!(
            colors_for(WindowAppearance::Light),
            colors_for(WindowAppearance::Dark)
        );
    }
}
