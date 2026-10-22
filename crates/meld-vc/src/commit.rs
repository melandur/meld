//! Commit dialog logic (the non-GTK parts of `meld/ui/vcdialogs.py` and
//! `meld/ui/historyentry.py`): the list of files to be committed, commit
//! message hard-wrapping at the margin (`textwrap.fill`), and the history
//! of previous commit messages stored in `history.ini`.

use std::path::{Path, PathBuf};

use crate::error::{Result, VcError};
use crate::paths;
use crate::vc::Vc;

/// Shown when committing the selection would commit nothing.
pub const NO_FILES_LABEL: &str = "No files will be committed";

/// Lines listing the files a commit of `selected` would include. Backends
/// that can't tell list the selection relative to its common folder.
pub fn files_to_commit_display(vc: &dyn Vc, selected: &[PathBuf]) -> Result<Vec<String>> {
    match vc.get_files_to_commit(selected) {
        Ok(files) if files.is_empty() => Ok(vec![NO_FILES_LABEL.to_string()]),
        Ok(files) => Ok(files),
        Err(VcError::Unsupported { .. }) => {
            let topdir = paths::lossy(&paths::common_dir(selected));
            let skip = topdir.len() + 1;
            Ok(selected
                .iter()
                .map(|s| {
                    let s = paths::lossy(s);
                    s.get(skip..).unwrap_or("").to_string()
                })
                .collect())
        }
        Err(e) => Err(e),
    }
}

/// Commit message editor settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitSettings {
    /// `vc-show-commit-margin`
    pub show_margin: bool,
    /// `vc-commit-margin`
    pub margin: usize,
    /// `vc-break-commit-message`
    pub break_message: bool,
}

impl Default for CommitSettings {
    fn default() -> Self {
        CommitSettings {
            show_margin: true,
            margin: 72,
            break_message: false,
        }
    }
}

/// The message to commit: hard-wrapped paragraph by paragraph at the
/// margin when both the margin is shown and breaking is enabled (breaking
/// depends on the margin column).
pub fn finish_commit_message(message: &str, settings: &CommitSettings) -> String {
    if settings.show_margin && settings.break_message {
        wrap_commit_message(message, settings.margin)
    } else {
        message.to_string()
    }
}

