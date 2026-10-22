//! Gettext `.po` catalogs: parsing, turning the Python version's strings
//! into the Rust app's message syntax, and the compact catalog the app
//! embeds.
//!
//! Std only: `build.rs` and `xtask` compile this file too (`#[path]`).
//!
//! Messages in the Rust app use `format!` syntax: `{}` positional and
//! `{name}` named placeholders, `{{`/`}}` for literal braces, and no GTK
//! mnemonics. The Python strings are converted on the way in:
//!
//! - `python-format` strings: `%s`/`%d`/`%.1f`… become `{}`, `%(name)s`
//!   becomes `{name}`, `%%` a `%`, and braces are escaped;
//! - `python-brace-format` strings keep their fields, minus conversions
//!   and format specs (`{name!r:>5}` becomes `{name}`);
//! - other strings are literal text, so their braces are escaped;
//! - a single mnemonic underscore (`_Save`, `Save _As…`) is dropped from
//!   the message and its translation (also the `(_S)` form CJK
//!   translations use); the message stays available under its original
//!   spelling as well.
//!
//! Translations whose placeholders don't match their message are dropped,
//! so a bad translation shows English rather than a broken text.

use std::collections::{BTreeMap, BTreeSet};

/// One message of a `.po` file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub context: Option<String>,
    pub id: String,
    pub id_plural: Option<String>,
    /// `msgstr`, or `msgstr[0]`, `msgstr[1]`… for plural messages.
    pub strs: Vec<String>,
    /// Flags of `#,` lines, e.g. `fuzzy`, `python-format`.
    pub flags: Vec<String>,
    /// Other comment lines, verbatim (`#.`, `#:`, `# …`, `#|`).
    pub comments: Vec<String>,
}

impl Entry {
    pub fn fuzzy(&self) -> bool {
        self.flags.iter().any(|f| f == "fuzzy")
    }
}

#[derive(Debug, Clone, Default)]
pub struct PoFile {
    /// The header entry (`msgid ""`), if present.
    pub header: Option<Entry>,
    pub entries: Vec<Entry>,
}

impl PoFile {
    /// The header's `msgstr`: `Key: value` lines.
    pub fn header_text(&self) -> &str {
        self.header.as_ref().and_then(|h| h.strs.first()).map(String::as_str).unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    None,
    Context,
    Id,
    IdPlural,
    Str(usize),
}

/// Parses a `.po` file. Obsolete (`#~`) entries are skipped.
pub fn parse(source: &str) -> Result<PoFile, ParseError> {
    let mut file = PoFile::default();
    let mut current = Entry::default();
    // Whether `current` has a msgid (so a new keyword starts a new entry)
    let mut has_id = false;
    let mut has_str = false;
    let mut field = Field::None;

    fn finish(file: &mut PoFile, entry: &mut Entry, has_id: &mut bool, has_str: &mut bool) {
        let done = std::mem::take(entry);
        if *has_id {
            if done.id.is_empty() && done.context.is_none() && file.header.is_none() {
                file.header = Some(done);
            } else {
                file.entries.push(done);
            }
        }
        *has_id = false;
        *has_str = false;
    }

    for (index, raw) in source.lines().enumerate() {
        let lineno = index + 1;
        let line = raw.trim();
        let err = |message: &str| ParseError { line: lineno, message: message.to_string() };
        if line.is_empty() {
            if has_str {
                finish(&mut file, &mut current, &mut has_id, &mut has_str);
            }
            field = Field::None;
            continue;
        }
        if line.starts_with("#~") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            if has_str {
                finish(&mut file, &mut current, &mut has_id, &mut has_str);
            }
            if let Some(flags) = rest.strip_prefix(',') {
                current.flags.extend(flags.split(',').map(|f| f.trim().to_string()).filter(|f| !f.is_empty()));
            } else {
                current.comments.push(line.to_string());
            }
            field = Field::None;
            continue;
        }
        if line.starts_with('"') {
            let value = unquote(line).map_err(|m| err(&m))?;
            match field {
                Field::None => return Err(err("string continuation without a keyword")),
                Field::Context => current.context.get_or_insert_with(String::new).push_str(&value),
                Field::Id => current.id.push_str(&value),
                Field::IdPlural => current.id_plural.get_or_insert_with(String::new).push_str(&value),
                Field::Str(i) => current.strs[i].push_str(&value),
            }
            continue;
        }
        let (keyword, rest) = match line.find(|c: char| c.is_whitespace() || c == '"') {
            Some(i) => (&line[..i], line[i..].trim_start()),
            None => return Err(err("keyword without a string")),
        };
        let value = unquote(rest).map_err(|m| err(&m))?;
        match keyword {
            "msgctxt" => {
                if has_str || has_id {
                    finish(&mut file, &mut current, &mut has_id, &mut has_str);
                }
                current.context = Some(value);
                field = Field::Context;
            }
            "msgid" => {
                if has_str || has_id {
                    finish(&mut file, &mut current, &mut has_id, &mut has_str);
                }
                current.id = value;
                has_id = true;
                field = Field::Id;
            }
            "msgid_plural" => {
                if !has_id {
                    return Err(err("msgid_plural without msgid"));
                }
                current.id_plural = Some(value);
                field = Field::IdPlural;
            }
            "msgstr" => {
                if !has_id {
                    return Err(err("msgstr without msgid"));
                }
                current.strs = vec![value];
                has_str = true;
                field = Field::Str(0);
            }
            k if k.starts_with("msgstr[") && k.ends_with(']') => {
                if !has_id {
                    return Err(err("msgstr without msgid"));
                }
                let i: usize = k[7..k.len() - 1].parse().map_err(|_| err("bad msgstr index"))?;
                if i > 16 {
                    return Err(err("msgstr index too large"));
                }
                if current.strs.len() <= i {
                    current.strs.resize(i + 1, String::new());
                }
                current.strs[i] = value;
                has_str = true;
                field = Field::Str(i);
            }
            _ => return Err(err(&format!("unknown keyword {keyword:?}"))),
        }
    }
    if has_id {
        finish(&mut file, &mut current, &mut has_id, &mut has_str);
    }
    Ok(file)
}

