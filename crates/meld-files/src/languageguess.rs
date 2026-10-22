//! Guess syntax highlighting languages for compared files
//!
//! Port of `meld/languageguess.py`, plus the parts of GtkSourceView's
//! language manager it relied on (file name globs and content type
//! sniffing; see [`crate::languages`]).
//!
//! A file name often fails to identify a language (e.g., blank
//! comparisons, version control temporary files or scripts without an
//! extension). The helpers here fill that gap from the text itself, by
//! looking for editor modelines, shebang lines and finally content
//! sniffing, and keep the languages of the panes in a comparison
//! consistent. Results are GtkSourceView language IDs.

use std::path::Path;
use std::sync::OnceLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;

use crate::languages::{self, LanguageSpec};

/// Number of lines at the start and end of a file searched for
/// modelines; this matches Vim's default 'modelines' setting
pub const MODELINE_LINES: usize = 5;

/// Maximum number of bytes used for content type sniffing
pub const CONTENT_SAMPLE_SIZE: usize = 4096;

/// Lines from the start of a file used to guess its language
pub const LANGUAGE_SAMPLE_HEAD_LINES: usize = 100;
/// Lines from the end of a file used to guess its language (for
/// modelines)
pub const LANGUAGE_SAMPLE_TAIL_LINES: usize = 20;
/// Limit on sampled characters, in case of e.g., minified files
pub const LANGUAGE_SAMPLE_MAX_CHARS: usize = 64 * 1024;

/// Editor mode and interpreter names that don't match GtkSourceView's
/// language IDs. Anything not listed here is tried as an ID directly.
pub const LANGUAGE_ALIASES: &[(&str, &str)] = &[
    ("ash", "sh"),
    ("bash", "sh"),
    ("bats", "sh"),
    ("bun", "js"),
    ("c++", "cpp"),
    ("cperl", "perl"),
    ("cs", "c-sharp"),
    ("csharp", "c-sharp"),
    ("cxx", "cpp"),
    ("dash", "sh"),
    ("deno", "js"),
    ("dockerfile", "docker"),
    ("dosini", "ini"),
    ("emacs-lisp", "commonlisp"),
    ("escript", "erlang"),
    ("gawk", "awk"),
    ("gfm", "markdown"),
    ("gmake", "makefile"),
    ("golang", "go"),
    ("javascript", "js"),
    ("jsonc", "json"),
    ("ksh", "sh"),
    ("lisp", "commonlisp"),
    ("make", "makefile"),
    ("mawk", "awk"),
    ("md", "markdown"),
    ("mksh", "sh"),
    ("nawk", "awk"),
    ("node", "js"),
    ("nodejs", "js"),
    ("objective-c", "objc"),
    ("patch", "diff"),
    ("pwsh", "powershell"),
    ("ps1", "powershell"),
    ("py", "python3"),
    ("python", "python3"),
    ("python2", "python"),
    ("rb", "ruby"),
    ("rs", "rust"),
    ("rscript", "r"),
    ("runghc", "haskell"),
    ("runhaskell", "haskell"),
    ("shell", "sh"),
    ("shell-script", "sh"),
    ("tclsh", "tcl"),
    ("tex", "latex"),
    ("ts", "typescript"),
    ("wish", "tcl"),
    ("yml", "yaml"),
    ("zsh", "sh"),
];

struct Patterns {
    vim_modeline: FancyRegex,
    emacs_modeline: Regex,
    emacs_mode: Regex,
    emacs_local_mode: Regex,
    shebang: Regex,
    version_suffix: Regex,
    minor_version_suffix: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns {
        // Vim modelines look like "vim: set ft=python :" or
        // "vi:syntax=c ts=4"; we only care about the file type.
        vim_modeline: FancyRegex::new(
            r"(?:^|\s)(?:vi|vim|ex)(?:[<=>]?\d+)?:.*?(?<![\w])(?:ft|filetype|syn|syntax)=(?P<name>[\w+.#-]+)",
        )
        .expect("valid regex"),
        // Emacs modelines look like "-*- mode: python; coding: utf-8 -*-"
        // or simply "-*- python -*-"
        emacs_modeline: Regex::new(r"-\*-(?P<vars>.*?)-\*-").expect("valid regex"),
        emacs_mode: Regex::new(r"(?i)(?:^|;)\s*mode\s*:\s*(?P<name>[\w+.#-]+)").expect("valid regex"),
        emacs_local_mode: Regex::new(r"(?i)\bmode\s*:\s*(?P<name>[\w+.#-]+)").expect("valid regex"),
        shebang: Regex::new(r"^#!\s*(?P<command>.*)$").expect("valid regex"),
        version_suffix: Regex::new(r"[.-]?\d+(?:\.\d+)*$").expect("valid regex"),
        minor_version_suffix: Regex::new(r"(?:\.\d+)+$").expect("valid regex"),
    })
}