/// Wrap each `\n\n`-separated paragraph of `message` to `width` columns.
pub fn wrap_commit_message(message: &str, width: usize) -> String {
    message
        .split("\n\n")
        .map(|p| fill(p, width))
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ----- textwrap.fill -----

fn is_letter(c: char) -> bool {
    // `[^\d\W]`
    (c.is_alphanumeric() || c == '_') && !c.is_numeric()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_word_punct(c: char) -> bool {
    // `[\w!"'&.,?]`
    is_word(c) || "!\"'&.,?".contains(c)
}

/// Split a non-whitespace run into chunks, breaking after hyphens of
/// hyphenated words and around em-dashes like `TextWrapper.wordsep_re`.
fn split_word(word: &[char], out: &mut Vec<Vec<char>>) {
    let at = |i: isize| -> Option<char> {
        if i < 0 {
            None
        } else {
            word.get(i as usize).copied()
        }
    };
    let letter = |i: isize| at(i).is_some_and(is_letter);
    let mut start = 0usize;
    let mut i = 0usize;
    while i < word.len() {
        if word[i] == '-' {
            let ii = i as isize;
            // em-dash: (?<=wp) -{2,} (?=\w)
            let mut end = i;
            while end < word.len() && word[end] == '-' {
                end += 1;
            }
            if end - i >= 2
                && i > start
                && at(ii - 1).is_some_and(is_word_punct)
                && word.get(end).copied().is_some_and(is_word)
            {
                out.push(word[start..i].to_vec());
                out.push(word[i..end].to_vec());
                start = end;
                i = end;
                continue;
            }
            // hyphenated word: -(?: (?<=lt{2}-) | (?<=lt-lt-)) (?= lt -? lt)
            let behind = (letter(ii - 2) && letter(ii - 1))
                || (letter(ii - 3) && at(ii - 2) == Some('-') && letter(ii - 1));
            let ahead =
                letter(ii + 1) && (letter(ii + 2) || (at(ii + 2) == Some('-') && letter(ii + 3)));
            if i > start && behind && ahead {
                out.push(word[start..=i].to_vec());
                start = i + 1;
            }
        }
        i += 1;
    }
    if start < word.len() {
        out.push(word[start..].to_vec());
    }
}

fn chunks(text: &str) -> Vec<Vec<char>> {
    // expandtabs(8), then each whitespace character becomes a space
    let mut expanded = Vec::new();
    let mut col = 0usize;
    for c in text.chars() {
        match c {
            '\t' => {
                let n = 8 - col % 8;
                expanded.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\n' | '\r' => {
                expanded.push(' ');
                col = 0;
            }
            '\x0b' | '\x0c' => {
                expanded.push(' ');
                col += 1;
            }
            c => {
                expanded.push(c);
                col += 1;
            }
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < expanded.len() {
        let is_space = expanded[i] == ' ';
        let mut j = i;
        while j < expanded.len() && (expanded[j] == ' ') == is_space {
            j += 1;
        }
        if is_space {
            out.push(expanded[i..j].to_vec());
        } else {
            split_word(&expanded[i..j], &mut out);
        }
        i = j;
    }
    out
}

fn is_blank(chunk: &[char]) -> bool {
    chunk.iter().all(|c| *c == ' ')
}

/// Port of Python's `textwrap.fill(text, width)` with default options.
pub fn fill(text: &str, width: usize) -> String {
    let mut reversed: Vec<Vec<char>> = chunks(text);
    reversed.reverse();
    let mut lines: Vec<String> = Vec::new();
    while !reversed.is_empty() {
        let mut cur: Vec<Vec<char>> = Vec::new();
        let mut cur_len = 0usize;
        // Drop leading whitespace, except at the very beginning
        if !lines.is_empty() && reversed.last().is_some_and(|c| is_blank(c)) {
            reversed.pop();
        }
        while let Some(chunk) = reversed.last() {
            if cur_len + chunk.len() <= width {
                cur_len += chunk.len();
                cur.push(reversed.pop().unwrap_or_default());
            } else {
                break;
            }
        }
        // Handle a word longer than the line
        if reversed.last().is_some_and(|c| c.len() > width) {
            let space_left = if width < 1 { 1 } else { width - cur_len };
            if space_left == 0 {
                if cur.is_empty() {
                    cur.push(reversed.pop().unwrap_or_default());
                }
            } else if let Some(chunk) = reversed.last_mut() {
                let mut end = space_left;
                if chunk.len() > space_left
                    && let Some(h) = chunk[..space_left].iter().rposition(|c| *c == '-')
                    && h > 0
                    && chunk[..h].iter().any(|c| *c != '-')
                {
                    end = h + 1;
                }
                let end = end.min(chunk.len());
                cur.push(chunk[..end].to_vec());
                chunk.drain(..end);
                if chunk.is_empty() {
                    reversed.pop();
                }
            }
        }
        if cur.last().is_some_and(|c| is_blank(c)) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(cur.iter().flatten().collect());
        } else if reversed.is_empty() {
            break;
        }
    }
    lines.join("\n")
}

// ----- Commit message history (HistoryCombo) -----

/// Messages of this length or shorter are not remembered.
pub const MIN_ITEM_LEN: usize = 3;
/// Default `history-length`.
pub const DEFAULT_HISTORY_LENGTH: usize = 10;
/// The `history-id` of the commit dialog's history.
pub const COMMIT_HISTORY_ID: &str = "commit-messages";

/// The history of previous commit messages ("Previous logs"), most recent
/// first, persisted in `history.ini` like Python's `HistoryCombo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageHistory {
    messages: Vec<String>,
    length: usize,
}

impl Default for MessageHistory {
    fn default() -> Self {
        MessageHistory::new(DEFAULT_HISTORY_LENGTH)
    }
}

impl MessageHistory {
    /// An empty history; `length` is clamped to 1..=20 like the GObject
    /// property. At most `length - 1` messages are kept.
    pub fn new(length: usize) -> MessageHistory {
        MessageHistory {
            messages: Vec::new(),
            length: length.clamp(1, 20),
        }
    }

    /// Stored messages, most recent first.
    pub fn messages(&self) -> &[String] {
        &self.messages
    }

    /// First lines of the messages, as shown in the combo box.
    pub fn summaries(&self) -> Vec<String> {
        self.messages
            .iter()
            .map(|m| m.lines().next().unwrap_or_default().to_string())
            .collect()
    }

    /// Remember `text` as the most recent message.
    pub fn prepend(&mut self, text: &str) {
        self.insert(text, true);
    }

    /// Remember `text` as the oldest message.
    pub fn append(&mut self, text: &str) {
        self.insert(text, false);
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    fn insert(&mut self, text: &str, prepend: bool) {
        if text.chars().count() <= MIN_ITEM_LEN {
            return;
        }
        if let Some(i) = self.messages.iter().position(|m| m == text) {
            self.messages.remove(i);
        } else if self.length >= 2 {
            self.messages.truncate(self.length - 2);
        }
        if prepend {
            self.messages.insert(0, text.to_string());
        } else {
            self.messages.push(text.to_string());
        }
    }

    /// `$XDG_CONFIG_HOME/meld/history.ini`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("meld").join("history.ini"))
    }

    /// Load the `section` (normally [`COMMIT_HISTORY_ID`]) of the ini file
    /// at `path`; a missing file gives an empty history.
    pub fn load(path: &Path, section: &str, length: usize) -> Result<MessageHistory> {
        let mut history = MessageHistory::new(length);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(history),
            Err(e) => return Err(e.into()),
        };
        let ini = Ini::parse(&text);
        if let Some((_, items)) = ini.sections.iter().find(|(name, _)| name == section) {
            let mut items = items.clone();
            items.sort();
            history.messages = items
                .into_iter()
                .take(history.length - 1)
                .map(|(_, v)| unicode_unescape(&v))
                .filter(|m| !m.is_empty())
                .collect();
        }
        Ok(history)
    }

    /// Save into `section` of the ini file at `path`, keeping other
    /// sections. Parent folders are created.
    pub fn save(&self, path: &Path, section: &str) -> Result<()> {
        let mut ini = match std::fs::read_to_string(path) {
            Ok(t) => Ini::parse(&t),
            Err(_) => Ini::default(),
        };
        let items: Vec<(String, String)> = self
            .messages
            .iter()
            .enumerate()
            .map(|(i, m)| (format!("item{i}"), unicode_escape(m)))
            .collect();
        ini.sections.retain(|(name, _)| name != section);
        ini.sections.push((section.to_string(), items));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, ini.to_string())?;
        Ok(())
    }
}

