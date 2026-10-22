//! Translations, without a system gettext library.
//!
//! `build.rs` compiles `po/*.po` (the Python version's catalogs) into
//! compact tables embedded in the binary ([`po`] describes how the Python
//! strings are mapped). At run time the language comes from `LANGUAGE`,
//! `LC_ALL`, `LC_MESSAGES` and `LANG` (`pt_BR.UTF-8` tries `pt_BR`, then
//! `pt`); English, the source language, is the fallback.
//!
//! Messages are marked with macros taking `format!` syntax, with `{}`
//! positional and `name = value` named arguments; translated messages may
//! reorder them. A compile-time `format!` check catches argument
//! mistakes. Named placeholders need explicit `name = value` arguments:
//! implicit captures aren't seen at run time.
//!
//! - `tr!("Save")` → `&'static str`; `tr!("Compare {}", x)` → `String`
//! - `trc!("shortcut window", "New comparison")`: with a `msgctxt`
//! - `trn!("{} file", "{} files", n, n)`: the plural form for `n`
//! - `n_!("Save")`, `nc_!(ctx, "Open")`: mark a message in a table for
//!   extraction; translate it where it is shown with [`gettext`] (or
//!   [`pgettext`])
//!
//! `cargo xtask pot` extracts the marked messages into `po/meld.pot`.

// Shared with build.rs and xtask, which use more of them than the app
#[allow(dead_code)]
pub mod plural;
#[allow(dead_code)]
pub mod po;

use std::collections::HashMap;
use std::fmt::{Display, Write as _};
use std::sync::{Mutex, OnceLock};

include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));

/// The language catalogs compiled into the binary, by code (`de`,
/// `pt_BR`, `sr@latin`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn available() -> impl Iterator<Item = &'static str> {
    CATALOGS.iter().map(|(code, _)| *code)
}

/// One loaded catalog.
pub struct Language {
    pub code: &'static str,
    nplurals: usize,
    plural: plural::Expr,
    messages: HashMap<(&'static str, &'static str), Vec<&'static str>>,
}

impl Language {
    /// The embedded catalog for `code`, if there is one.
    pub fn load(code: &str) -> Option<Language> {
        let (code, bytes) = CATALOGS.iter().find(|(c, _)| *c == code)?;
        let decoded = po::decode(bytes)?;
        let plural = plural::Expr::parse(decoded.plural).ok()?;
        let messages = decoded
            .messages
            .into_iter()
            .map(|(ctx, id, forms)| ((ctx, id), forms))
            .collect();
        Some(Language {
            code,
            nplurals: decoded.nplurals,
            plural,
            messages,
        })
    }

    fn get<'k>(&'k self, context: &'k str, id: &'k str) -> Option<&'k [&'static str]> {
        // `HashMap` is covariant in its key: look up with borrowed strings
        let map: &HashMap<(&str, &str), Vec<&'static str>> = &self.messages;
        map.get(&(context, id)).map(Vec::as_slice)
    }

    fn form(&self, n: u64) -> usize {
        (self.plural.eval(n) as usize).min(self.nplurals.saturating_sub(1))
    }
}

/// The languages to translate into, best first.
pub struct Translator {
    languages: Vec<Language>,
}

impl Translator {
    /// English only.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn english() -> Translator {
        Translator {
            languages: Vec::new(),
        }
    }

    /// The catalogs for `wanted` (see [`wanted_languages`]); stops at
    /// English.
    pub fn for_languages(wanted: &[String]) -> Translator {
        let mut languages: Vec<Language> = Vec::new();
        for code in wanted {
            if code == "C" || code == "en" {
                break;
            }
            if languages.iter().any(|l| l.code == code) {
                continue;
            }
            languages.extend(Language::load(code));
        }
        Translator { languages }
    }

    /// From the environment, like gettext.
    pub fn from_env() -> Translator {
        Translator::for_languages(&wanted_languages(|v| std::env::var(v).ok()))
    }

    /// The code of the language messages are mostly shown in, `None` for
    /// English.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn language(&self) -> Option<&'static str> {
        self.languages.first().map(|l| l.code)
    }

    /// The translation of `id` (in `context`, `""` for none), or `id`.
    pub fn lookup<'a>(&self, context: &str, id: &'a str) -> &'a str {
        self.languages
            .iter()
            .find_map(|l| l.get(context, id))
            .and_then(|f| f.first().copied())
            .unwrap_or(id)
    }

    /// The translation of `one`/`many` for count `n`.
    pub fn lookup_plural<'a>(&self, context: &str, one: &'a str, many: &'a str, n: u64) -> &'a str {
        for l in &self.languages {
            if let Some(forms) = l.get(context, one)
                && let Some(f) = forms.get(l.form(n))
            {
                return f;
            }
        }
        if n == 1 { one } else { many }
    }
}

