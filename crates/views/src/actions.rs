//! The actions the window responds to. Key bindings for these live in the app
//! crate; keeping the actions here lets any view dispatch them.

use gpui::actions;

actions!(
    tinnitus,
    [
        /// Space
        PlayPause,
        /// Ctrl-Right
        NextTrack,
        /// Ctrl-Left
        PreviousTrack,
        Stop,
        /// Up
        VolumeUp,
        /// Down
        VolumeDown,
        /// M
        ToggleMute,
        /// S
        ToggleShuffle,
        /// R
        CycleRepeat,
        /// Ctrl-F
        FocusSearch,
        /// Ctrl-O
        OpenFiles,
        /// Ctrl-Shift-O
        OpenFolder,
        /// Ctrl-Space
        CommandPalette,
        /// Escape
        Dismiss,
        /// Ctrl-Q
        Quit,
        /// Ctrl-E
        ShowEqualizer,
        /// Ctrl-,
        ShowSettings,
        /// Ctrl-J
        ShowQueue,
        /// Alt-Left
        GoBack,
        /// Alt-Right
        GoForward,
        /// F5
        Rescan,
        /// Left
        SeekBack,
        /// Right
        SeekForward,
        /// F
        ToggleFavorite,
        /// Ctrl-A
        SelectAll,
        /// Enter
        PlaySelected,
        /// Delete
        DeleteSelected,
        /// Ctrl-G
        ShowCurrent,
    ]
);

/// How each action is described in the command palette and the settings screen.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Play / Pause", "Space"),
    ("Next track", "Ctrl+Right"),
    ("Previous track", "Ctrl+Left"),
    ("Volume up", "Up"),
    ("Volume down", "Down"),
    ("Mute", "M"),
    ("Shuffle", "S"),
    ("Repeat", "R"),
    ("Search", "Ctrl+F"),
    ("Open files", "Ctrl+O"),
    ("Open folder", "Ctrl+Shift+O"),
    ("Command palette", "Ctrl+Space"),
    ("Queue", "Ctrl+J"),
    ("Equalizer", "Ctrl+E"),
    ("Settings", "Ctrl+,"),
    ("Rescan library", "F5"),
    ("Stop", "Ctrl+."),
    ("Back", "Alt+Left"),
    ("Forward", "Alt+Right"),
    ("Close dialog or menu", "Escape"),
    ("Seek back 5 seconds", "Left"),
    ("Seek forward 5 seconds", "Right"),
    ("Favorite the playing song", "F"),
    ("Select all songs", "Ctrl+A"),
    ("Play selected song", "Enter"),
    ("Delete selected files", "Delete"),
    ("Show the playing song", "Ctrl+G"),
    ("Quit", "Ctrl+Q"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_is_described_once() {
        let mut labels: Vec<&str> = SHORTCUTS.iter().map(|(label, _)| *label).collect();
        let total = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), total, "two shortcuts share a label");
    }

    #[test]
    fn the_documented_defaults_are_present() {
        // These are the ones the design calls out by name.
        for wanted in ["Space", "Ctrl+F", "Ctrl+Space", "Ctrl+Q"] {
            assert!(
                SHORTCUTS.iter().any(|(_, keys)| *keys == wanted),
                "{wanted} is not bound"
            );
        }
    }
}
