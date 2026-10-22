//! Translation of Python `re` pattern syntax to Rust regex syntax
//!
//! Meld's settings store regular expressions written for Python's `re`
//! module. Most of that syntax is shared with the `regex` and
//! `fancy-regex` crates; this module rewrites the parts that differ so
//! that patterns valid in Python compile here, and patterns invalid in
//! Python (e.g., `*.foo*`, `\xfoo`) still fail to compile.
//!
//! Handled differences:
//!  * `{` and `}` that don't form a Python quantifier are literals
//!    (`a{`, `x{}`, `{foo}`), and `{,n}` means `{0,n}`
//!  * `\Z` is Python's end-of-text anchor (`\z` in Rust)
//!  * `\<`, `\>` and escaped non-ASCII characters are literals
//!  * `[` inside a character class is a literal, as are `&` and `~`
//!    (which start set operations in Rust)

/// Translate a Python regular expression to Rust syntax
pub(crate) fn translate(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                i += 1;
                match chars.get(i) {
                    None => out.push('\\'),
                    Some(&e) => push_escape(&mut out, e),
                }
                i += 1;
            }
            '[' => {
                i = translate_class(&chars, i, &mut out);
            }
            '{' => {
                if let Some((quantifier, end)) = parse_quantifier(&chars, i) {
                    out.push_str(&quantifier);
                    i = end;
                } else {
                    out.push_str("\\{");
                    i += 1;
                }
            }
            '}' => {
                out.push_str("\\}");
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn push_escape(out: &mut String, e: char) {
    match e {
        'Z' => out.push_str("\\z"),
        '<' | '>' => out.push_str(&format!("\\x{{{:x}}}", e as u32)),
        c if c.is_ascii() => {
            out.push('\\');
            out.push(c);
        }
        c => out.push_str(&regex::escape(&c.to_string())),
    }
}

/// Copy a character class starting at `chars[start] == '['`, returning
/// the index after it
fn translate_class(chars: &[char], start: usize, out: &mut String) -> usize {
    let mut i = start + 1;
    let mut body = String::from("[");
    if chars.get(i) == Some(&'^') {
        body.push('^');
        i += 1;
    }
    // A leading ']' is a literal in Python
    if chars.get(i) == Some(&']') {
        body.push_str("\\]");
        i += 1;
    }
    while i < chars.len() {
        let c = chars[i];
        match c {
            ']' => {
                body.push(']');
                out.push_str(&body);
                return i + 1;
            }
            '\\' => {
                i += 1;
                match chars.get(i) {
                    None => body.push('\\'),
                    Some(&e) => push_escape(&mut body, e),
                }
            }
            '[' | '&' | '~' => {
                body.push('\\');
                body.push(c);
            }
            _ => body.push(c),
        }
        i += 1;
    }
    // Unterminated class; leave it for the regex compiler to reject
    out.push_str(&body);
    chars.len()
}

/// Parse a Python `{m,n}` quantifier at `chars[start] == '{'`
fn parse_quantifier(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start + 1;
    let mut lo = String::new();
    while let Some(c) = chars.get(i).filter(|c| c.is_ascii_digit()) {
        lo.push(*c);
        i += 1;
    }
    let mut hi = None;
    if chars.get(i) == Some(&',') {
        i += 1;
        let mut h = String::new();
        while let Some(c) = chars.get(i).filter(|c| c.is_ascii_digit()) {
            h.push(*c);
            i += 1;
        }
        hi = Some(h);
    }
    if chars.get(i) != Some(&'}') {
        return None;
    }
    // Python treats "{}" as a literal
    if lo.is_empty() && hi.is_none() {
        return None;
    }
    let lo = if lo.is_empty() { "0".to_string() } else { lo };
    let quantifier = match hi {
        None => format!("{{{lo}}}"),
        Some(h) => format!("{{{lo},{h}}}"),
    };
    Some((quantifier, i + 1))
}

#[cfg(test)]
mod tests {
    use super::translate;

    #[test]
    fn translates_python_only_syntax() {
        assert_eq!(translate("x{}"), "x\\{\\}");
        assert_eq!(translate("x{,3}"), "x{0,3}");
        assert_eq!(translate("x{,}"), "x{0,}");
        assert_eq!(translate("x{2,5}"), "x{2,5}");
        assert_eq!(translate("a{"), "a\\{");
        assert_eq!(translate("a{1"), "a\\{1");
        assert_eq!(translate("abc\\Z"), "abc\\z");
        assert_eq!(translate("[[]"), "[\\[]");
        assert_eq!(translate("[]a]"), "[\\]a]");
        assert_eq!(translate("[^]a]"), "[^\\]a]");
        assert_eq!(translate("\\<"), "\\x{3c}");
        assert_eq!(
            translate("\\$\\w+(:[^\\n$]+)?\\$"),
            "\\$\\w+(:[^\\n$]+)?\\$"
        );
    }
}
