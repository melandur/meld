//! Finds `tr!`, `trc!`, `trn!`, `n_!` and `nc_!` invocations in Rust source, like
//! `xgettext` does for other languages: a small lexer skips comments,
//! strings and character literals, and reads the message literals and the
//! names of named arguments.

use crate::po;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub context: Option<String>,
    pub id: String,
    pub plural: Option<String>,
    /// `file:line`.
    pub location: String,
    /// `// Translators: …` comments just above.
    pub comments: Vec<String>,
    /// Which macro (`tr`, `trc`, `trn`, `n_`).
    pub kind: &'static str,
    /// Arguments after the message(s) (for `trn!`, after the count).
    pub positional: usize,
    pub named: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Punct(char),
    /// Anything else (numbers, lifetimes, char literals).
    Other,
}

struct Lexed {
    toks: Vec<(Tok, usize)>,
    /// Line comments: (line, text after `//`).
    comments: Vec<(usize, String)>,
}

fn lex(src: &str) -> Result<Lexed, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut comments = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            let start = i + 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            comments.push((line, chars[start..i].iter().collect()));
        } else if c == '/' && at(i + 1) == '*' {
            let mut depth = 1;
            i += 2;
            while i < chars.len() && depth > 0 {
                if chars[i] == '\n' {
                    line += 1;
                }
                if chars[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if c == '"' || ((c == 'b' || c == 'c') && at(i + 1) == '"') {
            let start_line = line;
            i += if c == '"' { 1 } else { 2 };
            let mut s = String::new();
            loop {
                let Some(&ch) = chars.get(i) else {
                    return Err(format!("line {start_line}: unterminated string"));
                };
                i += 1;
                match ch {
                    '"' => break,
                    '\n' => {
                        line += 1;
                        s.push('\n');
                    }
                    '\\' => {
                        let e = at(i);
                        i += 1;
                        match e {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            '0' => s.push('\0'),
                            '\\' => s.push('\\'),
                            '"' => s.push('"'),
                            '\'' => s.push('\''),
                            'x' => {
                                let hex: String = chars[i..i + 2].iter().collect();
                                i += 2;
                                let v = u32::from_str_radix(&hex, 16)
                                    .map_err(|_| format!("line {line}: bad \\x"))?;
                                s.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                            }
                            'u' => {
                                let end =
                                    i + chars[i..].iter().position(|&c| c == '}').unwrap_or(0);
                                let hex: String = chars[i + 1..end].iter().collect();
                                i = end + 1;
                                let v = u32::from_str_radix(&hex, 16)
                                    .map_err(|_| format!("line {line}: bad \\u"))?;
                                s.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                            }
                            '\n' => {
                                // Line continuation: skip the break and indentation
                                line += 1;
                                while chars.get(i).is_some_and(|c| c.is_whitespace()) {
                                    if chars[i] == '\n' {
                                        line += 1;
                                    }
                                    i += 1;
                                }
                            }
                            other => return Err(format!("line {line}: unknown escape \\{other}")),
                        }
                    }
                    ch => s.push(ch),
                }
            }
            toks.push((Tok::Str(s), start_line));
        } else if (c == 'r' || (c == 'b' && at(i + 1) == 'r')) && {
            let j = if c == 'r' { i + 1 } else { i + 2 };
            let mut k = j;
            while at(k) == '#' {
                k += 1;
            }
            at(k) == '"'
        } {
            let start_line = line;
            i += if c == 'r' { 1 } else { 2 };
            let mut hashes = 0;
            while at(i) == '#' {
                hashes += 1;
                i += 1;
            }
            i += 1;
            let mut s = String::new();
            loop {
                let Some(&ch) = chars.get(i) else {
                    return Err(format!("line {start_line}: unterminated raw string"));
                };
                if ch == '"' && (0..hashes).all(|h| at(i + 1 + h) == '#') {
                    i += 1 + hashes;
                    break;
                }
                if ch == '\n' {
                    line += 1;
                }
                s.push(ch);
                i += 1;
            }
            toks.push((Tok::Str(s), start_line));
        } else if c == '\'' {
            // A char literal ('x', '\n', '\u{..}') or a lifetime/label
            if at(i + 1) == '\\' {
                i += 2;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if at(i + 2) == '\'' {
                i += 3;
            } else {
                i += 1;
                while at(i).is_alphanumeric() || at(i) == '_' {
                    i += 1;
                }
            }
            toks.push((Tok::Other, line));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while at(i).is_alphanumeric() || at(i) == '_' {
                i += 1;
            }
            toks.push((Tok::Ident(chars[start..i].iter().collect()), line));
        } else if c.is_ascii_digit() {
            while at(i).is_alphanumeric()
                || at(i) == '_'
                || (at(i) == '.' && at(i + 1).is_ascii_digit())
            {
                i += 1;
            }
            toks.push((Tok::Other, line));
        } else {
            toks.push((Tok::Punct(c), line));
            i += 1;
        }
    }
    Ok(Lexed { toks, comments })
}

/// The `Translators:` comment block ending just above `line`.
fn translator_comments(comments: &[(usize, String)], line: usize) -> Vec<String> {
    let mut block: Vec<&str> = Vec::new();
    let mut want = line - 1;
    for (l, text) in comments.iter().rev() {
        if *l > want || (*l == line) {
            continue;
        }
        if *l != want {
            break;
        }
        block.push(text.trim());
        want -= 1;
    }
    block.reverse();
    match block
        .iter()
        .position(|t| t.to_ascii_lowercase().starts_with("translators:"))
    {
        Some(start) => vec![block[start..].join(" ")],
        None => Vec::new(),
    }
}

/// The marked messages of one source file.
pub fn extract(src: &str, file: &str) -> Result<Vec<Message>, String> {
    let lexed = lex(src)?;
    let toks = &lexed.toks;
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < toks.len() {
        let kind = match &toks[i].0 {
            Tok::Ident(name) if matches!(name.as_str(), "tr" | "trc" | "trn" | "n_" | "nc_") => {
                name.clone()
            }
            _ => {
                i += 1;
                continue;
            }
        };
        // `tr!(`, not `macro_rules! tr` or `fn tr(`
        let after_macro_rules = i > 1
            && toks[i - 1].0 == Tok::Punct('!')
            && toks[i - 2].0 == Tok::Ident("macro_rules".into());
        if after_macro_rules
            || toks[i + 1].0 != Tok::Punct('!')
            || !matches!(toks[i + 2].0, Tok::Punct('(' | '[' | '{'))
        {
            i += 1;
            continue;
        }
        let line = toks[i].1;
        let location = format!("{file}:{line}");
        // Arguments split at top-level commas
        let mut args: Vec<Vec<Tok>> = vec![Vec::new()];
        let mut depth = 0;
        let mut j = i + 3;
        loop {
            let Some((tok, _)) = toks.get(j) else {
                return Err(format!("{location}: unterminated {kind}!"));
            };
            match tok {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') if depth == 0 => break,
                Tok::Punct(')' | ']' | '}') => depth -= 1,
                Tok::Punct(',') if depth == 0 => {
                    args.push(Vec::new());
                    j += 1;
                    continue;
                }
                _ => {}
            }
            args.last_mut().expect("args").push(tok.clone());
            j += 1;
        }
        if args.last().is_some_and(Vec::is_empty) {
            args.pop();
        }
        let literal = |n: usize| -> Result<String, String> {
            match args.get(n).map(Vec::as_slice) {
                Some([Tok::Str(s)]) => Ok(s.clone()),
                _ => Err(format!(
                    "{location}: {kind}! needs a string literal as argument {}",
                    n + 1
                )),
            }
        };
        let mut message = Message {
            location: location.clone(),
            comments: translator_comments(&lexed.comments, line),
            ..Message::default()
        };
        let rest = match kind.as_str() {
            "tr" => {
                message.kind = "tr";
                message.id = literal(0)?;
                1
            }
            "n_" => {
                message.kind = "n_";
                message.id = literal(0)?;
                1
            }
            "trc" | "nc_" => {
                message.kind = if kind == "trc" { "trc" } else { "nc_" };
                message.context = Some(literal(0)?);
                message.id = literal(1)?;
                2
            }
            _ => {
                message.kind = "trn";
                message.id = literal(0)?;
                message.plural = Some(literal(1)?);
                if args.len() < 3 {
                    return Err(format!("{location}: trn! needs a count"));
                }
                3
            }
        };
        for arg in args.iter().skip(rest) {
            match arg.as_slice() {
                [Tok::Ident(name), Tok::Punct('='), next, ..] if *next != Tok::Punct('=') => {
                    message.named.push(name.clone())
                }
                _ => message.positional += 1,
            }
        }
        out.push(message);
        i = j + 1;
    }
    Ok(out)
}

/// Messages marked in ways the app can't translate.
pub fn problems(messages: &[Message]) -> Vec<String> {
    let mut out = Vec::new();
    for m in messages {
        for text in std::iter::once(&m.id).chain(m.plural.as_ref()) {
            let Some(pieces) = po::pieces(text) else {
                out.push(format!(
                    "{}: {:?}: only `{{}}`, `{{0}}` and `{{name}}` placeholders work (no format specs; `{{{{` for a brace)",
                    m.location, text
                ));
                continue;
            };
            let has_args = m.positional > 0 || !m.named.is_empty();
            for piece in pieces {
                match piece {
                    po::Piece::Named(name) if !m.named.iter().any(|n| n == name) => {
                        out.push(format!(
                            "{}: {:?}: `{{{name}}}` needs an explicit `{name} = …` argument",
                            m.location, text
                        ))
                    }
                    po::Piece::Positional(_) if !has_args => out.push(format!(
                        "{}: {:?}: placeholder without arguments",
                        m.location, text
                    )),
                    _ => {}
                }
            }
        }
        if m.id.is_empty() {
            out.push(format!("{}: empty message", m.location));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_messages_and_arguments() {
        let src = r##"
fn view() {
    // Not this: tr!("commented")
    let a = tr!("Save");
    /* nor tr!("block") */
    // Translators: the number of files
    let b = trn!("{} file", "{} files", n, n);
    let c = trc!("shortcut window", "Quit Meld");
    let d = tr!("{name} changed in {}", path.display(), name = x.y(1, 2));
    let e = tr!(r#"raw "quoted""#);
    let f = tr!("multi \
                 line\n");
    let g: &'static str = n_!("Label"); let g2 = nc_!("state", "Open");
    let h = '"'; let i = tr!("after char");
}
macro_rules! tr { () => {} }
"##;
        let m = extract(src, "x.rs").unwrap();
        let ids: Vec<&str> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "Save",
                "{} file",
                "Quit Meld",
                "{name} changed in {}",
                "raw \"quoted\"",
                "multi line\n",
                "Label",
                "Open",
                "after char"
            ]
        );
        assert_eq!(m[0].location, "x.rs:4");
        assert_eq!(m[1].plural.as_deref(), Some("{} files"));
        assert_eq!(m[1].comments, ["Translators: the number of files"]);
        assert_eq!(m[1].positional, 1);
        assert_eq!(m[2].context.as_deref(), Some("shortcut window"));
        assert_eq!(m[7].context.as_deref(), Some("state"));
        assert_eq!(
            (m[3].positional, m[3].named.clone()),
            (1, vec!["name".to_string()])
        );
        assert!(problems(&m).is_empty(), "{:?}", problems(&m));
    }

    #[test]
    fn reports_bad_markings() {
        let src = r#"
let a = tr!("Hello {name}");
let b = tr!("Width {:>5}", w);
let c = tr!("{who} said", other = 1);
"#;
        let m = extract(src, "x.rs").unwrap();
        assert_eq!(problems(&m).len(), 3, "{:?}", problems(&m));
    }
}
