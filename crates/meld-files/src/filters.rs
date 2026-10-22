//! Filename and text filters
//!
//! Port of `meld/filters.py`. A [`FilterEntry`] is either a shell-style
//! filename filter ([`FilterKind::Shell`], e.g. `*.{orig,bak} *~`) used
//! to hide files from folder comparisons, or a regular expression text
//! filter ([`FilterKind::Regex`]) whose matches are ignored when
//! comparing text. Patterns follow Python's syntax and validity rules
//! (Python-only syntax is translated); invalid filters are kept but can never be
//! active, as in Meld.

use std::fmt;

use fancy_regex::Regex as FancyRegex;

use crate::pyregex;

/// Which syntax a filter's pattern uses
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterKind {
    /// A Python regular expression, compiled in multi-line mode
    Regex,
    /// Space-separated shell glob patterns matched against file names
    Shell,
}

/// The default `filename-filters` setting: (name, active, pattern)
pub const DEFAULT_FILENAME_FILTERS: &[(&str, bool, &str)] = &[
    ("Backups", true, "#*# .#* ~* *~ *.{orig,bak,swp}"),
    (
        "OS-Specific Metadata",
        true,
        ".DS_Store ._* .Spotlight-V100 .Trashes Thumbs.db Desktop.ini",
    ),
    (
        "Version Control",
        true,
        "_MTN .bzr .svn .svn .hg .fslckout _FOSSIL_ .fos CVS _darcs .git .svn .osc",
    ),
    ("Binaries", true, "*.{pyc,a,obj,o,so,la,lib,dll,exe}"),
    (
        "Media",
        false,
        "*.{jxl,jpg,jpeg,gif,png,avif,webp,heif,heic,bmp,tif,tiff,raw,dng,cr2,wav,wave,mp3,ogg,oga,vorbis,spx,opus,flac,ac3,aac,aif,aiff,aifc,alac,m4a,3gp,wma,aup,aup3,avi,mov,mpg,mpeg,mp4,m4v,webm,ogv,flv,xcf,xpm,ora,kra,psd,psp}",
    ),
];

/// The default `text-filters` setting: (name, active, pattern)
pub const DEFAULT_TEXT_FILTERS: &[(&str, bool, &str)] = &[
    ("CVS/SVN Keywords", false, r"\$\w+(:[^\n$]+)?\$"),
    ("C++ Comment", false, r"//.*"),
    ("C Comment", false, r"/\*.*?\*/"),
    ("All Whitespace", false, r"[ \t\r\f\v]*"),
    ("Leading Whitespace", false, r"^[ \t\r\f\v]*"),
    ("Trailing Whitespace", false, r"[ \t\r\f\v]*$"),
    ("Script Comment", false, r"#.*"),
];

/// A compiled text filter regex, for filtering `str` text
#[derive(Clone)]
pub struct TextRegex {
    regex: FancyRegex,
    groups: usize,
}

impl TextRegex {
    /// Compile a Python regex in multi-line mode
    pub fn new(pattern: &str) -> Option<TextRegex> {
        let translated = format!("(?m){}", pyregex::translate(pattern));
        match FancyRegex::new(&translated) {
            Ok(regex) => {
                let groups = regex.captures_len().saturating_sub(1);
                Some(TextRegex { regex, groups })
            }
            Err(err) => {
                warn_compile(pattern, &err.to_string());
                None
            }
        }
    }

    /// The filtered byte ranges of `text`
    ///
    /// As in Meld, if the regex has no groups each non-empty match is
    /// filtered; otherwise every non-empty group that participated in a
    /// match is. Offsets are byte offsets into `text`.
    pub fn spans(&self, text: &str) -> Vec<(usize, usize)> {
        let mut spans = Vec::new();
        for caps in self.regex.captures_iter(text) {
            let Ok(caps) = caps else {
                // Backtracking limit exceeded; stop filtering this text
                break;
            };
            if self.groups == 0 {
                if let Some(m) = caps.get(0).filter(|m| m.start() != m.end()) {
                    spans.push((m.start(), m.end()));
                }
                continue;
            }
            for i in 1..=self.groups {
                if let Some(m) = caps.get(i).filter(|m| m.start() != m.end()) {
                    spans.push((m.start(), m.end()));
                }
            }
        }
        spans
    }

    /// The compiled (translated) pattern
    pub fn as_str(&self) -> &str {
        self.regex.as_str()
    }

    /// Whether the regex matches anywhere in `text`
    pub fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text).unwrap_or(false)
    }
}

