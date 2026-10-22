//! Type-to-search in tree views (`GtkTreeView`'s interactive search).
//!
//! Typing a letter over a tree opens a small search entry and moves the
//! cursor to the first row whose name starts with the text, ignoring
//! case; when no row starts with it, rows containing it match instead.
//! Up and Down in the entry go to the previous or next match, Return
//! activates the row, and Escape, a click elsewhere or five seconds
//! without typing close it.
//!
//! The views own a [`TreeSearch`], feed it their rows' names with
//! [`find_row`], and route keys with [`start_key`] and [`entry_key`].

use std::time::{Duration, Instant};

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{column, container, row, space, text_input};
use iced::{Element, Length};

/// The id of the search entry (only the visible tab shows one).
pub const INPUT_ID: &str = "tree-search";

/// GTK closes the search entry after this long without typing.
const TIMEOUT: Duration = Duration::from_secs(5);

/// An open search.
#[derive(Debug, Clone)]
pub struct TreeSearch {
    pub text: String,
    last_input: Instant,
}

impl TreeSearch {
    pub fn new(text: String) -> TreeSearch {
        TreeSearch {
            text,
            last_input: Instant::now(),
        }
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.touch();
    }

    /// Keeps the search open for another while.
    pub fn touch(&mut self) {
        self.last_input = Instant::now();
    }

    pub fn expired(&self) -> bool {
        self.last_input.elapsed() >= TIMEOUT
    }
}

/// Where to look for a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The first match from the top (the text changed).
    First,
    /// The next match after the current row.
    Next,
    /// The previous match before the current row.
    Previous,
}

/// What a key press in the open search entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKey {
    Close,
    Step(Step),
    Activate,
}

/// The index of the matching row in `names` (the visible rows, top to
/// bottom), searching from `current` in the direction of `step`. Rows
/// match when their name starts with `key`, ignoring case; if no row
/// does, rows containing `key` match. Searches don't wrap around.
pub fn find_row<S: AsRef<str>>(names: &[S], key: &str, current: Option<usize>, step: Step) -> Option<usize> {
    let key = key.to_lowercase();
    if key.is_empty() {
        return None;
    }
    let folded: Vec<String> = names.iter().map(|n| n.as_ref().to_lowercase()).collect();
    let prefix = folded.iter().any(|n| n.starts_with(&key));
    let matches = |i: &usize| {
        let name = &folded[*i];
        if prefix { name.starts_with(&key) } else { name.contains(&key) }
    };
    match step {
        Step::First => (0..folded.len()).find(matches),
        Step::Next => {
            let start = current.map_or(0, |c| c + 1);
            (start..folded.len()).find(matches)
        }
        Step::Previous => {
            let end = current.unwrap_or(folded.len()).min(folded.len());
            (0..end).rev().find(matches)
        }
    }
}

/// The text a key press over a tree starts a search with: printable
/// characters without Ctrl, Alt or Super, except the tree's own
/// expand/collapse keys and spaces.
pub fn start_key(key: &Key, modifiers: Modifiers) -> Option<String> {
    if modifiers.command() || modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    let Key::Character(c) = key else {
        return None;
    };
    let c = c.as_str();
    if matches!(c, "+" | "-" | "*" | "=" | "/" | "\\") || c.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return None;
    }
    Some(c.to_string())
}

/// Keys the open search entry handles itself, before the text field
/// sees them.
pub fn entry_key(key: &Key, modifiers: Modifiers) -> Option<EntryKey> {
    if modifiers.alt() || modifiers.logo() {
        return None;
    }
    match key {
        Key::Named(Named::Escape) => Some(EntryKey::Close),
        Key::Named(Named::ArrowUp) => Some(EntryKey::Step(Step::Previous)),
        Key::Named(Named::ArrowDown) => Some(EntryKey::Step(Step::Next)),
        Key::Named(Named::Enter) => Some(EntryKey::Activate),
        Key::Character(c) if modifiers.command() && c.eq_ignore_ascii_case("g") => Some(EntryKey::Step(
            if modifiers.shift() { Step::Previous } else { Step::Next },
        )),
        _ => None,
    }
}

/// Draws the search entry over the bottom right corner of `base`.
pub fn overlay<'a, M: Clone + 'a>(
    base: impl Into<Element<'a, M>>,
    search: Option<&TreeSearch>,
    on_input: impl Fn(String) -> M + 'a,
) -> Element<'a, M> {
    let Some(search) = search else {
        return base.into();
    };
    let entry = container(
        text_input(tr!("Search"), &search.text)
            .id(INPUT_ID)
            .on_input(on_input)
            .size(13)
            .padding([4, 8])
            .width(220),
    )
    .padding(4)
    .style(super::popover);
    iced::widget::stack![
        base.into(),
        column![space::vertical(), row![space::horizontal(), entry].padding(10)]
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 6] = ["src", "README.md", "Cargo.toml", "readme.txt", "build.rs", "docs"];

    #[test]
    fn prefix_matches_ignore_case() {
        assert_eq!(find_row(&NAMES, "re", None, Step::First), Some(1));
        assert_eq!(find_row(&NAMES, "RE", Some(1), Step::Next), Some(3));
        assert_eq!(find_row(&NAMES, "re", Some(3), Step::Next), None, "no wrapping");
        assert_eq!(find_row(&NAMES, "re", Some(3), Step::Previous), Some(1));
        assert_eq!(find_row(&NAMES, "re", Some(1), Step::Previous), None);
        assert_eq!(find_row(&NAMES, "c", Some(4), Step::First), Some(2), "changes search from the top");
        assert_eq!(find_row(&NAMES, "", None, Step::First), None);
    }

    #[test]
    fn substrings_match_when_no_name_starts_with_the_text() {
        assert_eq!(find_row(&NAMES, "toml", None, Step::First), Some(2));
        assert_eq!(find_row(&NAMES, ".r", None, Step::First), Some(4));
        // "r" starts names, so only prefixes count: not "src"
        assert_eq!(find_row(&NAMES, "r", None, Step::First), Some(1));
        assert_eq!(find_row(&NAMES, "zzz", None, Step::First), None);
        assert_eq!(find_row(&NAMES, "s", Some(2), Step::Previous), Some(0));
    }

    #[test]
    fn keys() {
        let none = Modifiers::empty();
        assert_eq!(start_key(&Key::Character("a".into()), none).as_deref(), Some("a"));
        assert_eq!(start_key(&Key::Character("A".into()), Modifiers::SHIFT).as_deref(), Some("A"));
        assert_eq!(start_key(&Key::Character("a".into()), Modifiers::CTRL), None);
        assert_eq!(start_key(&Key::Character("+".into()), none), None);
        assert_eq!(start_key(&Key::Character(" ".into()), none), None);
        assert_eq!(start_key(&Key::Named(Named::ArrowDown), none), None);
        assert_eq!(entry_key(&Key::Named(Named::Escape), none), Some(EntryKey::Close));
        assert_eq!(entry_key(&Key::Named(Named::ArrowUp), none), Some(EntryKey::Step(Step::Previous)));
        assert_eq!(entry_key(&Key::Named(Named::Enter), none), Some(EntryKey::Activate));
        assert_eq!(entry_key(&Key::Character("x".into()), none), None);
    }

    #[test]
    fn searches_time_out() {
        let mut search = TreeSearch::new("a".into());
        assert!(!search.expired());
        search.last_input -= TIMEOUT;
        assert!(search.expired());
        search.set_text("ab".into());
        assert!(!search.expired());
    }
}
