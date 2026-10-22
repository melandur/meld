//! Window-level commands and their keyboard shortcuts (`accelerators.py`).
//!
//! A shortcut maps to one command; the active tab decides what a command
//! means for it (Alt+Left pushes a chunk in a file comparison and copies a
//! file in a folder comparison, F9 toggles the overview map or the console).

use iced::keyboard::{Key, Modifiers, key::Named};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Quit,
    Help,
    Preferences,
    Find,
    FindNext,
    FindPrevious,
    FindReplace,
    GoToLine,
    NextChange,
    PreviousChange,
    NextPane,
    PreviousPane,
    OpenExternal,
    Undo,
    Redo,
    Refresh,
    Save,
    SaveAll,
    SaveAs,
    NextFile,
    PreviousFile,
    CloseTab,
    GearMenu,
    Fullscreen,
    NewTab,
    NextTab,
    PreviousTab,
    SelectTab(usize),
    Stop,
    Filter,
    PreviousConflict,
    NextConflict,
    PushLeft,
    PushRight,
    PullLeft,
    PullRight,
    CopyLeftUp,
    CopyRightUp,
    CopyLeftDown,
    CopyRightDown,
    DeleteChange,
    /// F9: the overview map in file comparisons, the console in VC views.
    TogglePanel,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Activate,
    Delete,
    Commit,
    SwapPanes,
}

/// Maps a key press to a command. Only key presses no widget consumed reach
/// this, so plain keys like Return and Delete only act when no text field
/// has focus.
pub fn from_key(key: &Key, modifiers: Modifiers) -> Option<Command> {
    use Command::*;

    let ctrl = modifiers.command();
    let shift = modifiers.shift();
    let alt = modifiers.alt();

    if let Key::Named(named) = key {
        let command = match (named, ctrl, alt, shift) {
            (Named::F1, false, false, false) => Help,
            (Named::F3, false, false, false) => FindNext,
            (Named::F3, false, false, true) => FindPrevious,
            (Named::F5, false, false, false) => Refresh,
            (Named::F7, false, true, false) => NextFile,
            (Named::F7, false, true, true) => PreviousFile,
            (Named::F8, false, false, false) => Filter,
            (Named::F9, false, false, false) => TogglePanel,
            (Named::F10, false, false, false) => GearMenu,
            (Named::F11, false, false, false) => Fullscreen,
            (Named::Escape, false, false, false) => Stop,
            (Named::ArrowDown, false, true, false) => NextChange,
            (Named::ArrowUp, false, true, false) => PreviousChange,
            (Named::PageDown, false, true, false) => NextPane,
            (Named::PageUp, false, true, false) => PreviousPane,
            (Named::PageDown, true, false, false) => NextTab,
            (Named::PageUp, true, false, false) => PreviousTab,
            (Named::Tab, true, false, false) => NextTab,
            (Named::Tab, true, false, true) => PreviousTab,
            (Named::ArrowLeft, false, true, false) => PushLeft,
            (Named::ArrowRight, false, true, false) => PushRight,
            (Named::ArrowRight, false, true, true) => PullLeft,
            (Named::ArrowLeft, false, true, true) => PullRight,
            (Named::Delete, false, true, false) => DeleteChange,
            (Named::Delete, false, false, false) => Delete,
            (Named::Enter, false, false, false) => Activate,
            _ => return None,
        };
        return Some(command);
    }

    let Key::Character(c) = key else {
        return None;
    };
    let c = c.to_lowercase();
    let command = match (c.as_str(), ctrl, alt, shift) {
        ("q", true, false, false) => Quit,
        (",", true, false, false) => Preferences,
        ("f", true, false, false) => Find,
        ("g", true, false, false) => FindNext,
        ("g", true, false, true) => FindPrevious,
        ("h", true, false, false) => FindReplace,
        ("i", true, false, false) => GoToLine,
        ("d", true, false, false) => NextChange,
        ("e", true, false, false) => PreviousChange,
        ("o", true, false, true) => OpenExternal,
        ("z", true, false, false) => Undo,
        ("z", true, false, true) => Redo,
        ("r", true, false, false) => Refresh,
        ("s", true, false, false) => Save,
        ("l", true, false, true) => SaveAll,
        ("s", true, false, true) => SaveAs,
        ("w", true, false, false) => CloseTab,
        ("n", true, false, false) => NewTab,
        ("j", true, false, false) => PreviousConflict,
        ("k", true, false, false) => NextConflict,
        ("[", false, true, false) => CopyLeftUp,
        ("]", false, true, false) => CopyRightUp,
        (";", false, true, false) => CopyLeftDown,
        ("'", false, true, false) => CopyRightDown,
        ("+" | "=", true, false, _) => ZoomIn,
        ("-", true, false, false) => ZoomOut,
        ("0", true, false, false) => ZoomReset,
        ("m", true, false, false) => Commit,
        ("\\", false, true, false) => SwapPanes,
        (d, false, true, false) if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => {
            let n = (d.as_bytes()[0] - b'0') as usize;
            SelectTab(if n == 0 { 9 } else { n - 1 })
        }
        _ => return None,
    };
    Some(command)
}