/// The value of a C string literal `"…"` (with gettext's escapes).
fn unquote(s: &str) -> Result<String, String> {
    let s = s.trim();
    let inner = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .filter(|_| s.len() >= 2)
        .ok_or_else(|| format!("expected a quoted string, got {s:?}"))?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            return Err("unescaped quote in string".into());
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('a') => out.push('\u{7}'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('v') => out.push('\u{b}'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('?') => out.push('?'),
            Some('x') => {
                let mut v = 0u32;
                let mut any = false;
                while let Some(d) = chars.peek().and_then(|c| c.to_digit(16)) {
                    v = v * 16 + d;
                    any = true;
                    chars.next();
                }
                if !any {
                    return Err("\\x without digits".into());
                }
                out.push(char::from_u32(v).ok_or("bad \\x escape")?);
            }
            Some(d @ '0'..='7') => {
                let mut v = d.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(v).ok_or("bad octal escape")?);
            }
            Some(other) => return Err(format!("unknown escape \\{other}")),
            None => return Err("string ends with a backslash".into()),
        }
    }
    Ok(out)
}

/// A C string literal for `s`, as `.po` files write them.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// --- Message syntax -------------------------------------------------------

/// A piece of a message in Rust syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    /// A literal `{` or `}` (written doubled).
    Brace(char),
    /// `{}` (`None`, the next argument) or `{2}`.
    Positional(Option<usize>),
    Named(&'a str),
}

/// Splits a message in Rust syntax into pieces; `None` if a brace is
/// unbalanced or a field is neither empty, a number nor a name.
pub fn pieces(template: &str) -> Option<Vec<Piece<'_>>> {
    let mut out = Vec::new();
    let bytes = template.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'}' if bytes.get(i + 1) == Some(&bytes[i]) => {
                if start < i {
                    out.push(Piece::Text(&template[start..i]));
                }
                out.push(Piece::Brace(bytes[i] as char));
                i += 2;
                start = i;
            }
            b'{' => {
                if start < i {
                    out.push(Piece::Text(&template[start..i]));
                }
                let end = i + template[i..].find('}')?;
                let field = template[i + 1..end].trim();
                if field.is_empty() {
                    out.push(Piece::Positional(None));
                } else if field.bytes().all(|b| b.is_ascii_digit()) {
                    out.push(Piece::Positional(Some(field.parse().ok()?)));
                } else if is_identifier(field) {
                    out.push(Piece::Named(field));
                } else {
                    return None;
                }
                i = end + 1;
                start = i;
            }
            b'}' => return None,
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        out.push(Piece::Text(&template[start..]));
    }
    Some(out)
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// The placeholders of a message: the positional argument indices it uses
/// and its names. `None` if it isn't valid syntax.
pub fn placeholders(template: &str) -> Option<(BTreeSet<usize>, BTreeSet<String>)> {
    let mut positional = BTreeSet::new();
    let mut named = BTreeSet::new();
    let mut next = 0;
    for piece in pieces(template)? {
        match piece {
            Piece::Positional(None) => {
                positional.insert(next);
                next += 1;
            }
            Piece::Positional(Some(i)) => {
                positional.insert(i);
            }
            Piece::Named(n) => {
                named.insert(n.to_string());
            }
            _ => {}
        }
    }
    Some((positional, named))
}

