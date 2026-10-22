//! Python `shlex.split()` and `shlex.quote()` (POSIX mode)

/// Quote a string for a POSIX shell, like `shlex.quote()`
pub fn quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Split a command line like `shlex.split()`; `None` if a quotation
/// isn't closed or the line ends with an escape
pub fn split(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\\' => {
                word.push(chars.next()?);
                in_word = true;
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let next = chars.next()?;
                            if !matches!(next, '"' | '\\') {
                                word.push('\\');
                            }
                            word.push(next);
                        }
                        c => word.push(c),
                    }
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("/tmp/a_b-c.txt"), "/tmp/a_b-c.txt");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn splitting() {
        assert_eq!(
            split("gedit  +3 'a b'").unwrap(),
            vec!["gedit", "+3", "a b"]
        );
        assert_eq!(
            split(r#"x "a \"q\" \n" c\ d"#).unwrap(),
            vec!["x", "a \"q\" \\n", "c d"]
        );
        assert_eq!(split("''").unwrap(), vec![""]);
        assert!(split("'open").is_none());
        for s in ["a b", "it's", "/x/y z/ü"] {
            assert_eq!(split(&quote(s)).unwrap(), vec![s]);
        }
    }
}
