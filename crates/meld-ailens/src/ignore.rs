//! Which files are never sent to a model: `.ailensignore` and generated
//! files (port of `ignore.py`)
//!
//! `.ailensignore` uses gitignore syntax. Matching follows git (and the npm
//! `ignore` package AI Lens uses): the last matching pattern wins, `!`
//! re-includes, a trailing `/` matches only directories, a pattern with a
//! slash in it is anchored to the repository root, `**` spans directories,
//! and a file inside an ignored directory can't be re-included. Like
//! `ignore`, matching is case-insensitive.

use regex::Regex;

pub const GENERATED_PATTERNS: [&str; 9] = [
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "npm-shrinkwrap.json",
    "*.min.js",
    "*.min.css",
    "*.map",
    "node_modules/",
];

fn posix_class(name: &str) -> Option<&'static str> {
    Some(match name {
        "alnum" => "a-zA-Z0-9",
        "alpha" => "a-zA-Z",
        "blank" => " \\t",
        "cntrl" => "\\x00-\\x1f\\x7f",
        "digit" => "0-9",
        "graph" => "\\x21-\\x7e",
        "lower" => "a-z",
        "print" => "\\x20-\\x7e",
        "punct" => "!-/:-@\\[-`{-~",
        "space" => " \\t\\n\\r\\x0C\\x0B",
        "upper" => "A-Z",
        "xdigit" => "0-9a-fA-F",
        _ => return None,
    })
}

#[derive(Debug, Clone)]
struct Rule {
    negative: bool,
    /// Matches only directories (the pattern ended with `/`)
    directory: bool,
    /// Matched against the whole path (without its trailing slash); None
    /// never matches
    regex: Option<Regex>,
}

/// Drop trailing spaces, keeping one escaped by a backslash
fn trim_end(body: &str) -> String {
    let body = body.trim_end_matches(['\r', '\n']);
    let trimmed = body.trim_end_matches(' ');
    if trimmed == body {
        return body.to_string();
    }
    let backslashes = trimmed.len() - trimmed.trim_end_matches('\\').len();
    if backslashes % 2 == 1 {
        return format!("{} ", &trimmed[..trimmed.len() - 1]);
    }
    trimmed.to_string()
}

/// Whether `body` ends with an unescaped backslash, which git rejects
fn ends_in_escape(body: &str) -> bool {
    (body.len() - body.trim_end_matches('\\').len()) % 2 == 1
}

fn escape(c: char) -> String {
    regex::escape(c.encode_utf8(&mut [0u8; 4]))
}

enum Piece {
    Source(String),
    /// A bracket that matches nothing
    Never,
}

/// The regex for the bracket expression at `glob[start]`
///
/// Returns it and the index after the closing `]`, or None when the
/// bracket isn't closed.
fn bracket(glob: &[char], start: usize) -> Option<(Piece, usize)> {
    let mut i = start + 1;
    let negated = i < glob.len() && (glob[i] == '!' || glob[i] == '^');
    if negated {
        i += 1;
    }

    let mut members: Vec<String> = Vec::new();
    let mut first = true;
    let mut closed = false;
    while i < glob.len() {
        let mut char = glob[i];
        if char == ']' && !first {
            closed = true;
            break;
        }
        first = false;

        if char == '[' && glob.get(i + 1) == Some(&':') {
            let rest: String = glob[i + 2..].iter().collect();
            if let Some(end) = rest.find(":]") {
                let name = &rest[..end];
                if let Some(class) = posix_class(name) {
                    members.push(class.to_string());
                    i += 2 + name.chars().count() + 2;
                    continue;
                }
            }
        }

        if char == '\\' && i + 1 < glob.len() {
            i += 1;
            char = glob[i];
        }
        let low = char;
        i += 1;

        // A range, unless the dash is the last member
        if i + 1 < glob.len() && glob[i] == '-' && glob[i + 1] != ']' {
            let mut high = glob[i + 1];
            i += 2;
            if high == '\\' && i < glob.len() {
                high = glob[i];
                i += 1;
            }
            // Of a reversed range, git's wildmatch matches only its start
            if low <= high {
                members.push(format!("{}-{}", escape(low), escape(high)));
            } else {
                members.push(escape(low));
            }
            continue;
        }
        members.push(escape(low));
    }
    if !closed {
        return None;
    }

    let body = members.concat();
    if negated {
        // Like `*` and `?`, a bracket never matches a slash
        return Some((Piece::Source(format!("[^/{body}]")), i + 1));
    }
    if body.is_empty() {
        return Some((Piece::Never, i + 1));
    }
    Some((Piece::Source(format!("[[{body}]&&[^/]]")), i + 1))
}