/// The syntax a `.po` string is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// Python `%` formatting.
    Percent,
    /// Python `str.format()`.
    Brace,
    /// Already the Rust app's syntax.
    Rust,
    /// Plain text.
    Literal,
}

impl Syntax {
    /// From an entry's flags; strings without a format flag that still
    /// look like `%` formats count as such.
    pub fn of(entry: &Entry) -> Syntax {
        let has = |f: &str| entry.flags.iter().any(|x| x == f);
        if has("rust-format") {
            Syntax::Rust
        } else if has("python-brace-format") {
            Syntax::Brace
        } else if has("python-format") {
            Syntax::Percent
        } else if has("no-python-format") || has("no-rust-format") {
            Syntax::Literal
        } else if looks_like_percent(&entry.id) {
            Syntax::Percent
        } else if from_python(entry) && brace_to_rust(&entry.id).is_some_and(|s| s.contains('{')) {
            // xgettext misses `str.format()` fields of strings formatted
            // after translation
            Syntax::Brace
        } else {
            Syntax::Literal
        }
    }
}

/// Whether all the places an entry comes from are Python files.
fn from_python(entry: &Entry) -> bool {
    let mut refs = entry.comments.iter().filter_map(|c| c.strip_prefix("#:")).flat_map(str::split_whitespace).peekable();
    refs.peek().is_some() && refs.all(|r| r.split(':').next().is_some_and(|f| f.ends_with(".py")))
}

fn looks_like_percent(s: &str) -> bool {
    let b = s.as_bytes();
    (0..b.len()).any(|i| {
        b[i] == b'%' && matches!(b.get(i + 1), Some(b's' | b'd' | b'i' | b'(')) && (i == 0 || b[i - 1] != b'%')
    })
}

/// `s` (written in `syntax`) in Rust syntax; `None` if malformed.
pub fn to_rust(s: &str, syntax: Syntax) -> Option<String> {
    match syntax {
        Syntax::Rust => pieces(s).map(|_| s.to_string()),
        Syntax::Literal => Some(s.replace('{', "{{").replace('}', "}}")),
        Syntax::Percent => percent_to_rust(s),
        Syntax::Brace => brace_to_rust(s),
    }
}

fn percent_to_rust(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        match c {
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            '%' => {
                let Some(&(i, next)) = chars.peek() else {
                    out.push('%');
                    break;
                };
                if next == '%' {
                    chars.next();
                    out.push('%');
                    continue;
                }
                // %(name)… or %N$…
                let mut name = None;
                let mut index = None;
                if next == '(' {
                    let end = i + s[i..].find(')')?;
                    name = Some(&s[i + 1..end]);
                    while chars.peek().is_some_and(|&(j, _)| j <= end) {
                        chars.next();
                    }
                } else {
                    let digits: String = s[i..].chars().take_while(char::is_ascii_digit).collect();
                    if !digits.is_empty() && s[i + digits.len()..].starts_with('$') {
                        index = Some(digits.parse::<usize>().ok()?.checked_sub(1)?);
                        for _ in 0..=digits.len() {
                            chars.next();
                        }
                    }
                }
                // Flags, width, precision, length
                while chars.peek().is_some_and(|&(_, c)| "#0- +.*123456789hlL".contains(c)) {
                    chars.next();
                }
                match chars.next() {
                    Some((_, conv)) if "sdiouxXeEfFgGcra".contains(conv) => {}
                    // Not a conversion: Python would fail; keep the text
                    _ => return None,
                }
                match (name, index) {
                    (Some(n), _) if is_identifier(n) => {
                        out.push('{');
                        out.push_str(n);
                        out.push('}');
                    }
                    (Some(_), _) => return None,
                    (None, Some(i)) => out.push_str(&format!("{{{i}}}")),
                    (None, None) => out.push_str("{}"),
                }
            }
            c => out.push(c),
        }
    }
    Some(out)
}