fn active() -> &'static Translator {
    static ACTIVE: OnceLock<Translator> = OnceLock::new();
    ACTIVE.get_or_init(Translator::from_env)
}

/// The language the app is shown in, `None` for English.
#[cfg_attr(not(test), allow(dead_code))]
pub fn language() -> Option<&'static str> {
    active().language()
}

/// The languages asked for by the environment, best first: each entry of
/// `LANGUAGE` (colon-separated), then the first set one of `LC_ALL`,
/// `LC_MESSAGES` and `LANG`; each expanded from most to least specific
/// (`pt_BR.UTF-8@x` gives `pt_BR@x`, `pt_BR`, `pt@x`, `pt`). The C locale
/// gives `C`.
pub fn wanted_languages(var: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut out = Vec::new();
    let set = |name: &str| var(name).filter(|v| !v.trim().is_empty());
    if let Some(list) = set("LANGUAGE") {
        for item in list.split(':').filter(|s| !s.is_empty()) {
            expand_locale(item, &mut out);
        }
    }
    if let Some(locale) = set("LC_ALL")
        .or_else(|| set("LC_MESSAGES"))
        .or_else(|| set("LANG"))
    {
        expand_locale(&locale, &mut out);
    }
    out
}

fn expand_locale(locale: &str, out: &mut Vec<String>) {
    let locale = locale.trim();
    if locale == "C" || locale == "POSIX" || locale.starts_with("C.") || locale.starts_with("C@") {
        out.push("C".into());
        return;
    }
    let (rest, modifier) = match locale.split_once('@') {
        Some((r, m)) => (r, Some(m)),
        None => (locale, None),
    };
    let rest = rest.split('.').next().unwrap_or(rest);
    let (lang, territory) = match rest.split_once('_') {
        Some((l, t)) => (l, Some(t)),
        None => (rest, None),
    };
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    if let (Some(t), Some(m)) = (territory, modifier) {
        push(format!("{lang}_{t}@{m}"));
    }
    if let Some(t) = territory {
        push(format!("{lang}_{t}"));
    }
    if let Some(m) = modifier {
        push(format!("{lang}@{m}"));
    }
    push(lang.to_string());
}