impl fmt::Debug for TextRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TextRegex")
            .field(&self.regex.as_str())
            .finish()
    }
}

/// A compiled text filter regex, for filtering raw file bytes
///
/// This matches Python's bytes patterns (ASCII-only `\w`, `.` matching
/// any byte). Patterns that need backtracking features (look-around,
/// backreferences) are applied to each valid UTF-8 run of the bytes
/// instead.
#[derive(Clone, Debug)]
pub enum ByteRegex {
    /// A byte-oriented regex
    Bytes {
        /// The compiled regex
        regex: regex::bytes::Regex,
        /// Number of capture groups, excluding the whole match
        groups: usize,
    },
    /// A backtracking regex applied to the UTF-8 runs of the bytes
    Text(TextRegex),
}

impl ByteRegex {
    /// Compile a Python regex for matching bytes, in multi-line mode
    pub fn new(pattern: &str) -> Option<ByteRegex> {
        let translated = pyregex::translate(pattern);
        let built = regex::bytes::RegexBuilder::new(&translated)
            .multi_line(true)
            .unicode(false)
            .build();
        match built {
            Ok(regex) => {
                let groups = regex.captures_len().saturating_sub(1);
                Some(ByteRegex::Bytes { regex, groups })
            }
            Err(_) => TextRegex::new(pattern).map(ByteRegex::Text),
        }
    }

    /// The compiled (translated) pattern
    pub fn as_str(&self) -> &str {
        match self {
            ByteRegex::Bytes { regex, .. } => regex.as_str(),
            ByteRegex::Text(text_regex) => text_regex.as_str(),
        }
    }

