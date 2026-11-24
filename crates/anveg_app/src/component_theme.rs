//! Keeps gpui-component's theme in step with gpui-cn's.
//!
//! The app is styled by gpui-cn. gpui-component is used only for the code
//! editor, the diff view and the Markdown renderer (Phase 4), which read
//! gpui-component's own theme global. This copies gpui-cn's mode, fonts and
//! main colors into it, then lets gpui-cn project its theme onto gpui-base
//! again (both libraries write that projection; gpui-cn's must win so the
//! scrollbars and resize handles look the same everywhere).

use gpui_cn::ActiveTheme as _;
use gpui_kit::App;
use gpui_kit::component::{Theme as CTheme, ThemeMode};

/// Copy gpui-cn's appearance into gpui-component's theme. Cheap when
/// nothing changed.
pub fn sync(cx: &mut App) {
    let t = cx.theme();
    let dark = t.is_dark();
    let colors = [
        t.background(),
        t.foreground(),
        t.border(),
        t.muted(),
        t.muted_foreground(),
        t.accent(),
        t.accent_foreground(),
        t.primary(),
        t.primary_foreground(),
        t.secondary(),
        t.secondary_foreground(),
        t.destructive(),
        t.input(),
        t.ring(),
        t.selection(),
    ];
    let font = t.font_family().clone();
    let mono = t.mono_font_family().clone();
    let [
        background,
        foreground,
        border,
        muted,
        muted_foreground,
        accent,
        accent_foreground,
        primary,
        primary_foreground,
        secondary,
        secondary_foreground,
        destructive,
        input,
        ring,
        selection,
    ] = colors;

    let mode = if dark { ThemeMode::Dark } else { ThemeMode::Light };
    let current = CTheme::global(cx);
    let up_to_date = current.mode == mode
        && current.background == background
        && current.font_family == font
        && current.mono_font_family == mono;
    if up_to_date {
        return;
    }
    if current.mode != mode {
        CTheme::change(mode, None, cx);
    }
    CTheme::update(cx, |c| {
        c.font_family = font;
        c.mono_font_family = mono;
        c.mono_font_size = gpui_kit::px(13.);
        c.background = background;
        c.foreground = foreground;
        c.border = border;
        c.muted = muted;
        c.muted_foreground = muted_foreground;
        c.accent = accent;
        c.accent_foreground = accent_foreground;
        c.primary = primary;
        c.primary_foreground = primary_foreground;
        c.secondary = secondary;
        c.secondary_foreground = secondary_foreground;
        c.danger = destructive;
        c.input = input;
        c.ring = ring;
        c.selection = selection;
        c.popover = background;
        c.popover_foreground = foreground;
        c.list_hover = accent;
        c.list_active = selection;
        c.caret = foreground;
    });
    // gpui-cn owns the gpui-base projection.
    gpui_cn::Theme::update(cx, |_| {});
}