/// Minimal `configparser.RawConfigParser` compatible ini handling.
#[derive(Debug, Default)]
struct Ini {
    sections: Vec<(String, Vec<(String, String)>)>,
}

impl Ini {
    fn parse(text: &str) -> Ini {
        let mut ini = Ini::default();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                ini.sections
                    .push((trimmed[1..trimmed.len() - 1].to_string(), Vec::new()));
                continue;
            }
            let Some((_, items)) = ini.sections.last_mut() else {
                continue;
            };
            let Some(pos) = trimmed.find(['=', ':']) else {
                continue;
            };
            let key = trimmed[..pos].trim().to_lowercase();
            let value = trimmed[pos + 1..].trim().to_string();
            items.retain(|(k, _)| *k != key);
            items.push((key, value));
        }
        ini
    }
}

impl std::fmt::Display for Ini {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (name, items) in &self.sections {
            writeln!(f, "[{name}]")?;
            for (k, v) in items {
                writeln!(f, "{k} = {v}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

/// Python's `str.encode("unicode-escape")`.
pub fn unicode_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ' '..='~' => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out
}

/// Python's `.decode("unicode-escape")` (unknown escapes are kept).
pub fn unicode_unescape(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let hex = |from: usize, n: usize| -> Option<char> {
        let digits: String = chars.get(from..from + n)?.iter().collect();
        char::from_u32(u32::from_str_radix(&digits, 16).ok()?)
    };
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = chars[i + 1];
        let simple = match n {
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            'a' => Some('\x07'),
            'b' => Some('\x08'),
            'f' => Some('\x0c'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\x0b'),
            _ => None,
        };
        if let Some(ch) = simple {
            out.push(ch);
            i += 2;
            continue;
        }
        let (len, decoded) = match n {
            'x' => (2, hex(i + 2, 2)),
            'u' => (4, hex(i + 2, 4)),
            'U' => (8, hex(i + 2, 8)),
            '0'..='7' => {
                let mut j = i + 1;
                let mut v = 0u32;
                while j < chars.len() && j < i + 4 && ('0'..='7').contains(&chars[j]) {
                    v = v * 8 + chars[j].to_digit(8).unwrap_or(0);
                    j += 1;
                }
                (j - i - 2, char::from_u32(v))
            }
            _ => (0, None),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                i += 2 + len;
            }
            None => {
                out.push('\\');
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_matches_textwrap() {
        // Expected values produced with Python's textwrap.fill
        assert_eq!(
            fill("The quick brown fox jumps over the lazy dog", 10),
            "The quick\nbrown fox\njumps over\nthe lazy\ndog"
        );
        assert_eq!(fill("a\nb  c\td", 72), "a b  c    d");
        assert_eq!(fill("", 72), "");
        assert_eq!(fill("   ", 72), "");
        assert_eq!(fill("  indented start", 8), "indented\nstart");
        assert_eq!(fill("abcdefghijklmnop", 5), "abcde\nfghij\nklmno\np");
        assert_eq!(
            fill("well-known long-standing issue", 12),
            "well-known\nlong-\nstanding\nissue"
        );
        assert_eq!(fill("x-ray e-mail", 4), "x-\nray\ne-\nmail");
        assert_eq!(fill("see this--that thing", 9), "see this\n--that\nthing");
    }

    #[test]
    fn wraps_paragraphs() {
        let msg =
            "Short summary\n\nA body paragraph that is long enough to need wrapping at the margin.";
        assert_eq!(
            wrap_commit_message(msg, 30),
            "Short summary\n\nA body paragraph that is long\nenough to need wrapping at the\nmargin."
        );
        let s = CommitSettings {
            break_message: true,
            ..CommitSettings::default()
        };
        assert_eq!(finish_commit_message("a\nb", &s), "a b");
        assert_eq!(
            finish_commit_message("a\nb", &CommitSettings::default()),
            "a\nb"
        );
    }

    #[test]
    fn history_insertion_rules() {
        let mut h = MessageHistory::new(4);
        h.prepend("abc"); // too short
        assert!(h.messages().is_empty());
        for m in ["one!", "two!", "three", "four"] {
            h.prepend(m);
        }
        assert_eq!(h.messages(), ["four", "three", "two!"]);
        h.prepend("two!");
        assert_eq!(h.messages(), ["two!", "four", "three"]);
        h.prepend("multi\nline");
        assert_eq!(h.summaries(), ["multi", "two!", "four"]);
    }

    #[test]
    fn history_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meld/history.ini");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[other]\nitem0 = keep me\n\n").unwrap();
        let mut h = MessageHistory::default();
        h.prepend("first: message\nwith \\ body ü 😀");
        h.prepend("second message");
        h.save(&path, COMMIT_HISTORY_ID).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[other]\nitem0 = keep me"));
        assert!(text.contains("item1 = first: message\\nwith \\\\ body \\xfc \\U0001f600"));
        let loaded = MessageHistory::load(&path, COMMIT_HISTORY_ID, 10).unwrap();
        assert_eq!(loaded.messages(), h.messages());
        let missing =
            MessageHistory::load(&dir.path().join("nope.ini"), COMMIT_HISTORY_ID, 10).unwrap();
        assert!(missing.messages().is_empty());
    }

    #[test]
    fn unescape_python_forms() {
        assert_eq!(unicode_unescape("a\\nb\\x41\\u00e9\\101\\q"), "a\nbAéA\\q");
    }
}
