//! Option-as-Meta: what Option+key sends when the setting is on.
//!
//! The engine encodes keys with Ghostty's encoder, which on macOS treats
//! Option as a character-composing key (Option+a types "å"). Terminal apps
//! such as Claude Code expect Meta instead: ESC followed by the key. Keys
//! the encoder already handles well with Alt (arrows, function keys) are
//! left to it.

/// The bytes to send for Option + `key`, or `None` to let the engine encode
/// the key itself. `key` is GPUI's key name (`"a"`, `"enter"`, …).
pub(crate) fn meta_sequence(key: &str, shift: bool) -> Option<String> {
    let tail = match key {
        "enter" => "\r".to_owned(),
        "backspace" => "\x7f".to_owned(),
        "space" => " ".to_owned(),
        "escape" => "\x1b".to_owned(),
        _ => {
            let mut chars = key.chars();
            let ch = chars.next()?;
            if chars.next().is_some() || ch.is_control() {
                return None;
            }
            if shift {
                ch.to_uppercase().collect()
            } else {
                ch.to_string()
            }
        }
    };
    Some(format!("\x1b{tail}"))
}

#[cfg(test)]
mod tests {
    use super::meta_sequence;

    #[test]
    fn letters_get_an_escape_prefix() {
        assert_eq!(meta_sequence("b", false).as_deref(), Some("\x1bb"));
        assert_eq!(meta_sequence("b", true).as_deref(), Some("\x1bB"));
        assert_eq!(meta_sequence(".", false).as_deref(), Some("\x1b."));
    }

    #[test]
    fn option_enter_is_esc_cr() {
        assert_eq!(meta_sequence("enter", false).as_deref(), Some("\x1b\r"));
        assert_eq!(
            meta_sequence("backspace", false).as_deref(),
            Some("\x1b\x7f")
        );
    }

    #[test]
    fn named_keys_are_left_to_the_engine() {
        assert_eq!(meta_sequence("left", false), None);
        assert_eq!(meta_sequence("f5", false), None);
        assert_eq!(meta_sequence("", false), None);
    }
}