/// Split lines like Python's `str.splitlines()`
pub fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        lines.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' && chars.peek().map(|(_, n)| *n) == Some('\n') {
            chars.next();
            end += 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Whether `id` is a language in the embedded table
pub fn is_known_language(id: &str) -> bool {
    languages::language(id).is_some()
}

/// Map an editor mode or interpreter name to a known language ID
///
/// Names are matched case-insensitively, with Emacs' "-mode" suffix and
/// version suffixes (e.g., "python3.12", "lua5.4") removed if the full
/// name isn't recognised. `is_known` decides which IDs exist; use
/// [`is_known_language`] for the embedded table.
pub fn normalise_language_name(name: &str, is_known: impl Fn(&str) -> bool) -> Option<String> {
    let mut name = name.trim().to_lowercase();
    if let Some(stripped) = name.strip_suffix("-mode") {
        name = stripped.to_string();
    }
    if name.is_empty() {
        return None;
    }

    let p = patterns();
    let mut candidates = vec![name.clone()];
    let without_minor = p.minor_version_suffix.replace_all(&name, "").into_owned();
    let without_version = p.version_suffix.replace_all(&name, "").into_owned();
    for candidate in [without_minor, without_version] {
        if !candidate.is_empty() && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    candidates.into_iter().find_map(|candidate| {
        let id = LANGUAGE_ALIASES
            .iter()
            .find(|(alias, _)| *alias == candidate)
            .map(|(_, id)| id.to_string())
            .unwrap_or(candidate);
        is_known(&id).then_some(id)
    })
}

/// Find a Vim or Emacs mode name in the text's modelines, if any
pub fn language_name_from_modeline(text: &str) -> Option<String> {
    let p = patterns();
    let lines = py_splitlines(text);
    let head = &lines[..lines.len().min(MODELINE_LINES)];
    let tail: &[&str] = if lines.len() > MODELINE_LINES {
        &lines[lines.len() - MODELINE_LINES..]
    } else {
        &[]
    };

    for line in head.iter().chain(tail) {
        if let Ok(Some(caps)) = p.vim_modeline.captures(line)
            && let Some(name) = caps.name("name")
        {
            return Some(name.as_str().to_string());
        }
    }

    // Emacs only reads its modeline from the first line, or from the
    // second if the first is a shebang.
    for line in &head[..head.len().min(2)] {
        let Some(caps) = p.emacs_modeline.captures(line) else {
            continue;
        };
        let variables = caps.name("vars").map_or("", |m| m.as_str());
        if !variables.contains(':') {
            let trimmed = variables.trim();
            return (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
        if let Some(mode) = p
            .emacs_mode
            .captures(variables)
            .and_then(|c| c.name("name"))
        {
            return Some(mode.as_str().to_string());
        }
    }

    // Emacs also supports a "Local Variables:" block near the end of
    // the file.
    let mut in_local_variables = false;
    for line in &lines[lines.len().saturating_sub(20)..] {
        if line.contains("Local Variables:") {
            in_local_variables = true;
        } else if line.contains("End:") {
            in_local_variables = false;
        } else if in_local_variables
            && let Some(mode) = p
                .emacs_local_mode
                .captures(line)
                .and_then(|c| c.name("name"))
        {
            return Some(mode.as_str().to_string());
        }
    }

    None
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Get the interpreter name from a shebang line
///
/// This handles plain paths (`#!/bin/bash`), arguments
/// (`#!/usr/bin/perl -w`) and env (`#!/usr/bin/env -S python3 -u`).
pub fn interpreter_from_shebang(line: &str) -> Option<String> {
    let caps = patterns().shebang.captures(line.trim())?;
    let command = caps.name("command").map_or("", |m| m.as_str());
    let mut words = command.split_whitespace();
    let interpreter = basename(words.next()?);
    if interpreter != "env" {
        return (!interpreter.is_empty()).then(|| interpreter.to_string());
    }

    // Skip env's options and environment assignments to find the
    // actual command
    while let Some(arg) = words.next() {
        if matches!(arg, "-u" | "--unset" | "-C" | "--chdir") {
            words.next();
        } else if arg.starts_with('-') || arg.contains('=') {
            continue;
        } else {
            let name = basename(arg);
            return (!name.is_empty()).then(|| name.to_string());
        }
    }
    None
}

/// Guess a language ID from modelines and shebangs in the text
///
/// Modelines take precedence, since they're an explicit statement of
/// the file type by the user.
pub fn language_id_from_text(text: &str, is_known: impl Fn(&str) -> bool) -> Option<String> {
    if let Some(mode) = language_name_from_modeline(text)
        && let Some(id) = normalise_language_name(&mode, &is_known)
    {
        return Some(id);
    }

    let first_line = text.split('\n').next().unwrap_or("");
    let interpreter = interpreter_from_shebang(first_line)?;
    normalise_language_name(&interpreter, &is_known)
}

/// Sniff a MIME type from the start of a text, like GIO's
/// `g_content_type_guess()` for the text formats that matter here
///
/// Returns `None` for plain text.
pub fn sniff_content_type(sample: &[u8]) -> Option<&'static str> {
    let sample = &sample[..sample.len().min(CONTENT_SAMPLE_SIZE)];
    let sample = sample.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(sample);
    let starts = |prefix: &[u8]| sample.starts_with(prefix);
    let trimmed = sample.trim_ascii_start();
    let starts_ci = |prefix: &[u8]| {
        trimmed.len() >= prefix.len() && trimmed[..prefix.len()].eq_ignore_ascii_case(prefix)
    };
    let contains = |needle: &[u8]| {
        sample[..sample.len().min(256)]
            .windows(needle.len())
            .any(|w| w == needle)
    };

    if starts(b"<?xml") {
        return Some(if contains(b"<svg") {
            "image/svg+xml"
        } else {
            "application/xml"
        });
    }
    if starts_ci(b"<!doctype html") || starts_ci(b"<html") {
        return Some("text/html");
    }
    if starts(b"<svg") {
        return Some("image/svg+xml");
    }
    if starts(b"<?php") {
        return Some("application/x-php");
    }
    if starts(b"[Desktop Entry]") {
        return Some("application/x-desktop");
    }
    if starts(b"%YAML") {
        return Some("application/yaml");
    }
    if starts(b"\\documentclass") {
        return Some("text/x-tex");
    }
    let patch_prefixes: [&[u8]; 7] = [
        b"diff --git ",
        b"diff -",
        b"--- ",
        b"*** ",
        b"Only in ",
        b"Index: ",
        b"Common subdirectories: ",
    ];
    if patch_prefixes.iter().any(|p| starts(p)) {
        return Some("text/x-patch");
    }
    None
}

/// Guess a language for text whose file name gave no answer
pub fn guess_language_from_text(text: &str) -> Option<&'static LanguageSpec> {
    if text.is_empty() {
        return None;
    }
    if let Some(id) = language_id_from_text(text, is_known_language) {
        return languages::language(&id);
    }
    // Fall back to content sniffing (e.g., XML declarations)
    sniff_content_type(text.as_bytes()).and_then(languages::language_for_mime_type)
}

/// Guess a language from a file's name, as GtkSourceView does
///
/// Missing files give no language. If the name doesn't identify a
/// language, the start of the file is sniffed for a content type.
pub fn language_from_path(path: &Path) -> Option<&'static LanguageSpec> {
    let meta = std::fs::metadata(path).ok()?;
    let name = path.file_name()?.to_string_lossy();
    if let Some(lang) = languages::language_for_filename(&name) {
        return Some(lang);
    }
    if !meta.is_file() {
        return None;
    }
    use std::io::Read;
    let mut sample = Vec::with_capacity(CONTENT_SAMPLE_SIZE);
    std::fs::File::open(path)
        .ok()?
        .take(CONTENT_SAMPLE_SIZE as u64)
        .read_to_end(&mut sample)
        .ok()?;
    sniff_content_type(&sample).and_then(languages::language_for_mime_type)
}

/// Decide on the language of each pane in a comparison
///
/// `file_languages` holds the languages guessed from each pane's file;
/// `guess_from_text` takes a pane index and guesses its language from
/// the pane's text. It is only called for panes whose file gave no
/// language, and only when needed.
///
/// If every pane whose file gave a language agrees on it, all panes use
/// that language. Otherwise, panes without a file language are guessed
/// from their text, and if all known languages then agree, that
/// language is used for every pane. Panes with conflicting languages
/// (e.g., a C file compared with a C header) keep their own.
pub fn choose_pane_languages<T: Clone + PartialEq>(
    file_languages: &[Option<T>],
    mut guess_from_text: impl FnMut(usize) -> Option<T>,
) -> Vec<Option<T>> {
    fn consensus<T: Clone + PartialEq>(langs: &[Option<T>]) -> Option<T> {
        let mut known = langs.iter().flatten();
        let first = known.next()?;
        known.all(|l| l == first).then(|| first.clone())
    }

    if let Some(agreed) = consensus(file_languages) {
        return vec![Some(agreed); file_languages.len()];
    }

    let langs: Vec<Option<T>> = file_languages
        .iter()
        .enumerate()
        .map(|(i, lang)| lang.clone().or_else(|| guess_from_text(i)))
        .collect();
    match consensus(&langs) {
        Some(agreed) => vec![Some(agreed); langs.len()],
        None => langs,
    }
}

fn truncate_chars(s: &str, limit: usize) -> &str {
    match s.char_indices().nth(limit) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn last_chars(s: &str, limit: usize) -> &str {
    let count = s.chars().count();
    if count <= limit {
        return s;
    }
    match s.char_indices().nth(count - limit) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

/// Byte offsets of the start of each line (`\n`, `\r\n` or `\r`)
fn line_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = vec![0];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => starts.push(i + 1),
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                starts.push(i + 1);
            }
            _ => {}
        }
        i += 1;
    }
    starts
}

