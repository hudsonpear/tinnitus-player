//! Default key bindings.
//!
//! Everything is scoped to `TYPING`: inside the window, but not while a text
//! field has focus. A field cannot do that for itself — GPUI dispatches the
//! bindings for a keystroke *before* the key listeners run, so stopping
//! propagation inside the field comes too late. The context predicate is what
//! keeps typing "s" in the search box from toggling shuffle.

use gpui::{App, KeyBinding};
use views::actions::*;

/// The window, minus any focused text field.
const TYPING: Option<&str> = Some("Root && !Field");

pub fn install(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", PlayPause, TYPING),
        KeyBinding::new("ctrl-right", NextTrack, TYPING),
        KeyBinding::new("ctrl-left", PreviousTrack, TYPING),
        KeyBinding::new("ctrl-.", Stop, TYPING),
        KeyBinding::new("up", VolumeUp, TYPING),
        KeyBinding::new("down", VolumeDown, TYPING),
        KeyBinding::new("m", ToggleMute, TYPING),
        KeyBinding::new("s", ToggleShuffle, TYPING),
        KeyBinding::new("r", CycleRepeat, TYPING),
        KeyBinding::new("ctrl-f", FocusSearch, Some("Root")),
        KeyBinding::new("ctrl-o", OpenFiles, TYPING),
        KeyBinding::new("ctrl-shift-o", OpenFolder, TYPING),
        KeyBinding::new("ctrl-space", CommandPalette, TYPING),
        // Escape stays window-wide: a field turns it into "clear me", and the
        // window still has to be able to close a dialog the field lives in.
        KeyBinding::new("escape", Dismiss, Some("Root")),
        KeyBinding::new("ctrl-q", Quit, Some("Root")),
        KeyBinding::new("ctrl-e", ShowEqualizer, TYPING),
        KeyBinding::new("ctrl-,", ShowSettings, TYPING),
        KeyBinding::new("ctrl-j", ShowQueue, TYPING),
        KeyBinding::new("alt-left", GoBack, TYPING),
        KeyBinding::new("alt-right", GoForward, TYPING),
        KeyBinding::new("f5", Rescan, Some("Root")),
        KeyBinding::new("left", SeekBack, TYPING),
        KeyBinding::new("right", SeekForward, TYPING),
        KeyBinding::new("f", ToggleFavorite, TYPING),
        KeyBinding::new("ctrl-a", SelectAll, TYPING),
        KeyBinding::new("enter", PlaySelected, TYPING),
        KeyBinding::new("delete", DeleteSelected, TYPING),
        KeyBinding::new("ctrl-g", ShowCurrent, TYPING),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{KeyBindingContextPredicate, KeyContext};

    /// True when a binding with `predicate` would fire for a focus path. The
    /// contexts run from the window inwards, which is the order GPUI hands the
    /// keymap, and `depth_of` is the same call the dispatcher makes.
    fn fires(predicate: &str, path: &[&str]) -> bool {
        let contexts: Vec<KeyContext> = path
            .iter()
            .map(|context| KeyContext::parse(context).expect("a valid context"))
            .collect();
        KeyBindingContextPredicate::parse(predicate)
            .expect("a valid predicate")
            .depth_of(&contexts)
            .is_some()
    }

    #[test]
    fn shortcuts_fire_in_the_window_but_never_inside_a_text_field() {
        let typing = TYPING.expect("a predicate");
        assert!(fires(typing, &["Root"]));
        assert!(fires(typing, &["Root", "Table"]));
        // The whole point: "s" in the search box types an s rather than
        // toggling shuffle, and space types a space rather than pausing.
        assert!(!fires(typing, &["Root", "Field"]));
    }

    #[test]
    fn escape_still_reaches_the_window_from_a_field() {
        // A dialog's own text field must not trap Escape.
        assert!(fires("Root", &["Root", "Field"]));
    }

    #[test]
    fn every_context_predicate_parses() {
        // A predicate that does not parse panics inside `KeyBinding::new`, which
        // would take the app down at start-up rather than at build time.
        for predicate in [TYPING.expect("a predicate"), "Root"] {
            assert!(
                KeyBindingContextPredicate::parse(predicate).is_ok(),
                "{predicate}"
            );
        }
    }
}

// Media keys are deliberately not bound here. On Windows they arrive as
// WM_APPCOMMAND rather than as key events, so a key binding would never fire —
// wiring them needs the OS transport API (SMTC on Windows, MPRIS on Linux),
// which is a separate piece of work, not a line in this list.