fn brace_to_rust(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    let mut start = 0;
    while i < b.len() {
        match b[i] {
            b'{' | b'}' if b.get(i + 1) == Some(&b[i]) => i += 2,
            b'{' => {
                out.push_str(&s[start..i]);
                let end = i + s[i..].find('}')?;
                let field = &s[i + 1..end];
                // Drop `!r` conversions, `:spec` and attribute/index access
                let field = field.split(['!', ':', '.', '[']).next().unwrap_or("").trim();
                if !(field.is_empty() || field.bytes().all(|c| c.is_ascii_digit()) || is_identifier(field)) {
                    return None;
                }
                out.push('{');
                out.push_str(field);
                out.push('}');
                i = end + 1;
                start = i;
            }
            b'}' => return None,
            _ => i += 1,
        }
    }
    out.push_str(&s[start..]);
    Some(out)
}

/// The canonical spelling of a message used as catalog key: explicit
/// positional fields that just count up (`{0} of {1}`) become `{}`, as
/// Rust code writes them.
pub fn canonical(template: &str) -> String {
    let Some(pieces) = pieces(template) else {
        return template.to_string();
    };
    let explicit: Vec<usize> = pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Positional(Some(i)) => Some(*i),
            _ => None,
        })
        .collect();
    let auto = pieces.iter().any(|p| matches!(p, Piece::Positional(None)));
    if explicit.is_empty() || auto || !explicit.iter().enumerate().all(|(k, &i)| k == i) {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    for p in pieces {
        match p {
            Piece::Text(t) => out.push_str(t),
            Piece::Brace(c) => {
                out.push(c);
                out.push(c);
            }
            Piece::Positional(_) => out.push_str("{}"),
            Piece::Named(n) => {
                out.push('{');
                out.push_str(n);
                out.push('}');
            }
        }
    }
    out
}

