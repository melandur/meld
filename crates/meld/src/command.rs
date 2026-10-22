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

/// A heading and its (keys, action) rows in the keyboard shortcuts window.
pub type ShortcutGroup = (&'static str, Vec<(&'static str, &'static str)>);

/// Shortcut list for the keyboard shortcuts window (`help-overlay.ui`),
/// translated.
pub fn shortcuts() -> Vec<ShortcutGroup> {
    vec![
        (
            trc!("shortcut window", "Windows and Tabs"),
            vec![
                ("Ctrl+N", trc!("shortcut window", "New comparison")),
                ("Ctrl+W", trc!("shortcut window", "Close a comparison")),
                ("Ctrl+Q", trc!("shortcut window", "Quit Meld")),
                ("Ctrl+PgUp, Ctrl+Shift+Tab", trc!("shortcut window", "Go to previous tab")),
                ("Ctrl+PgDn, Ctrl+Tab", trc!("shortcut window", "Go to next tab")),
                ("Alt+1…9", trc!("shortcut window", "Switch to tab")),
            ],
        ),
        (
            trc!("shortcut window", "General"),
            vec![
                ("F1", trc!("shortcut window", "Show help")),
                ("Ctrl+,", trc!("shortcut window", "Show preferences")),
                ("F10", trc!("shortcut window", "Open menu")),
                ("F11", trc!("shortcut window", "Fullscreen")),
            ],
        ),
        (
            trc!("shortcut window", "Common Actions"),
            vec![
                ("Escape", trc!("shortcut window", "Stop the current action")),
                ("Ctrl+R, F5", trc!("shortcut window", "Refresh comparison")),
                ("Ctrl+Shift+O", trc!("shortcut window", "Open externally")),
                ("F8", trc!("shortcut window", "Filters")),
                ("Ctrl+= / Ctrl+- / Ctrl+0", trc!("shortcut window", "Zoom in / out / reset")),
            ],
        ),
        (
            trc!("shortcut window", "Panes"),
            vec![
                ("Alt+PgUp", trc!("shortcut window", "Previous comparison pane")),
                ("Alt+PgDn", trc!("shortcut window", "Next comparison pane")),
                ("Alt+\\", trc!("shortcut window", "Swap left and right panes")),
            ],
        ),
        (
            trc!("shortcut window", "Changes"),
            vec![
                ("Alt+Up, Ctrl+E", trc!("shortcut window", "Go to previous change")),
                ("Alt+Down, Ctrl+D", trc!("shortcut window", "Go to next change")),
            ],
        ),
        (
            trc!("shortcut window", "Editing"),
            vec![
                ("Ctrl+Z", trc!("shortcut window", "Undo")),
                ("Ctrl+Shift+Z", trc!("shortcut window", "Redo")),
                ("Ctrl+F", trc!("shortcut window", "Find")),
                ("Ctrl+G, F3", trc!("shortcut window", "Find Next")),
                ("Ctrl+Shift+G, Shift+F3", trc!("shortcut window", "Find Previous")),
                ("Ctrl+H", trc!("shortcut window", "Replace")),
                ("Ctrl+I", trc!("shortcut window", "Go to line")),
            ],
        ),
        (
            trc!("shortcut window", "File comparison"),
            vec![
                ("Ctrl+S", trc!("shortcut window", "Save current file")),
                ("Ctrl+Shift+S", trc!("shortcut window", "Save current file to new path")),
                ("Ctrl+Shift+L", trc!("shortcut window", "Save all files in comparison")),
                ("Ctrl+J", trc!("shortcut window", "Previous conflict")),
                ("Ctrl+K", trc!("shortcut window", "Next conflict")),
                ("Alt+Left", trc!("shortcut window", "Push change to left")),
                ("Alt+Right", trc!("shortcut window", "Push change to right")),
                ("Alt+Shift+Right", trc!("shortcut window", "Pull change from left")),
                ("Alt+Shift+Left", trc!("shortcut window", "Pull change from right")),
                ("Alt+[", trc!("shortcut window", "Copy change above left")),
                ("Alt+;", trc!("shortcut window", "Copy change below left")),
                ("Alt+]", trc!("shortcut window", "Copy change above right")),
                ("Alt+'", trc!("shortcut window", "Copy change below right")),
                ("Alt+Delete", trc!("shortcut window", "Delete change")),
                ("F9", trc!("shortcut window", "Show overview map")),
            ],
        ),
        (
            trc!("shortcut window", "Folder comparison"),
            vec![
                ("Return", trc!("shortcut window", "Compare selected")),
                ("Alt+Left", trc!("shortcut window", "Copy to left")),
                ("Alt+Right", trc!("shortcut window", "Copy to right")),
                ("Delete", trc!("shortcut window", "Delete selected")),
            ],
        ),
        (
            trc!("shortcut window", "Version control comparison"),
            vec![
                ("Ctrl+M", trc!("shortcut window", "Commit to version control")),
                ("F9", trc!("shortcut window", "Show/hide console output")),
            ],
        ),
        (
            trc!("shortcut window", "Git Compare"),
            vec![("Alt+F7 / Alt+Shift+F7", trc!("shortcut window", "Next / previous changed file"))],
        ),
    ]
}

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
