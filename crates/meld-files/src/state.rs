//! Row states shared by folder and version control comparisons
//!
//! Port of the `STATE_*` constants that `meld/tree.py` re-exports from
//! `meld/vc/_vc.py`. The discriminants match the Python values, and the
//! derived ordering follows them, so e.g. `State::New < State::Modified`.

use std::fmt;

/// The state of one pane of a folder comparison row
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum State {
    /// Ignored by a filter or the version control system
    Ignored = 0,
    /// Unversioned or otherwise unknown
    None = 1,
    /// Identical in every pane
    Normal = 2,
    /// Identical after applying text filters
    NoChange = 3,
    /// Could not be read
    Error = 4,
    /// Placeholder row for an empty folder
    Empty = 5,
    /// Present in some panes but not others (or added in VC)
    New = 6,
    /// Different between panes
    Modified = 7,
    /// Renamed (VC only)
    Renamed = 8,
    /// Conflicted (VC only)
    Conflict = 9,
    /// Removed (VC only)
    Removed = 10,
    /// Missing locally (VC only)
    Missing = 11,
    /// Does not exist in this pane
    NonExist = 12,
    /// Scan in progress
    Spinner = 13,
}

/// Number of states; `STATE_MAX` in Python
pub const STATE_MAX: usize = 14;

impl State {
    /// Every state, in discriminant order
    pub const ALL: [State; STATE_MAX] = [
        State::Ignored,
        State::None,
        State::Normal,
        State::NoChange,
        State::Error,
        State::Empty,
        State::New,
        State::Modified,
        State::Renamed,
        State::Conflict,
        State::Removed,
        State::Missing,
        State::NonExist,
        State::Spinner,
    ];

    /// The Python integer value of this state
    pub fn index(self) -> usize {
        self as usize
    }

    /// The state for a Python integer value
    pub fn from_index(index: usize) -> Option<State> {
        Self::ALL.get(index).copied()
    }

    /// Whether this state counts as a difference when navigating
    /// between changes (everything but normal, no-change and empty)
    pub fn is_change(self) -> bool {
        !matches!(self, State::Normal | State::NoChange | State::Empty)
    }

    /// A short, untranslated name for the state
    pub fn name(self) -> &'static str {
        match self {
            State::Ignored => "ignored",
            State::None => "none",
            State::Normal => "normal",
            State::NoChange => "nochange",
            State::Error => "error",
            State::Empty => "empty",
            State::New => "new",
            State::Modified => "modified",
            State::Renamed => "renamed",
            State::Conflict => "conflict",
            State::Removed => "removed",
            State::Missing => "missing",
            State::NonExist => "nonexist",
            State::Spinner => "spinner",
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_match_python() {
        for (i, state) in State::ALL.iter().enumerate() {
            assert_eq!(state.index(), i);
            assert_eq!(State::from_index(i), Some(*state));
        }
        assert_eq!(State::from_index(STATE_MAX), None);
        assert_eq!(State::Normal.index(), 2);
        assert_eq!(State::NonExist.index(), 12);
    }

    #[test]
    fn ordering_follows_values() {
        assert!(State::Ignored < State::None);
        assert!(State::New < State::Modified);
        assert!(State::Modified < State::Conflict);
        assert!(State::NonExist < State::Spinner);
        let mut states = vec![State::Modified, State::Normal, State::New];
        states.sort();
        assert_eq!(states, vec![State::Normal, State::New, State::Modified]);
    }

    #[test]
    fn change_states() {
        assert!(!State::Normal.is_change());
        assert!(!State::NoChange.is_change());
        assert!(!State::Empty.is_change());
        assert!(State::Modified.is_change());
        assert!(State::New.is_change());
    }
}