/// `template` with `{{`/`}}` made single, for messages without
/// arguments; interned, so the result stays borrowed.
fn unescaped(template: &'static str) -> &'static str {
    if !template.contains("{{") && !template.contains("}}") {
        return template;
    }
    static INTERNED: Mutex<Option<HashMap<&'static str, &'static str>>> = Mutex::new(None);
    let mut guard = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    map.entry(template)
        .or_insert_with(|| Box::leak(format(template, &[], &[]).into_boxed_str()))
}

/// The translation of `id` (a message without arguments).
pub fn gettext(id: &'static str) -> &'static str {
    unescaped(active().lookup("", id))
}

/// The translation of `id` in `context`.
pub fn pgettext(context: &str, id: &'static str) -> &'static str {
    unescaped(active().lookup(context, id))
}

/// The translation of `one`/`many` (without arguments) for `n`.
pub fn ngettext(context: &str, one: &'static str, many: &'static str, n: u64) -> &'static str {
    unescaped(active().lookup_plural(context, one, many, n))
}

/// The translation of `id` as a template for [`format`].
pub fn template(context: &str, id: &'static str) -> &'static str {
    active().lookup(context, id)
}

/// The plural translation as a template for [`format`].
pub fn template_plural(
    context: &str,
    one: &'static str,
    many: &'static str,
    n: u64,
) -> &'static str {
    active().lookup_plural(context, one, many, n)
}

/// Fills `template`'s placeholders: `{}` takes the next of `positional`,
/// `{2}` the third, `{name}` the `named` value. Placeholders without a
/// value stay as written.
pub fn format(
    template: &str,
    positional: &[&dyn Display],
    named: &[(&str, &dyn Display)],
) -> String {
    let Some(pieces) = po::pieces(template) else {
        return template.to_string();
    };
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0;
    for piece in pieces {
        match piece {
            po::Piece::Text(t) => out.push_str(t),
            po::Piece::Brace(c) => out.push(c),
            po::Piece::Positional(index) => {
                let i = index.unwrap_or_else(|| {
                    next += 1;
                    next - 1
                });
                match positional.get(i) {
                    Some(v) => {
                        let _ = write!(out, "{v}");
                    }
                    None => match index {
                        Some(i) => {
                            let _ = write!(out, "{{{i}}}");
                        }
                        None => out.push_str("{}"),
                    },
                }
            }
            po::Piece::Named(name) => match named.iter().find(|(n, _)| *n == name) {
                Some((_, v)) => {
                    let _ = write!(out, "{v}");
                }
                None => {
                    let _ = write!(out, "{{{name}}}");
                }
            },
        }
    }
    out
}

/// Translates a message: `tr!("Save")` gives a `&'static str`,
/// `tr!("Compare {} with {other}", a, other = b)` a `String`.
macro_rules! tr {
    ($msg:literal $(,)?) => {
        $crate::i18n::gettext($msg)
    };
    ($msg:literal, $($args:tt)+) => {
        $crate::__tr_format!(($crate::i18n::template("", $msg)) ($msg) [] [] $($args)+)
    };
}

/// Translates a message with a context (gettext's `msgctxt`):
/// `trc!("shortcut window", "Quit Meld")`.
macro_rules! trc {
    ($ctx:literal, $msg:literal $(,)?) => {
        $crate::i18n::pgettext($ctx, $msg)
    };
    ($ctx:literal, $msg:literal, $($args:tt)+) => {
        $crate::__tr_format!(($crate::i18n::template($ctx, $msg)) ($msg) [] [] $($args)+)
    };
}

/// Translates a message with plural forms for count `n`:
/// `trn!("{} file", "{} files", n, n)`. The count isn't an argument by
/// itself; pass it again to show it.
macro_rules! trn {
    ($one:literal, $many:literal, $n:expr $(,)?) => {
        $crate::i18n::ngettext("", $one, $many, ($n) as u64)
    };
    ($one:literal, $many:literal, $n:expr, $($args:tt)+) => {
        $crate::__tr_format!(($crate::i18n::template_plural("", $one, $many, ($n) as u64)) ($many) [] [] $($args)+)
    };
}

/// Marks a message for extraction without translating it, for tables of
/// labels; translate where shown with [`gettext`].
macro_rules! n_ {
    ($msg:literal) => {
        $msg
    };
}

/// [`n_!`] with a context: `nc_!("pull request state", "Open")`; translate
/// with [`pgettext`].
macro_rules! nc_ {
    ($ctx:literal, $msg:literal) => {
        $msg
    };
}

/// Collects the arguments of `tr!`/`trc!`/`trn!`.
#[doc(hidden)]
#[macro_export]
macro_rules! __tr_format {
    (($t:expr) ($lit:literal) [$($p:expr),*] [$($k:ident = $v:expr),*] $name:ident = $val:expr, $($rest:tt)*) => {
        $crate::__tr_format!(($t) ($lit) [$($p),*] [$($k = $v,)* $name = $val] $($rest)*)
    };
    (($t:expr) ($lit:literal) [$($p:expr),*] [$($k:ident = $v:expr),*] $name:ident = $val:expr) => {
        $crate::__tr_format!(($t) ($lit) [$($p),*] [$($k = $v,)* $name = $val])
    };
    (($t:expr) ($lit:literal) [$($p:expr),*] [$($k:ident = $v:expr),*] $val:expr, $($rest:tt)*) => {
        $crate::__tr_format!(($t) ($lit) [$($p,)* $val] [$($k = $v),*] $($rest)*)
    };
    (($t:expr) ($lit:literal) [$($p:expr),*] [$($k:ident = $v:expr),*] $val:expr) => {
        $crate::__tr_format!(($t) ($lit) [$($p,)* $val] [$($k = $v),*])
    };
    (($t:expr) ($lit:literal) [$($p:expr),*] [$($k:ident = $v:expr),*]) => {{
        // Checks the arguments against the English message
        if false {
            let _ = ::std::format!($lit, $($p,)* $($k = $v),*);
        }
        $crate::i18n::format(
            $t,
            &[$(&$p as &dyn ::std::fmt::Display),*],
            &[$((::std::stringify!($k), &$v as &dyn ::std::fmt::Display)),*],
        )
    }};
}

// After the macros, which are scoped textually
mod library;
#[cfg(test)]
mod tests;