    /// The filtered byte ranges of `data`; see [`TextRegex::spans`]
    pub fn spans(&self, data: &[u8]) -> Vec<(usize, usize)> {
        match self {
            ByteRegex::Bytes { regex, groups } => {
                let mut spans = Vec::new();
                for caps in regex.captures_iter(data) {
                    if *groups == 0 {
                        if let Some(m) = caps.get(0).filter(|m| m.start() != m.end()) {
                            spans.push((m.start(), m.end()));
                        }
                        continue;
                    }
                    for i in 1..=*groups {
                        if let Some(m) = caps.get(i).filter(|m| m.start() != m.end()) {
                            spans.push((m.start(), m.end()));
                        }
                    }
                }
                spans
            }
            ByteRegex::Text(text_regex) => {
                let mut spans = Vec::new();
                let mut offset = 0;
                for chunk in data.utf8_chunks() {
                    let valid = chunk.valid();
                    spans.extend(
                        text_regex
                            .spans(valid)
                            .into_iter()
                            .map(|(s, e)| (s + offset, e + offset)),
                    );
                    offset += valid.len() + chunk.invalid().len();
                }
                spans
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Compiled {
    Name(regex::Regex),
    Text(TextRegex),
}

/// A named, toggleable filename or text filter
#[derive(Clone, Debug)]
pub struct FilterEntry {
    /// The user-visible name of the filter
    pub label: String,
    /// Whether the filter is applied; always false for invalid filters
    pub active: bool,
    /// The pattern as entered by the user
    pub filter_string: String,
    /// The pattern syntax
    pub kind: FilterKind,
    filter: Option<Compiled>,
    byte_filter: Option<ByteRegex>,
}

impl FilterEntry {
    /// Build a filter from a settings tuple; `new_from_gsetting()`
    ///
    /// Filters whose pattern doesn't compile are inactive.
    pub fn new(label: &str, active: bool, filter_string: &str, kind: FilterKind) -> FilterEntry {
        let (filter, byte_filter) = match kind {
            FilterKind::Regex => (
                TextRegex::new(filter_string).map(Compiled::Text),
                ByteRegex::new(filter_string),
            ),
            FilterKind::Shell => (
                compile_shell_pattern(filter_string).map(Compiled::Name),
                None,
            ),
        };
        FilterEntry {
            label: label.to_string(),
            active: active && filter.is_some(),
            filter_string: filter_string.to_string(),
            kind,
            filter,
            byte_filter,
        }
    }

    /// Whether `filter_string` is a valid pattern of the given kind
    pub fn check_filter(filter_string: &str, kind: FilterKind) -> bool {
        match kind {
            FilterKind::Regex => TextRegex::new(filter_string).is_some(),
            FilterKind::Shell => compile_shell_pattern(filter_string).is_some(),
        }
    }

    /// Whether the pattern compiled
    pub fn is_valid(&self) -> bool {
        self.filter.is_some()
    }

    /// Set whether the filter is applied; invalid filters stay inactive
    pub fn set_active(&mut self, active: bool) {
        self.active = active && self.is_valid();
    }

    /// Whether a shell filter matches the whole of a file name
    ///
    /// This ignores `active`; text filters never match names.
    pub fn matches_name(&self, name: &str) -> bool {
        match &self.filter {
            Some(Compiled::Name(regex)) => regex.is_match(name),
            _ => false,
        }
    }

    /// The compiled regex of a text filter, for `str` text
    pub fn text_regex(&self) -> Option<&TextRegex> {
        match &self.filter {
            Some(Compiled::Text(regex)) => Some(regex),
            _ => None,
        }
    }

    /// The compiled regex of a text filter, for raw bytes
    pub fn byte_regex(&self) -> Option<&ByteRegex> {
        self.byte_filter.as_ref()
    }
}

/// The default filename filters, compiled
pub fn default_filename_filters() -> Vec<FilterEntry> {
    DEFAULT_FILENAME_FILTERS
        .iter()
        .map(|(name, active, pattern)| FilterEntry::new(name, *active, pattern, FilterKind::Shell))
        .collect()
}

/// The default text filters, compiled
pub fn default_text_filters() -> Vec<FilterEntry> {
    DEFAULT_TEXT_FILTERS
        .iter()
        .map(|(name, active, pattern)| FilterEntry::new(name, *active, pattern, FilterKind::Regex))
        .collect()
}

/// Compile a space-separated list of shell patterns into one regex
/// matching whole file names; `None` for empty or invalid patterns
pub fn compile_shell_pattern(pattern: &str) -> Option<regex::Regex> {
    let bits: Vec<&str> = pattern.split_whitespace().collect();
    let regex = match bits.as_slice() {
        // An empty pattern would match everything, so skip it
        [] => return None,
        [single] => shell_to_regex(single),
        _ => {
            let parts: Vec<String> = bits
                .iter()
                .map(|b| strip_dollar(shell_to_regex(b)))
                .collect();
            format!("({})$", parts.join("|"))
        }
    };
    // Python's `match()` anchors at the start only; the pattern itself
    // ends with `$`.
    match regex::Regex::new(&format!("^(?:{regex})")) {
        Ok(compiled) => Some(compiled),
        Err(err) => {
            warn_compile(&regex, &err.to_string());
            None
        }
    }
}

fn strip_dollar(mut s: String) -> String {
    s.pop();
    s
}

/// Translate a shell pattern to a regular expression (Rust syntax)
///
/// Based on `fnmatch.translate()`, but also handling `{a,b,c}`
/// alternatives. The result ends with `$`.
pub fn shell_to_regex(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut res = String::new();
    let find = |from: usize, c: char| (from..n).find(|&k| chars[k] == c);
    while i < n {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                if i < n {
                    res.push_str(&regex::escape(&chars[i].to_string()));
                    i += 1;
                }
            }
            '*' => res.push_str(".*"),
            '?' => res.push('.'),
            '[' => match find(i, ']') {
                None => res.push_str("\\["),
                Some(j) => {
                    let stuff: String = chars[i..j].iter().collect();
                    i = j + 1;
                    let stuff = if let Some(rest) = stuff.strip_prefix('!') {
                        format!("^{rest}")
                    } else if let Some(rest) = stuff.strip_prefix('^') {
                        format!("\\^{rest}")
                    } else {
                        stuff
                    };
                    res.push_str(&pyregex::translate(&format!("[{stuff}]")));
                }
            },
            '{' => match find(i, '}') {
                None => res.push_str("\\{"),
                Some(j) => {
                    let stuff: String = chars[i..j].iter().collect();
                    i = j + 1;
                    let alternatives: Vec<String> = stuff
                        .split(',')
                        .map(|p| strip_dollar(shell_to_regex(p)))
                        .collect();
                    res.push('(');
                    res.push_str(&alternatives.join("|"));
                    res.push(')');
                }
            },
            _ => res.push_str(&regex::escape(&c.to_string())),
        }
    }
    res.push('$');
    res
}

fn warn_compile(pattern: &str, err: &str) {
    log::warn!("Error compiling regex {pattern:?}: {err}");
}

#[cfg(test)]
mod tests {
    use super::*;

    // test/test_filters.py::test_file_filters
    #[test]
    fn file_filters() {
        let cases: &[(&[&str], &str, bool)] = &[
            (&["*.csv"], "foo.csv", true),
            (&["*.cvs"], "foo.csv", false),
            (&["*.csv *.xml"], "foo.csv", true),
            (&["*.csv *.xml"], "foo.xml", true),
            (&["*.csv", "*.xml"], "foo.csv", true),
            (&["*.csv", "*.xml"], "foo.xml", true),
            (&["*.csv", "*.xml"], "dumbtest", false),
            (&["thing*csv"], "thingcsv", true),
            (&["thing*csv"], "thingwhatevercsv", true),
            (&["thing*csv"], "csvthing", false),
        ];
        for (patterns, filename, expected) in cases {
            let filters: Vec<FilterEntry> = patterns
                .iter()
                .map(|p| FilterEntry::new("name", true, p, FilterKind::Shell))
                .collect();
            let matched = filters.iter().any(|f| f.matches_name(filename));
            assert_eq!(matched, *expected, "{patterns:?} {filename}");
        }
    }

    // test/test_filters.py::test_bad_regex_compilation
    #[test]
    fn bad_regex_compilation() {
        for pattern in [r"*.foo*", r"\xfoo"] {
            let f = FilterEntry::new("name", true, pattern, FilterKind::Regex);
            assert!(f.text_regex().is_none(), "{pattern}");
            assert!(!f.active);
            assert!(!FilterEntry::check_filter(pattern, FilterKind::Regex));
        }
    }

    #[test]
    fn python_lenient_regexes_compile() {
        for pattern in [
            "a{", "x{}", "x{,3}", "[[]", r"\<tag\>", "foo\\Z", r"(?<=a)b", r"(\w)\1",
        ] {
            assert!(
                FilterEntry::check_filter(pattern, FilterKind::Regex),
                "{pattern}"
            );
        }
        assert!(!FilterEntry::check_filter("x{2,1}", FilterKind::Regex));
    }

    #[test]
    fn defaults_compile() {
        let names = default_filename_filters();
        assert_eq!(names.len(), 5);
        assert!(names.iter().all(FilterEntry::is_valid));
        assert!(names[0].active && !names[4].active);
        assert!(names[0].matches_name("foo.orig"));
        assert!(names[0].matches_name("foo~"));
        assert!(names[0].matches_name("#foo#"));
        assert!(!names[0].matches_name("foo.origx"));
        assert!(names[2].matches_name(".git"));
        assert!(!names[2].matches_name(".gitignore"));
        assert!(names[3].matches_name("x.pyc"));

        let texts = default_text_filters();
        assert_eq!(texts.len(), 7);
        assert!(
            texts
                .iter()
                .all(|f| f.is_valid() && f.byte_regex().is_some() && !f.active)
        );
    }

    #[test]
    fn shell_patterns() {
        assert_eq!(shell_to_regex("*.txt"), r".*\.txt$");
        assert_eq!(shell_to_regex("*.{a,b}"), r".*\.(a|b)$");
        assert_eq!(shell_to_regex("[!a]"), "[^a]$");
        assert_eq!(shell_to_regex("[^a]"), r"[\^a]$");
        assert_eq!(shell_to_regex("[abc"), r"\[abc$");
        assert!(compile_shell_pattern("   ").is_none());
        let re = compile_shell_pattern("[Mm]akefile").unwrap();
        assert!(re.is_match("Makefile") && re.is_match("makefile") && !re.is_match("xmakefile"));
    }

    #[test]
    fn regex_spans_use_groups() {
        let whole = TextRegex::new(r"#.*").unwrap();
        assert_eq!(whole.spans("a # b\nc"), vec![(2, 5)]);
        let groups = TextRegex::new(r"a(b)?(c)").unwrap();
        assert_eq!(groups.spans("abc ac"), vec![(1, 2), (2, 3), (5, 6)]);
        let multi = TextRegex::new(r"^\s*").unwrap();
        assert_eq!(multi.spans("  a\n b"), vec![(0, 2), (4, 5)]);

        let bytes = ByteRegex::new(r"#.*").unwrap();
        assert_eq!(bytes.spans(b"a #\xff b\nc"), vec![(2, 6)]);
        let fancy = ByteRegex::new(r"(?<=a)b").unwrap();
        assert!(matches!(fancy, ByteRegex::Text(_)));
        assert_eq!(fancy.spans(b"ab\xffab"), vec![(1, 2), (4, 5)]);
    }
}
