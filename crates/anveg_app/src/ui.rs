//! Shared look: colors read from the theme once per render, and small
//! building blocks.

use gpui_cn::ActiveTheme as _;
use gpui_kit::{
    AnyView, App, AppContext as _, Context, Hsla, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, hsla,
};

use anveg_core::status::SessionStatus;

/// Theme colors used across the app.
#[derive(Clone, Copy)]
pub struct Colors {
    pub background: Hsla,
    pub foreground: Hsla,
    pub muted_foreground: Hsla,
    pub border: Hsla,
    pub accent: Hsla,
    pub primary: Hsla,
    /// The selected row in a list: a tint of the theme's accent blue, strong
    /// enough to read on the dark sidebar too.
    pub selected_row: Hsla,
    /// The bar on the left edge of the selected row.
    pub selected_bar: Hsla,
    pub destructive: Hsla,
    pub sidebar: Hsla,
}

impl Colors {
    pub fn of(cx: &App) -> Self {
        let t = cx.theme();
        Self {
            background: t.background(),
            foreground: t.foreground(),
            muted_foreground: t.muted_foreground(),
            border: t.border(),
            accent: t.accent(),
            primary: t.primary(),
            selected_row: t.ring().opacity(if t.is_dark() { 0.30 } else { 0.16 }),
            selected_bar: t.ring(),
            destructive: t.destructive(),
            sidebar: t.muted().opacity(0.5),
        }
    }

    /// The status dot's color (static colors only, no animation).
    pub fn status(&self, status: Option<SessionStatus>) -> Option<Hsla> {
        match status? {
            SessionStatus::Working => Some(hsla(212. / 360., 0.85, 0.55, 1.0)),
            SessionStatus::NeedsInput => Some(hsla(45. / 360., 0.95, 0.52, 1.0)),
            SessionStatus::Idle => Some(self.muted_foreground),
            SessionStatus::Stopped => None,
        }
    }
}

/// The status as words, for tooltips.
pub fn status_label(status: Option<SessionStatus>) -> &'static str {
    match status {
        Some(SessionStatus::Working) => "Working",
        Some(SessionStatus::NeedsInput) => "Needs input",
        Some(SessionStatus::Idle) => "Idle",
        Some(SessionStatus::Stopped) | None => "Not running",
    }
}

/// A tooltip with several lines (gpui-cn's text tooltip is one line).
pub struct LinesTooltip(Vec<SharedString>);

impl LinesTooltip {
    pub fn build(lines: Vec<String>, cx: &mut App) -> AnyView {
        cx.new(|_| Self(lines.into_iter().map(Into::into).collect()))
            .into()
    }
}

impl Render for LinesTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        div()
            .w(gpui_kit::px(380.))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(c.border)
            .bg(c.background)
            .text_xs()
            .text_color(c.foreground)
            .flex()
            .flex_col()
            .children(
                self.0
                    .iter()
                    .cloned()
                    .map(|line| div().truncate().child(line)),
            )
    }
}

/// A path for display, with the home folder as `~`.
pub fn tilde(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