/// Shortcut list for the keyboard shortcuts window.
pub const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "General",
        &[
            ("Ctrl+N", "New comparison"),
            ("Ctrl+W", "Close comparison"),
            ("Ctrl+Q", "Quit"),
            ("Ctrl+,", "Preferences"),
            ("Ctrl+PgDn / Ctrl+PgUp", "Next / previous tab"),
            ("Alt+1…9", "Switch to tab"),
            ("F10", "Main menu"),
            ("F11", "Fullscreen"),
            ("Escape", "Stop the current operation"),
        ],
    ),
    (
        "Comparisons",
        &[
            ("Ctrl+S", "Save"),
            ("Ctrl+Shift+S", "Save as"),
            ("Ctrl+Shift+L", "Save all"),
            ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
            ("Ctrl+R, F5", "Refresh"),
            ("Alt+Down, Ctrl+D", "Next change"),
            ("Alt+Up, Ctrl+E", "Previous change"),
            ("Ctrl+K / Ctrl+J", "Next / previous conflict"),
            ("Alt+PgDn / Alt+PgUp", "Next / previous pane"),
            ("Ctrl+Shift+O", "Open in external editor"),
            ("Alt+\\", "Swap panes"),
            ("F8", "Filters"),
            ("F9", "Overview map / console"),
            ("Ctrl+= / Ctrl+- / Ctrl+0", "Zoom in / out / reset"),
        ],
    ),
    (
        "Changes",
        &[
            ("Alt+Left / Alt+Right", "Push change left / right"),
            ("Alt+Shift+Right / Alt+Shift+Left", "Pull change from left / right"),
            ("Alt+[ / Alt+]", "Copy change above left / right"),
            ("Alt+; / Alt+'", "Copy change below left / right"),
            ("Alt+Delete", "Delete change"),
        ],
    ),
    (
        "Find",
        &[
            ("Ctrl+F", "Find"),
            ("Ctrl+G, F3", "Find next"),
            ("Ctrl+Shift+G, Shift+F3", "Find previous"),
            ("Ctrl+H", "Find and replace"),
            ("Ctrl+I", "Go to line"),
        ],
    ),
    (
        "Folder and version control",
        &[
            ("Return", "Compare selected"),
            ("Alt+Left / Alt+Right", "Copy to left / right"),
            ("Delete", "Delete selected"),
            ("Ctrl+M", "Commit"),
        ],
    ),
    (
        "Git Compare",
        &[("Alt+F7 / Alt+Shift+F7", "Next / previous changed file")],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_meld_accelerators() {
        let ctrl = Modifiers::CTRL;
        assert_eq!(from_key(&Key::Character("s".into()), ctrl), Some(Command::Save));
        assert_eq!(
            from_key(&Key::Character("S".into()), ctrl | Modifiers::SHIFT),
            Some(Command::SaveAs)
        );
        assert_eq!(
            from_key(&Key::Named(Named::ArrowDown), Modifiers::ALT),
            Some(Command::NextChange)
        );
        assert_eq!(
            from_key(&Key::Named(Named::ArrowLeft), Modifiers::ALT | Modifiers::SHIFT),
            Some(Command::PullRight)
        );
        assert_eq!(
            from_key(&Key::Character("3".into()), Modifiers::ALT),
            Some(Command::SelectTab(2))
        );
        assert_eq!(from_key(&Key::Character("x".into()), Modifiers::empty()), None);
    }
}