/// Positions of mnemonic underscores outside placeholders: `_` before a
/// letter or digit (`__` is a literal underscore and skipped).
fn mnemonic_positions(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut depth = 0;
    while i < b.len() {
        match b[i] {
            b'{' if b.get(i + 1) == Some(&b'{') => i += 1,
            b'}' if b.get(i + 1) == Some(&b'}') => i += 1,
            b'{' => depth += 1,
            b'}' => depth = 0,
            b'_' if depth == 0 => {
                if b.get(i + 1) == Some(&b'_') {
                    i += 1;
                } else if s[i + 1..].chars().next().is_some_and(char::is_alphanumeric) {
                    out.push(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// `s` without its GTK mnemonic, if it has exactly one: `Save _As…` gives
/// `Save As…`, `保存(_S)` gives `保存`. `None` if there is none.
pub fn strip_mnemonic(s: &str) -> Option<String> {
    // The CJK style: a parenthesised mnemonic after the label
    for (open, close) in [("(_", ")"), ("（_", "）")] {
        if let Some(at) = s.find(open) {
            let rest = &s[at + open.len()..];
            let mut it = rest.chars();
            if let (Some(c), true) = (it.next(), it.as_str().starts_with(close))
                && c.is_alphanumeric()
            {
                let end = at + open.len() + c.len_utf8() + close.len();
                let before = s[..at].trim_end_matches(' ');
                return Some(format!("{before}{}", &s[end..]));
            }
        }
    }
    let positions = mnemonic_positions(s);
    if positions.len() != 1 {
        return None;
    }
    let at = positions[0];
    Some(format!("{}{}", &s[..at], &s[at + 1..]).replace("__", "_"))
}

// --- Catalogs ---------------------------------------------------------------

/// A translated message ready for the app: key in canonical Rust syntax,
/// translations (one per plural form) in Rust syntax.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub context: String,
    pub id: String,
    pub forms: Vec<String>,
}

/// What [`catalog`] skipped, for diagnostics.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Skipped {
    pub fuzzy: usize,
    pub untranslated: usize,
    pub malformed: usize,
}

/// A language's catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub nplurals: usize,
    pub plural: String,
    pub messages: Vec<Message>,
}

/// The app-side form of a message and its translations: (`context`,
/// canonical id, plural id, translations) in Rust syntax with mnemonics
/// stripped from the translations, plus the id with its mnemonic
/// stripped if it had one. `None` if the entry is malformed.
pub fn convert(entry: &Entry) -> Option<(String, Option<String>, Vec<String>)> {
    let syntax = Syntax::of(entry);
    let id = to_rust(&entry.id, syntax)?;
    let plural = match &entry.id_plural {
        Some(p) => Some(to_rust(p, syntax)?),
        None => None,
    };
    let mut strs = Vec::with_capacity(entry.strs.len());
    for s in &entry.strs {
        if s.is_empty() {
            strs.push(String::new());
            continue;
        }
        let s = to_rust(s, syntax)?;
        strs.push(strip_mnemonic(&s).unwrap_or(s));
    }
    Some((id, plural, strs))
}

/// Whether translation `tr` only uses placeholders its message `id` (and
/// plural `plural`) provides.
pub fn placeholders_match(id: &str, plural: Option<&str>, tr: &str) -> bool {
    let Some((pos, named)) = placeholders(tr) else {
        return false;
    };
    let mut have_pos = BTreeSet::new();
    let mut have_named = BTreeSet::new();
    for source in std::iter::once(id).chain(plural) {
        let Some((p, n)) = placeholders(source) else {
            return false;
        };
        have_pos.extend(p);
        have_named.extend(n);
    }
    // A translation may number arguments the message leaves implicit
    let count = have_pos.iter().next_back().map_or(0, |m| m + 1);
    pos.iter().all(|&i| i < count) && named.is_subset(&have_named)
}

/// Builds the catalog of `file`: fuzzy and untranslated entries are left
/// out, so those messages show in English.
pub fn catalog(file: &PoFile) -> Result<(Catalog, Skipped), String> {
    let (nplurals, plural) = super::plural::from_header(file.header_text()).unwrap_or((2, "n != 1".to_string()));
    super::plural::Expr::parse(&plural)?;
    let mut skipped = Skipped::default();
    // Exact spellings win over spellings with the mnemonic dropped
    let mut exact: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut stripped: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for entry in &file.entries {
        if entry.fuzzy() {
            skipped.fuzzy += 1;
            continue;
        }
        let wanted = if entry.id_plural.is_some() { nplurals } else { 1 };
        if entry.strs.len() < wanted || entry.strs.iter().take(wanted).any(String::is_empty) {
            skipped.untranslated += 1;
            continue;
        }
        let Some((id, plural_id, strs)) = convert(entry) else {
            skipped.malformed += 1;
            continue;
        };
        let strs: Vec<String> = strs.into_iter().take(wanted).collect();
        if !strs.iter().all(|s| placeholders_match(&id, plural_id.as_deref(), s)) {
            skipped.malformed += 1;
            continue;
        }
        let context = entry.context.clone().unwrap_or_default();
        if let Some(bare) = strip_mnemonic(&id) {
            stripped.entry((context.clone(), canonical(&bare))).or_insert_with(|| strs.clone());
        }
        exact.insert((context, canonical(&id)), strs);
    }
    for (key, strs) in stripped {
        exact.entry(key).or_insert(strs);
    }
    let messages = exact.into_iter().map(|((context, id), forms)| Message { context, id, forms }).collect();
    Ok((Catalog { nplurals, plural, messages }, skipped))
}

const MAGIC: &[u8; 8] = b"MELDCAT1";

/// The catalog as bytes for the app to embed.
pub fn encode(catalog: &Catalog) -> Vec<u8> {
    fn put(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    let mut out = MAGIC.to_vec();
    put(&mut out, &catalog.plural);
    out.extend_from_slice(&(catalog.nplurals as u32).to_le_bytes());
    out.extend_from_slice(&(catalog.messages.len() as u32).to_le_bytes());
    for m in &catalog.messages {
        put(&mut out, &m.context);
        put(&mut out, &m.id);
        out.extend_from_slice(&(m.forms.len() as u32).to_le_bytes());
        for f in &m.forms {
            put(&mut out, f);
        }
    }
    out
}

/// An encoded catalog read back, borrowing its strings.
pub struct Decoded<'a> {
    pub nplurals: usize,
    pub plural: &'a str,
    /// (context, id, translations)
    pub messages: Vec<(&'a str, &'a str, Vec<&'a str>)>,
}

pub fn decode(bytes: &[u8]) -> Option<Decoded<'_>> {
    struct Reader<'a>(&'a [u8]);
    impl<'a> Reader<'a> {
        fn u32(&mut self) -> Option<usize> {
            let (head, rest) = self.0.split_at_checked(4)?;
            self.0 = rest;
            Some(u32::from_le_bytes(head.try_into().ok()?) as usize)
        }
        fn str(&mut self) -> Option<&'a str> {
            let len = self.u32()?;
            let (head, rest) = self.0.split_at_checked(len)?;
            self.0 = rest;
            std::str::from_utf8(head).ok()
        }
    }
    let mut r = Reader(bytes.strip_prefix(MAGIC.as_slice())?);
    let plural = r.str()?;
    let nplurals = r.u32()?;
    let count = r.u32()?;
    let mut messages = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        let context = r.str()?;
        let id = r.str()?;
        let n = r.u32()?;
        let mut forms = Vec::with_capacity(n.min(16));
        for _ in 0..n {
            forms.push(r.str()?);
        }
        messages.push((context, id, forms));
    }
    Some(Decoded { nplurals, plural, messages })
}
