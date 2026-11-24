//! Toolbar commands: buttons in the top bar that run a shell command in a
//! terminal tab (build, run, test…), each with an optional shortcut.
//!
//! The commands belong to the project (`Project::commands` in
//! `projects.json`). This module checks shortcuts and binds them; the
//! window runs the commands (`workspace_ui.rs`) and the editor tab is
//! `CommandsView`.

use std::collections::BTreeSet;

use gpui_kit::{App, Global, KeyBinding, Keystroke, OperatingSystem};

use crate::actions::RunProjectCommand;
use crate::model::model;

/// Shortcuts the app (or the terminal and editor) already use.
const RESERVED: &[&str] = &[
    "cmd-q", "cmd-,", "cmd-o", "cmd-n", "cmd-shift-n", "cmd-t", "cmd-shift-t", "cmd-w",
    "cmd-shift-w", "cmd-m", "cmd-h", "cmd-alt-h", "cmd-shift-]", "cmd-shift-[", "cmd-f",
    "cmd-g", "cmd-shift-g", "cmd-\\", "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6",
    "cmd-7", "cmd-8", "cmd-9", "cmd-0", "cmd--", "cmd-=", "cmd-p", "cmd-alt-n", "cmd-s",
    "cmd-alt-s", "cmd-shift-1", "cmd-shift-2", "cmd-shift-3", "cmd-shift-4", "cmd-l",
    "cmd-shift-l", "cmd-enter", "cmd-c", "cmd-v", "cmd-x", "cmd-a", "cmd-z", "cmd-shift-z",
    "cmd-up", "cmd-down", "cmd-left", "cmd-right", "cmd-pageup", "cmd-pagedown",
    "cmd-backspace", "cmd-delete",
];

/// Lower-case, `+` to `-`, no spaces: "Cmd+Shift+B" → "cmd-shift-b".
pub fn normalize(input: &str) -> String {
    input
        .trim()
        .to_lowercase()
        .replace(['+', ' '], "-")
        .replace("command", "cmd")
        .replace("option", "alt")
}

/// Check a shortcut typed in the editor. Empty means none. It must use ⌘
/// (every other key belongs to the terminal and Claude), or be a function
/// key, and must not be one the app already uses.
pub fn validate(input: &str) -> Result<String, String> {
    let s = normalize(input);
    if s.is_empty() {
        return Ok(s);
    }
    let not_a_shortcut = || format!("“{input}” isn't a shortcut, try cmd-shift-b or f5.");
    // A bare modifier ("cmd-") has no key.
    if s.ends_with('-') && !s.ends_with("--") {
        return Err(not_a_shortcut());
    }
    let keystroke =
        Keystroke::parse_with_os(&s, OperatingSystem::Mac).map_err(|_| not_a_shortcut())?;
    if keystroke.key.is_empty()
        || matches!(keystroke.key.as_str(), "cmd" | "shift" | "alt" | "ctrl" | "fn")
    {
        return Err(not_a_shortcut());
    }
    let function_key = keystroke
        .key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=20).contains(&n));
    if !keystroke.modifiers.platform && !function_key {
        return Err("Use ⌘ (cmd-…) or a function key: other keys go to the terminal.".into());
    }
    if RESERVED.contains(&s.as_str()) {
        return Err(format!("{s} is already used by Anveg Code."));
    }
    Ok(s)
}

/// The shortcuts bound so far (bindings can only be added).
#[derive(Default)]
struct Bound(BTreeSet<String>);

impl Global for Bound {}

/// Bind every project's shortcuts that are not bound yet. A binding sends
/// [`RunProjectCommand`] with its keystroke; each window runs its own
/// project's command for it, or lets the key through when it has none.
pub fn bind_shortcuts(cx: &mut App) {
    let shortcuts: BTreeSet<String> = model(cx)
        .read(cx)
        .projects
        .projects
        .iter()
        .flat_map(|p| p.commands.iter())
        .map(|c| c.shortcut.clone())
        .filter(|s| validate(s).is_ok_and(|s| !s.is_empty()))
        .collect();
    if !cx.has_global::<Bound>() {
        cx.set_global(Bound::default());
    }
    let mut new = Vec::new();
    for shortcut in shortcuts {
        if cx.global::<Bound>().0.contains(&shortcut) {
            continue;
        }
        match KeyBinding::load(
            &shortcut,
            Box::new(RunProjectCommand {
                shortcut: shortcut.clone(),
            }),
            None,
            false,
            None,
            cx,
        ) {
            Ok(binding) => {
                new.push(binding);
                cx.global_mut::<Bound>().0.insert(shortcut);
            }
            Err(error) => tracing::warn!(%shortcut, %error, "could not bind a command shortcut"),
        }
    }
    if !new.is_empty() {
        cx.bind_keys(new);
    }
}

/// How a shortcut reads in a tooltip: "⌘⇧B".
pub fn display(shortcut: &str) -> String {
    let mut out = String::new();
    let parts: Vec<&str> = shortcut.split('-').collect();
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        out.push_str(match (*part, last) {
            ("cmd", false) => "⌘",
            ("shift", false) => "⇧",
            ("alt", false) => "⌥",
            ("ctrl", false) => "⌃",
            _ => "",
        });
        if last {
            out.push_str(&part.to_uppercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts() {
        assert_eq!(validate("Cmd+Shift+B"), Ok("cmd-shift-b".into()));
        assert_eq!(validate("f5"), Ok("f5".into()));
        assert_eq!(validate(""), Ok(String::new()));
        assert!(validate("ctrl-b").is_err());
        assert!(validate("cmd-s").is_err());
        assert!(validate("cmd-").is_err());
        assert!(validate("cmd-shift").is_err());
        // ⌘− is the terminal's "smaller font".
        assert!(validate("cmd--").is_err());
        assert_eq!(validate("cmd-alt-r"), Ok("cmd-alt-r".into()));
        assert_eq!(display("cmd-shift-b"), "⌘⇧B");
        assert_eq!(display("f5"), "F5");
    }
}