/// Get text from the start and end of a document for language guessing
///
/// This includes enough lines from the end for modelines, while
/// avoiding copying the text of large files; `get_language_sample_text()`
/// in `meld/filediff.py`.
pub fn get_language_sample_text(text: &str) -> String {
    let starts = line_starts(text);
    let line_count = starts.len();
    let (head_end, tail_start) =
        if line_count <= LANGUAGE_SAMPLE_HEAD_LINES + LANGUAGE_SAMPLE_TAIL_LINES {
            (text.len(), text.len())
        } else {
            (
                starts[LANGUAGE_SAMPLE_HEAD_LINES],
                starts[line_count - LANGUAGE_SAMPLE_TAIL_LINES],
            )
        };

    let mut head = truncate_chars(&text[..head_end], LANGUAGE_SAMPLE_MAX_CHARS).to_string();
    let tail = last_chars(&text[tail_start..], LANGUAGE_SAMPLE_MAX_CHARS);
    if !head.is_empty() && !tail.is_empty() && !head.ends_with('\n') {
        head.push('\n');
    }
    head.push_str(tail);
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpreter_from_shebang_cases() {
        let cases = [
            ("#!/bin/bash", Some("bash")),
            ("#!/bin/sh -e", Some("sh")),
            ("#! /usr/bin/perl -w", Some("perl")),
            ("#!/usr/bin/env python3", Some("python3")),
            ("#!/usr/bin/env python3.12", Some("python3.12")),
            ("#!/usr/bin/env -S python3 -u", Some("python3")),
            ("#!/usr/bin/env -S LANG=C ruby --disable-gems", Some("ruby")),
            ("#!/usr/bin/env -u PYTHONPATH python3", Some("python3")),
            ("#!/usr/local/bin/node", Some("node")),
            ("#!/usr/bin/env", None),
            ("#!", None),
            ("# just a comment", None),
            ("", None),
        ];
        for (line, expected) in cases {
            assert_eq!(
                interpreter_from_shebang(line).as_deref(),
                expected,
                "{line}"
            );
        }
    }

    #[test]
    fn normalise_language_name_cases() {
        let cases = [
            ("python", Some("python3")),
            ("Python", Some("python3")),
            ("python3", Some("python3")),
            ("python3.12", Some("python3")),
            ("python2", Some("python")),
            ("python2.7", Some("python")),
            ("bash", Some("sh")),
            ("zsh", Some("sh")),
            ("sh", Some("sh")),
            ("node", Some("js")),
            ("javascript", Some("js")),
            ("perl", Some("perl")),
            ("perl5", Some("perl")),
            ("lua5.4", Some("lua")),
            ("c++", Some("cpp")),
            ("C++", Some("cpp")),
            ("python-mode", Some("python3")),
            ("rust", Some("rust")),
            ("yml", Some("yaml")),
            ("make", Some("makefile")),
            ("fundamental", None),
            ("", None),
            ("not-a-language", None),
        ];
        for (name, expected) in cases {
            assert_eq!(
                normalise_language_name(name, is_known_language).as_deref(),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn language_name_from_modeline_cases() {
        let x20 = "x\n".repeat(20);
        let x6 = "x\n".repeat(6);
        let x10 = "x\n".repeat(10);
        let cases: Vec<(String, Option<&str>)> = vec![
            ("# vim: ft=python\nx = 1\n".into(), Some("python")),
            ("# vim: set filetype=sh :\n".into(), Some("sh")),
            ("/* vim:ft=c ts=4 sw=4 */\n".into(), Some("c")),
            (format!("{x20}# vi: set ts=4 syntax=ruby:\n"), Some("ruby")),
            ("<!-- vim: set noet ft=xml: -->\n".into(), Some("xml")),
            (format!("{x6}# vim: ft=python\n{x20}"), None),
            ("# environment: ft=python\n".into(), None),
            ("# vim: noft=python\n".into(), None),
            (
                "# -*- mode: python; coding: utf-8 -*-\n".into(),
                Some("python"),
            ),
            ("#!/bin/sh\n# -*- mode: perl -*-\n".into(), Some("perl")),
            ("/* -*- C++ -*- */\n".into(), Some("C++")),
            ("# -*- coding: utf-8 -*-\n".into(), None),
            (
                format!("{x10}# Local Variables:\n# mode: lua\n# End:\n"),
                Some("lua"),
            ),
            ("plain text\n".into(), None),
            ("".into(), None),
        ];
        for (text, expected) in cases {
            assert_eq!(
                language_name_from_modeline(&text).as_deref(),
                expected,
                "{text:?}"
            );
        }
    }

    #[test]
    fn language_id_from_text_cases() {
        let cases = [
            ("#!/usr/bin/env python3\nprint('hi')\n", Some("python3")),
            ("#!/usr/bin/env python\nprint('hi')\n", Some("python3")),
            ("#!/usr/bin/python2.7\nprint 'hi'\n", Some("python")),
            ("#!/bin/bash\necho hi\n", Some("sh")),
            ("#!/bin/zsh\necho hi\n", Some("sh")),
            ("#!/usr/bin/env -S node --experimental\n", Some("js")),
            ("#!/usr/bin/perl -w\n", Some("perl")),
            ("#!/bin/sh\n# vim: ft=python\n", Some("python3")),
            ("#!/bin/sh\n# vim: ft=nonsense\n", Some("sh")),
            ("#!/usr/bin/unknown-interpreter\n", None),
            ("Just some text\n", None),
            ("", None),
        ];
        for (text, expected) in cases {
            assert_eq!(
                language_id_from_text(text, is_known_language).as_deref(),
                expected,
                "{text:?}"
            );
        }
    }

    #[test]
    fn guess_language_from_text_cases() {
        let cases = [
            ("#!/usr/bin/env python3\nprint('hi')\n", Some("python3")),
            ("#!/bin/bash\necho hi\n", Some("sh")),
            ("# vim: ft=rust\nfn main() {}\n", Some("rust")),
            (
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<root/>\n",
                Some("xml"),
            ),
            ("Just some text\nwith no language\n", None),
            ("", None),
        ];
        for (text, expected) in cases {
            assert_eq!(
                guess_language_from_text(text).map(|l| l.id),
                expected,
                "{text:?}"
            );
        }
    }

    #[test]
    fn choose_pane_languages_cases() {
        type Case = (
            Vec<Option<&'static str>>,
            Vec<Option<&'static str>>,
            Vec<Option<&'static str>>,
        );
        let cases: Vec<Case> = vec![
            (
                vec![Some("c"), Some("c")],
                vec![None, None],
                vec![Some("c"), Some("c")],
            ),
            (
                vec![Some("python3"), None],
                vec![None, Some("sh")],
                vec![Some("python3"); 2],
            ),
            (
                vec![None, Some("python3"), None],
                vec![None, None, None],
                vec![Some("python3"); 3],
            ),
            (
                vec![Some("c"), Some("chdr")],
                vec![None, None],
                vec![Some("c"), Some("chdr")],
            ),
            (
                vec![Some("c"), Some("chdr"), None],
                vec![None, None, Some("cpp")],
                vec![Some("c"), Some("chdr"), Some("cpp")],
            ),
            (
                vec![Some("c"), Some("chdr"), None],
                vec![None, None, None],
                vec![Some("c"), Some("chdr"), None],
            ),
            (
                vec![None, None],
                vec![Some("sh"), None],
                vec![Some("sh"), Some("sh")],
            ),
            (
                vec![None, None],
                vec![Some("sh"), Some("sh")],
                vec![Some("sh"), Some("sh")],
            ),
            (
                vec![None, None],
                vec![Some("sh"), Some("perl")],
                vec![Some("sh"), Some("perl")],
            ),
            (vec![None, None], vec![None, None], vec![None, None]),
            (vec![None], vec![Some("ruby")], vec![Some("ruby")]),
            (vec![], vec![], vec![]),
        ];
        for (file_languages, text_languages, expected) in cases {
            let mut calls = Vec::new();
            let result = choose_pane_languages(&file_languages, |pane| {
                calls.push(pane);
                text_languages[pane]
            });
            assert_eq!(result, expected);
            for pane in calls {
                assert!(file_languages[pane].is_none());
            }
        }
    }

    #[test]
    fn choose_pane_languages_is_lazy() {
        let result = choose_pane_languages(&[Some("js"), None], |_| -> Option<&str> {
            panic!("Text guessing shouldn't be needed")
        });
        assert_eq!(result, vec![Some("js"), Some("js")]);
    }

    #[test]
    fn language_from_path_cases() {
        // Missing files give no language
        assert!(language_from_path(Path::new("/nonexistent/meld-test/example.py")).is_none());
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join("example.py");
        std::fs::write(&py, "x = 1\n").unwrap();
        assert_eq!(language_from_path(&py).map(|l| l.id), Some("python3"));
        let xml = dir.path().join("noext");
        std::fs::write(&xml, "<?xml version=\"1.0\"?><a/>").unwrap();
        assert_eq!(language_from_path(&xml).map(|l| l.id), Some("xml"));
        assert_eq!(
            guess_language_from_text("#!/bin/bash\n").map(|l| l.id),
            Some("sh")
        );
    }

    #[test]
    fn language_sample_text() {
        let long = format!(
            "# vim: ft=python\n{}# vim: ft=ruby\n",
            "x = 1\n".repeat(1000)
        );
        let long_no_newline = format!("# vim: ft=python\n{}# vim: ft=ruby", "x = 1\n".repeat(1000));
        let medium = "line\n".repeat(50);
        let texts = [
            "",
            "#!/bin/sh\necho\n",
            &long,
            &long_no_newline,
            &medium,
            "no newline at end",
        ];
        for text in texts {
            let sample = get_language_sample_text(text);
            let lines = py_splitlines(text);
            let limit = LANGUAGE_SAMPLE_HEAD_LINES + LANGUAGE_SAMPLE_TAIL_LINES;
            if lines.len() <= limit {
                assert_eq!(sample, text);
            } else {
                let sample_lines = py_splitlines(&sample);
                assert_eq!(sample_lines[..5], lines[..5]);
                assert_eq!(
                    sample_lines[sample_lines.len() - 5..],
                    lines[lines.len() - 5..]
                );
                assert!(sample_lines.len() <= limit);
                assert_eq!(
                    language_name_from_modeline(&sample).as_deref(),
                    Some("python")
                );
                let rest = sample.split_once('\n').unwrap().1;
                assert_eq!(language_name_from_modeline(rest).as_deref(), Some("ruby"));
            }
        }
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(
            py_splitlines("a\nb\r\nc\rd\x0ce"),
            vec!["a", "b", "c", "d", "e"]
        );
        assert_eq!(py_splitlines("a\n"), vec!["a"]);
        assert!(py_splitlines("").is_empty());
        assert_eq!(py_splitlines("\n\n"), vec!["", ""]);
    }
}
