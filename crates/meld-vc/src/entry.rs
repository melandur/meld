//! Version control states, conflict sides, actions and the [`Entry`] type
//! (port of the constants and `Entry` class in `meld/vc/_vc.py`).

use std::fmt;
use std::path::{Path, PathBuf};

/// Version control state of a path. The discriminants and their ordering
/// match the Python `STATE_*` constants (the ordering matters: e.g.
/// [`Entry::is_modified`] is `state >= New`, and Bazaar takes the `max` of
/// several states).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum State {
    /// `STATE_IGNORED`
    Ignored = 0,
    /// `STATE_NONE`: not under version control.
    Unversioned = 1,
    /// `STATE_NORMAL`: versioned and unchanged.
    #[default]
    Normal = 2,
    /// `STATE_NOCHANGE`
    NoChange = 3,
    /// `STATE_ERROR`
    Error = 4,
    /// `STATE_EMPTY`: placeholder row.
    Empty = 5,
    /// `STATE_NEW`: added to version control.
    New = 6,
    /// `STATE_MODIFIED`
    Modified = 7,
    /// `STATE_RENAMED`
    Renamed = 8,
    /// `STATE_CONFLICT`
    Conflict = 9,
    /// `STATE_REMOVED`: removed from version control.
    Removed = 10,
    /// `STATE_MISSING`: versioned, but missing locally.
    Missing = 11,
    /// `STATE_NONEXIST`: not present.
    NonExist = 12,
    /// `STATE_SPINNER`: scan in progress.
    Spinner = 13,
}

impl State {
    /// The status column label (`Entry.state_names`). Several states have
    /// no label to avoid visual clutter. Python renders `Conflict` in bold
    /// markup; see [`State::is_emphasized`].
    pub fn label(self) -> &'static str {
        match self {
            State::Ignored => "Ignored",
            State::Unversioned => "Unversioned",
            State::Normal | State::NoChange | State::Empty => "",
            State::Error => "Error",
            State::New => "Newly added",
            State::Modified => "Modified",
            State::Renamed => "Renamed",
            State::Conflict => "Conflict",
            State::Removed => "Removed",
            State::Missing => "Missing",
            State::NonExist => "Not present",
            State::Spinner => "Scanning…",
        }
    }

    /// Whether the status label should be shown emphasised (bold).
    pub fn is_emphasized(self) -> bool {
        self == State::Conflict
    }
}

/// Side of a three-way conflict (`CONFLICT_*` constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ConflictSide {
    /// The merged working file.
    Merged = 0,
    /// The common ancestor.
    Base = 1,
    /// Our side (`CONFLICT_THIS` in Bazaar terms).
    Local = 2,
    /// Their side (`CONFLICT_OTHER` in Bazaar terms).
    Remote = 3,
}

impl ConflictSide {
    /// Display label (`_vc.conflicts`), also used in temp file names.
    pub fn label(self) -> &'static str {
        match self {
            ConflictSide::Merged => "Merged",
            ConflictSide::Base => "Base",
            ConflictSide::Local => "Local",
            ConflictSide::Remote => "Remote",
        }
    }

    /// The Python integer value.
    pub fn index(self) -> u8 {
        self as u8
    }
}

/// A version control action that can be taken on a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Compare,
    Update,
    Push,
    Commit,
    Add,
    Remove,
    Revert,
    Resolve,
    /// Git only: unstage changes from the index.
    Unstage,
}

impl Action {
    /// The Python action name (`"compare"`, `"update"`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Action::Compare => "compare",
            Action::Update => "update",
            Action::Push => "push",
            Action::Commit => "commit",
            Action::Add => "add",
            Action::Remove => "remove",
            Action::Revert => "revert",
            Action::Resolve => "resolve",
            Action::Unstage => "unstage",
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One file or folder as seen by a version control backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Absolute, lexically normalised path.
    pub path: PathBuf,
    /// Display name (the file name; the full path for paths that could not
    /// be queried).
    pub name: String,
    pub state: State,
    pub isdir: bool,
    /// Extra per-path information shown in the "Extra" column, e.g.
    /// "Staged", "Rev 12" or "old ➡ new". Multiple items are joined.
    pub options: String,
}

impl Entry {
    pub fn new(
        path: impl Into<PathBuf>,
        name: impl Into<String>,
        state: State,
        isdir: bool,
        options: impl Into<String>,
    ) -> Entry {
        Entry {
            path: path.into(),
            name: name.into(),
            state,
            isdir,
            options: options.into(),
        }
    }

    /// The status label (`Entry.get_status`).
    pub fn status(&self) -> &'static str {
        self.state.label()
    }

    /// Should this entry actually be present on the file system?
    pub fn is_present(&self) -> bool {
        !matches!(self.state, State::Removed | State::Missing)
    }

    /// Filter predicate for the "modified" status filter.
    pub fn is_modified(&self) -> bool {
        self.state >= State::New || (self.isdir && self.state > State::Unversioned)
    }

    /// Filter predicate for the "normal" status filter.
    pub fn is_normal(&self) -> bool {
        self.state == State::Normal
    }

    /// Filter predicate for the "unknown" (non-VC) status filter.
    pub fn is_nonvc(&self) -> bool {
        self.state == State::Unversioned || (self.isdir && self.state > State::Ignored)
    }

    /// Filter predicate for the "ignored" status filter.
    pub fn is_ignored(&self) -> bool {
        self.state == State::Ignored || self.isdir
    }

    /// The parent folder of this entry relative to `vc_location`, as shown
    /// in the "Location" column (`Gio.File.get_relative_path`); `None` when
    /// the parent is not below the location (e.g. the location itself).
    pub fn display_location(&self, vc_location: &Path) -> Option<String> {
        let parent = self.path.parent()?;
        let rel = parent.strip_prefix(vc_location).ok()?;
        if rel.as_os_str().is_empty() {
            None
        } else {
            Some(rel.to_string_lossy().into_owned())
        }
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = match self.status() {
            "" => "Normal",
            s => s,
        };
        write!(f, "<Entry:{} {}>", self.path.display(), status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(state: State, isdir: bool) -> Entry {
        Entry::new("/r/x", "x", state, isdir, "")
    }

    #[test]
    fn state_order_matches_python() {
        assert!(State::Conflict > State::Renamed);
    }

    #[test]
    fn filter_predicates() {
        assert!(e(State::Modified, false).is_modified());
        assert!(e(State::New, false).is_modified());
        assert!(!e(State::Normal, false).is_modified());
        assert!(e(State::Normal, true).is_modified());
        assert!(!e(State::Unversioned, true).is_modified());
        assert!(e(State::Unversioned, false).is_nonvc());
        assert!(e(State::Normal, true).is_nonvc());
        assert!(!e(State::Ignored, true).is_nonvc());
        assert!(e(State::Ignored, false).is_ignored());
        assert!(e(State::Normal, true).is_ignored());
        assert!(!e(State::Removed, false).is_present());
        assert!(!e(State::Missing, false).is_present());
        assert!(e(State::Normal, false).is_normal());
    }

    #[test]
    fn display_location() {
        let entry = Entry::new("/r/a/b/c.txt", "c.txt", State::Normal, false, "");
        assert_eq!(entry.display_location(Path::new("/r")), Some("a/b".into()));
        let entry = Entry::new("/r/c.txt", "c.txt", State::Normal, false, "");
        assert_eq!(entry.display_location(Path::new("/r")), None);
        assert_eq!(entry.to_string(), "<Entry:/r/c.txt Normal>");
    }
}