/// Regex source for a gitignore glob, matched against a whole path
///
/// `Err(())` when the glob can never match: git gives up on a bracket that
/// isn't closed. `Ok(None)` for a glob that matches nothing.
fn glob_regex(glob: &str) -> Result<Option<String>, ()> {
    let glob: Vec<char> = glob.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < glob.len() {
        let char = glob[i];
        if char == '*' {
            let mut end = i;
            while end < glob.len() && glob[end] == '*' {
                end += 1;
            }
            let after_slash = i == 0 || glob[i - 1] == '/';
            let before_slash = end == glob.len() || glob[end] == '/';
            if end - i >= 2 && after_slash && before_slash {
                if end == glob.len() {
                    // `a/**` is everything inside `a`; `**` everything
                    out.push_str(if i > 0 { ".+" } else { ".*" });
                    i = end;
                } else {
                    // `**/` is zero or more directories
                    out.push_str("(?:.*/)?");
                    i = end + 1;
                }
            } else {
                out.push_str("[^/]*");
                i = end;
            }
        } else if char == '?' {
            out.push_str("[^/]");
            i += 1;
        } else if char == '[' {
            let (piece, next) = bracket(&glob, i).ok_or(())?;
            match piece {
                Piece::Source(source) => out.push_str(&source),
                Piece::Never => return Ok(None),
            }
            i = next;
        } else if char == '\\' && i + 1 < glob.len() {
            out.push_str(&escape(glob[i + 1]));
            i += 2;
        } else {
            out.push_str(&escape(char));
            i += 1;
        }
    }
    Ok(Some(out))
}

fn parse_rule(pattern: &str) -> Option<Rule> {
    let mut body = pattern
        .strip_prefix('\u{feff}')
        .unwrap_or(pattern)
        .to_string();
    if body.is_empty() || body.starts_with('#') || ends_in_escape(&body) {
        return None;
    }

    let negative = body.starts_with('!');
    if negative {
        body.remove(0);
    }
    if body.ends_with([' ', '\r', '\n']) {
        body = trim_end(&body);
    }
    if body.is_empty() {
        return None;
    }

    // A leading "\!" or "\#" is a literal "!" or "#"
    if body.starts_with("\\!") || body.starts_with("\\#") {
        body.remove(0);
    }

    let directory = body.ends_with('/');
    if directory {
        body.pop();
        if body.is_empty() {
            return None;
        }
    }
    // A slash at the start or in the middle anchors the pattern to the
    // root; otherwise it matches a name at any depth
    let anchored = body.contains('/');
    let body = body.strip_prefix('/').unwrap_or(&body);
    let source = glob_regex(body).ok()?;
    let regex = match source {
        None => None,
        Some(source) => {
            let source = if anchored {
                source
            } else {
                format!("(?:.*/)?{source}")
            };
            Some(Regex::new(&format!(r"(?is)\A(?:{source})\z")).ok()?)
        }
    };
    Some(Rule {
        negative,
        directory,
        regex,
    })
}

/// A list of gitignore patterns; later patterns override earlier ones
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    rules: Vec<Rule>,
}

impl IgnoreRules {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rules from the text of an ignore file
    pub fn from_text(text: &str) -> Self {
        let mut rules = Self::new();
        rules.add_text(text);
        rules
    }

    /// Add the patterns of an ignore file's text
    pub fn add_text(&mut self, text: &str) -> &mut Self {
        let lines: Vec<&str> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        self.add(&lines)
    }

    /// Add patterns, one per item
    pub fn add<S: AsRef<str>>(&mut self, patterns: &[S]) -> &mut Self {
        for pattern in patterns {
            if let Some(rule) = parse_rule(pattern.as_ref()) {
                self.rules.push(rule);
            }
        }
        self
    }

    fn matches(&self, path: &str) -> bool {
        let directory = path.ends_with('/');
        let name = path.trim_end_matches('/');
        let mut ignored = false;
        for rule in &self.rules {
            // Only a rule that could change the outcome is worth testing
            if rule.negative != ignored {
                continue;
            }
            if rule.directory && !directory {
                continue;
            }
            if rule
                .regex
                .as_ref()
                .is_some_and(|regex| regex.is_match(name))
            {
                ignored = !rule.negative;
            }
        }
        ignored
    }

    /// Whether `path` (relative, `/`-separated) is ignored
    ///
    /// A path ending with `/` is a directory. A path in an ignored
    /// directory is ignored whatever later patterns say.
    pub fn ignores(&self, path: &str) -> bool {
        if path.is_empty() {
            return false;
        }
        let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
        for depth in 1..parts.len() {
            if self.matches(&format!("{}/", parts[..depth].join("/"))) {
                return true;
            }
        }
        self.matches(path)
    }
}

/// The built-in rules plus a repo's `.ailensignore` (gitignore syntax)
pub fn create_ignore_rules(ailensignore: Option<&str>) -> IgnoreRules {
    let mut rules = IgnoreRules::new();
    rules.add(&GENERATED_PATTERNS);
    if let Some(text) = ailensignore {
        rules.add_text(text);
    }
    rules
}

/// Whether a changed file is left out, by either of its paths
pub fn is_ignored(rules: &IgnoreRules, path: &str, original_path: Option<&str>) -> bool {
    rules.ignores(path) || original_path.is_some_and(|original| rules.ignores(original))
}
