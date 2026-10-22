//! Incremental syntax highlighting with syntect, producing spans tagged
//! with GtkSourceView style names (`def:keyword`, `def:string`, ...).
//!
//! The [`Highlighter`] keeps per-line spans plus parse-state checkpoints.
//! After an [`Edit`](crate::buffer::Edit) it re-parses from the edited line
//! until the parser state converges with the previous parse, so typing
//! only re-highlights a handful of lines. Work is lazy and can be
//! time-budgeted: the text pane asks for the visible lines while drawing
//! and keeps requesting frames until they are done.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

use crate::buffer::Edit;

/// GtkSourceView style names produced by the highlighter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum SyntaxStyle {
    Comment,
    DocComment,
    Shebang,
    Constant,
    SpecialConstant,
    Boolean,
    String,
    Character,
    SpecialChar,
    Number,
    FloatingPoint,
    Identifier,
    Variable,
    Function,
    Builtin,
    Type,
    Statement,
    Keyword,
    Operator,
    Preprocessor,
    Error,
    NetAddress,
    Emphasis,
    StrongEmphasis,
    Heading,
    InlineCode,
    PreformattedSection,
    LinkText,
    LinkDestination,
    ListMarker,
    BlockquoteMarker,
    Insertion,
    Deletion,
}

impl SyntaxStyle {
    /// The GtkSourceView style name.
    pub fn name(self) -> &'static str {
        use SyntaxStyle::*;
        match self {
            Comment => "def:comment",
            DocComment => "def:doc-comment",
            Shebang => "def:shebang",
            Constant => "def:constant",
            SpecialConstant => "def:special-constant",
            Boolean => "def:boolean",
            String => "def:string",
            Character => "def:character",
            SpecialChar => "def:special-char",
            Number => "def:number",
            FloatingPoint => "def:floating-point",
            Identifier => "def:identifier",
            Variable => "def:variable",
            Function => "def:function",
            Builtin => "def:builtin",
            Type => "def:type",
            Statement => "def:statement",
            Keyword => "def:keyword",
            Operator => "def:operator",
            Preprocessor => "def:preprocessor",
            Error => "def:error",
            NetAddress => "def:net-address",
            Emphasis => "def:emphasis",
            StrongEmphasis => "def:strong-emphasis",
            Heading => "def:heading",
            InlineCode => "def:inline-code",
            PreformattedSection => "def:preformatted-section",
            LinkText => "def:link-text",
            LinkDestination => "def:link-destination",
            ListMarker => "def:list-marker",
            BlockquoteMarker => "def:blockquote-marker",
            Insertion => "def:insertion",
            Deletion => "def:deletion",
        }
    }
}

/// GtkSourceView's `def.lang` fallback for a style name, if any
/// (`def:keyword` → `def:statement`, `def:string` → `def:constant`, ...).
pub fn style_fallback(name: &str) -> Option<&'static str> {
    Some(match name {
        "def:shebang" | "def:doc-comment" | "def:doc-comment-element" => "def:comment",
        "def:character"
        | "def:string"
        | "def:special-char"
        | "def:number"
        | "def:boolean"
        | "def:special-constant" => "def:constant",
        "def:floating-point" | "def:decimal" | "def:base-n-integer" => "def:number",
        "def:function" | "def:builtin" | "def:variable" => "def:identifier",
        "def:keyword" | "def:reserved" => "def:statement",
        "def:link-destination" => "def:net-address",
        "def:link-text" | "def:inline-code" | "def:preformatted-section" => "def:constant",
        "def:list-marker" | "def:blockquote-marker" => "def:statement",
        "def:heading" => "def:type",
        "def:insertion" | "def:deletion" => "def:type",
        _ => return None,
    })
}

/// A styled character range within one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StyleSpan {
    /// Start character column.
    pub start: u32,
    /// End character column (exclusive).
    pub end: u32,
    /// Style.
    pub style: SyntaxStyle,
}

/// Maps a TextMate scope to a GtkSourceView style. Longest/most specific
/// prefixes come first.
const SCOPE_MAP: &[(&str, SyntaxStyle)] = &[
    ("comment.line.shebang", SyntaxStyle::Shebang),
    ("comment.block.documentation", SyntaxStyle::DocComment),
    ("comment.line.documentation", SyntaxStyle::DocComment),
    ("comment", SyntaxStyle::Comment),
    ("constant.character.escape", SyntaxStyle::SpecialChar),
    ("constant.other.placeholder", SyntaxStyle::SpecialChar),
    ("string.regexp", SyntaxStyle::String),
    ("string", SyntaxStyle::String),
    ("constant.numeric.float", SyntaxStyle::FloatingPoint),
    ("constant.numeric", SyntaxStyle::Number),
    ("constant.language.boolean", SyntaxStyle::Boolean),
    ("constant.language", SyntaxStyle::SpecialConstant),
    ("constant.character", SyntaxStyle::Character),
    ("constant", SyntaxStyle::Constant),
    ("keyword.operator.word", SyntaxStyle::Keyword),
    ("keyword.operator", SyntaxStyle::Operator),
    ("keyword.control.import", SyntaxStyle::Preprocessor),
    ("keyword.control.directive", SyntaxStyle::Preprocessor),
    ("keyword.control", SyntaxStyle::Statement),
    ("keyword", SyntaxStyle::Keyword),
    ("storage.type", SyntaxStyle::Keyword),
    ("storage.modifier", SyntaxStyle::Keyword),
    ("storage", SyntaxStyle::Keyword),
    ("meta.preprocessor", SyntaxStyle::Preprocessor),
    ("entity.name.function", SyntaxStyle::Function),
    ("support.function", SyntaxStyle::Function),
    ("variable.function", SyntaxStyle::Function),
    ("entity.name.type", SyntaxStyle::Type),
    ("entity.name.class", SyntaxStyle::Type),
    ("entity.name.struct", SyntaxStyle::Type),
    ("entity.name.enum", SyntaxStyle::Type),
    ("entity.name.trait", SyntaxStyle::Type),
    ("entity.name.impl", SyntaxStyle::Type),
    ("entity.name.union", SyntaxStyle::Type),
    ("entity.other.inherited-class", SyntaxStyle::Type),
    ("support.type", SyntaxStyle::Type),
    ("support.class", SyntaxStyle::Type),
    ("entity.name.tag", SyntaxStyle::Keyword),
    ("entity.other.attribute-name", SyntaxStyle::Type),
    ("entity.name.section", SyntaxStyle::Heading),
    ("entity.name", SyntaxStyle::Identifier),
    ("variable.language", SyntaxStyle::SpecialConstant),
    ("variable.parameter", SyntaxStyle::Variable),
    ("variable", SyntaxStyle::Variable),
    ("support.constant", SyntaxStyle::Constant),
    ("support", SyntaxStyle::Builtin),
    ("invalid", SyntaxStyle::Error),
    ("markup.heading", SyntaxStyle::Heading),
    ("markup.bold", SyntaxStyle::StrongEmphasis),
    ("markup.italic", SyntaxStyle::Emphasis),
    ("markup.underline.link", SyntaxStyle::LinkDestination),
    ("markup.raw.inline", SyntaxStyle::InlineCode),
    ("markup.raw", SyntaxStyle::PreformattedSection),
    ("markup.list", SyntaxStyle::ListMarker),
    ("markup.quote", SyntaxStyle::BlockquoteMarker),
    ("markup.inserted", SyntaxStyle::Insertion),
    ("markup.deleted", SyntaxStyle::Deletion),
    ("meta.link.inline.description", SyntaxStyle::LinkText),
];

/// Maps a single scope name to a style, if it has one.
pub fn style_for_scope(scope: &str) -> Option<SyntaxStyle> {
    SCOPE_MAP.iter().find_map(|(prefix, style)| {
        (scope.starts_with(prefix)
            && (scope.len() == prefix.len() || scope.as_bytes()[prefix.len()] == b'.'))
            .then_some(*style)
    })
}

/// The process-wide default syntax set (syntect's bundled Sublime syntaxes).
pub fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Maps a GtkSourceView language id to a syntect syntax name.
fn syntax_name_for_language(id: &str) -> Option<&'static str> {
    Some(match id {
        "python" | "python3" => "Python",
        "rust" => "Rust",
        "c" | "chdr" => "C",
        "cpp" | "cpphdr" => "C++",
        "c-sharp" | "csharp" => "C#",
        "objc" => "Objective-C",
        "sh" | "bash" | "zsh" => "Bourne Again Shell (bash)",
        "js" | "javascript" | "typescript" | "jsx" => "JavaScript",
        "json" => "JSON",
        "markdown" => "Markdown",
        "html" => "HTML",
        "xml" | "xslt" | "mallard" | "docbook" => "XML",
        "css" | "scss" | "less" => "CSS",
        "java" => "Java",
        "go" => "Go",
        "ruby" => "Ruby",
        "perl" => "Perl",
        "php" => "PHP",
        "diff" => "Diff",
        "makefile" => "Makefile",
        "yaml" => "YAML",
        "sql" => "SQL",
        "lua" => "Lua",
        "haskell" => "Haskell",
        "latex" => "LaTeX",
        "scala" => "Scala",
        "erlang" => "Erlang",
        "d" => "D",
        "pascal" => "Pascal",
        "r" => "R",
        "tcl" => "Tcl",
        "commonlisp" | "scheme" => "Lisp",
        "clojure" => "Clojure",
        "ocaml" => "OCaml",
        "matlab" | "octave" => "MATLAB",
        "groovy" => "Groovy",
        "dot" => "Graphviz (DOT)",
        "rst" => "reStructuredText",
        "dosbatch" => "Batch File",
        "asp" => "ASP",
        _ => return None,
    })
}

fn find_language(id: &str) -> Option<&'static SyntaxReference> {
    let set = syntax_set();
    syntax_name_for_language(id)
        .and_then(|name| set.find_syntax_by_name(name))
        .or_else(|| set.find_syntax_by_token(id))
}

fn find_file(name: &str, first_line: Option<&str>) -> Option<&'static SyntaxReference> {
    let set = syntax_set();
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let by_ext = base
        .rsplit_once('.')
        .and_then(|(_, ext)| set.find_syntax_by_extension(ext));
    by_ext
        .or_else(|| set.find_syntax_by_extension(base))
        .or_else(|| match base {
            "Makefile" | "makefile" | "GNUmakefile" => set.find_syntax_by_name("Makefile"),
            _ => None,
        })
        .or_else(|| first_line.and_then(|l| set.find_syntax_by_first_line(l)))
}

/// Lines longer than this (bytes) are not parsed, like Sublime Text does.
const MAX_LINE_BYTES: usize = 20_000;
/// Use approximate highlighting when the visible range starts this many
/// lines past the exactly highlighted part.
const APPROX_GAP: usize = 2000;
/// Lines parsed before (and after) the visible range for approximations.
const APPROX_CONTEXT: usize = 200;
/// Distance between stored parse-state checkpoints.
const CHECKPOINT_INTERVAL: usize = 32;

#[derive(Clone, PartialEq, Eq)]
struct State {
    parse: ParseState,
    scopes: ScopeStack,
}

struct Inner {
    syntax: Option<&'static SyntaxReference>,
    /// Spans for lines `0..spans.len()`; those from `valid` on are stale.
    spans: Vec<Box<[StyleSpan]>>,
    /// Lines `0..valid` are known to be correct.
    valid: usize,
    /// Pending edits force re-parsing at least up to here before a
    /// converged state may be trusted.
    reparse_until: usize,
    /// Sorted parse states at the start of the given lines.
    checkpoints: Vec<(usize, State)>,
    /// Approximate spans for a window far beyond `valid`, parsed from a
    /// fresh state, shown until the exact parse catches up.
    approx: Option<(usize, Vec<Box<[StyleSpan]>>)>,
    line_buf: String,
}

/// Incremental syntax highlighter for one buffer. See the module docs.
///
/// Uses interior mutability so a pane can highlight lazily while drawing;
/// it is therefore not `Sync`.
pub struct Highlighter {
    inner: RefCell<Inner>,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("syntax", &self.syntax_name())
            .finish()
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::plain()
    }
}

impl Highlighter {
    /// A highlighter that produces no spans.
    pub fn plain() -> Self {
        Self::with_syntax(None)
    }

    /// Highlighter for a GtkSourceView language id (`"python3"`, `"rust"`,
    /// `"c"`, `"sh"`, `"js"`, `"markdown"`, ...). Unknown ids fall back to
    /// syntect's token lookup (extension or syntax name), then plain text.
    pub fn for_language(id: &str) -> Self {
        Self::with_syntax(find_language(id))
    }

    /// Highlighter chosen from a file name (extension or well-known name),
    /// falling back to the first line (shebang, modelines).
    pub fn for_file(name: &str, first_line: Option<&str>) -> Self {
        Self::with_syntax(find_file(name, first_line))
    }

    /// Whether a language id maps to a known syntax.
    pub fn has_language(id: &str) -> bool {
        find_language(id).is_some()
    }

    fn with_syntax(syntax: Option<&'static SyntaxReference>) -> Self {
        let syntax = syntax.filter(|s| s.name != "Plain Text");
        Self {
            inner: RefCell::new(Inner {
                syntax,
                spans: Vec::new(),
                valid: 0,
                reparse_until: 0,
                checkpoints: Vec::new(),
                approx: None,
                line_buf: String::new(),
            }),
        }
    }

    /// Name of the syntect syntax in use, if any.
    pub fn syntax_name(&self) -> Option<&'static str> {
        self.inner.borrow().syntax.map(|s| s.name.as_str())
    }

    /// Whether this highlighter produces no spans.
    pub fn is_plain(&self) -> bool {
        self.inner.borrow().syntax.is_none()
    }

    /// Forgets everything (e.g. after replacing the whole text).
    pub fn reset(&mut self) {
        let inner = self.inner.get_mut();
        inner.spans.clear();
        inner.checkpoints.clear();
        inner.approx = None;
        inner.valid = 0;
        inner.reparse_until = 0;
    }

    /// Records an edit. Stale spans stay visible (shifted) until they are
    /// re-highlighted, which avoids flicker.
    pub fn edit(&mut self, edit: &Edit) {
        self.lines_changed(
            edit.start_line,
            edit.removed_lines.len(),
            edit.inserted_lines.len(),
        );
    }

    /// Records that `removed` lines at `start` were replaced by `inserted`
    /// lines.
    pub fn lines_changed(&mut self, start: usize, removed: usize, inserted: usize) {
        let inner = self.inner.get_mut();
        let delta = inserted as isize - removed as isize;
        let shift = |l: usize| (l as isize + delta).max(0) as usize;
        inner.approx = None;
        if start < inner.spans.len() {
            let end = (start + removed).min(inner.spans.len());
            inner.spans.splice(
                start..end,
                std::iter::repeat_with(Default::default).take(inserted),
            );
        }
        // States at the start of lines start+1..=start+removed are gone;
        // the state at `start` itself is unaffected.
        inner
            .checkpoints
            .retain(|(l, _)| *l <= start || *l > start + removed);
        for (l, _) in &mut inner.checkpoints {
            if *l > start + removed {
                *l = shift(*l);
            }
        }
        inner.valid = inner.valid.min(start);
        inner.reparse_until = if inner.reparse_until > start + removed {
            shift(inner.reparse_until)
        } else {
            inner.reparse_until.min(start)
        }
        .max(start + inserted + 1);
    }

    /// Whether lines `0..upto` have up-to-date spans.
    pub fn is_valid_through(&self, upto: usize) -> bool {
        let inner = self.inner.borrow();
        inner.syntax.is_none() || inner.valid >= upto
    }

    /// Highlights at least lines `0..upto` of `lines`, spending at most
    /// `budget` (if given). Returns whether `0..upto` is now valid.
    pub fn highlight(&self, lines: &[String], upto: usize, budget: Option<Duration>) -> bool {
        let mut inner = self.inner.borrow_mut();
        let upto = upto.min(lines.len());
        let Some(syntax) = inner.syntax else {
            return true;
        };
        if inner.valid >= upto {
            return true;
        }
        let deadline = budget.map(|b| Instant::now() + b);
        let set = syntax_set();

        // Resume from the last checkpoint at or before `valid`; every
        // checkpoint after it is from an earlier parse ("stale").
        let valid = inner.valid;
        let idx = inner.checkpoints.partition_point(|(l, _)| *l <= valid);
        let (mut line, mut state) = match idx.checked_sub(1) {
            Some(i) => inner.checkpoints[i].clone(),
            None => (
                0,
                State {
                    parse: ParseState::new(syntax),
                    scopes: ScopeStack::new(),
                },
            ),
        };
        let resume = line;
        let mut last_checkpoint = line;
        // Parse a little past what was asked for so that we reach a stale
        // checkpoint and can detect convergence.
        let stop = upto.max(inner.reparse_until) + 2 * CHECKPOINT_INTERVAL;

        while line < lines.len() {
            if line > resume {
                if line >= inner.reparse_until
                    && line < inner.spans.len()
                    && inner.stale_checkpoint(line).is_some_and(|s| *s == state)
                {
                    inner.valid = inner.spans.len();
                    inner.reparse_until = 0;
                    break;
                }
                if line - last_checkpoint >= CHECKPOINT_INTERVAL {
                    inner.store_checkpoint(line, state.clone());
                    last_checkpoint = line;
                } else {
                    inner.drop_checkpoint(line);
                }
            }
            if line >= stop || (line > resume && deadline.is_some_and(|d| Instant::now() >= d)) {
                // Remember where we stopped so the next call resumes here.
                inner.store_checkpoint(line, state.clone());
                break;
            }
            let spans = parse_line(&mut inner.line_buf, &lines[line], &mut state, set);
            if line < inner.spans.len() {
                inner.spans[line] = spans;
            } else {
                inner.spans.push(spans);
            }
            line += 1;
            inner.valid = inner.valid.max(line);
        }
        if line >= lines.len() {
            inner.valid = lines.len();
            inner.spans.truncate(lines.len());
            inner.checkpoints.retain(|(l, _)| *l < lines.len());
            inner.reparse_until = 0;
        }
        inner.valid >= upto
    }

    /// Highlights for display: exact spans up to `visible.end` within
    /// `budget`; if the exact parse is still far behind the visible range
    /// (e.g. after jumping to the end of a huge file), the visible lines
    /// are additionally parsed from a fresh state nearby so they show
    /// approximate colours immediately. Returns whether the visible lines
    /// are exact.
    pub fn highlight_visible(
        &self,
        lines: &[String],
        visible: std::ops::Range<usize>,
        budget: Option<Duration>,
    ) -> bool {
        if self.highlight(lines, visible.end, budget) {
            return true;
        }
        let mut inner = self.inner.borrow_mut();
        let Some(syntax) = inner.syntax else {
            return true;
        };
        if visible.start < inner.valid + APPROX_GAP {
            return false;
        }
        let end = (visible.end + APPROX_CONTEXT).min(lines.len());
        let covered = inner.approx.as_ref().is_some_and(|(s, v)| {
            *s <= visible.start && s + v.len() >= visible.end.min(lines.len())
        });
        if !covered {
            let start = visible.start.saturating_sub(APPROX_CONTEXT);
            let mut state = State {
                parse: ParseState::new(syntax),
                scopes: ScopeStack::new(),
            };
            let set = syntax_set();
            let mut buf = std::mem::take(&mut inner.line_buf);
            let spans = lines[start..end]
                .iter()
                .map(|l| parse_line(&mut buf, l, &mut state, set))
                .collect();
            inner.line_buf = buf;
            inner.approx = Some((start, spans));
        }
        false
    }

    /// Spans for a line, if it has been highlighted (possibly stale or
    /// approximate).
    pub fn line_spans(&self, line: usize) -> Option<Vec<StyleSpan>> {
        let inner = self.inner.borrow();
        inner.syntax?;
        inner.spans_for(line).map(<[StyleSpan]>::to_vec)
    }

    /// Calls `f` with the spans of a line without copying them.
    pub fn with_line_spans<R>(&self, line: usize, f: impl FnOnce(&[StyleSpan]) -> R) -> R {
        let inner = self.inner.borrow();
        f(inner.spans_for(line).unwrap_or(&[]))
    }
}

impl Inner {
    fn spans_for(&self, line: usize) -> Option<&[StyleSpan]> {
        if line >= self.valid
            && let Some((start, spans)) = &self.approx
            && line >= *start
            && let Some(s) = spans.get(line - start)
        {
            return Some(s);
        }
        self.spans.get(line).map(|s| &s[..])
    }

    /// A checkpoint stored for exactly `line` that lies after the part
    /// re-parsed so far (i.e. from the previous parse).
    fn stale_checkpoint(&self, line: usize) -> Option<&State> {
        let i = self.checkpoints.partition_point(|(l, _)| *l < line);
        let (l, s) = self.checkpoints.get(i)?;
        (*l == line).then_some(s)
    }

    fn store_checkpoint(&mut self, line: usize, state: State) {
        let i = self.checkpoints.partition_point(|(l, _)| *l < line);
        match self.checkpoints.get_mut(i) {
            Some((l, s)) if *l == line => *s = state,
            _ => self.checkpoints.insert(i, (line, state)),
        }
    }

    fn drop_checkpoint(&mut self, line: usize) {
        let i = self.checkpoints.partition_point(|(l, _)| *l < line);
        if self.checkpoints.get(i).is_some_and(|(l, _)| *l == line) {
            self.checkpoints.remove(i);
        }
    }
}

fn parse_line(
    buf: &mut String,
    line: &str,
    state: &mut State,
    set: &SyntaxSet,
) -> Box<[StyleSpan]> {
    if line.len() > MAX_LINE_BYTES {
        return Box::default();
    }
    buf.clear();
    buf.push_str(line);
    buf.push('\n');
    let ops = match state.parse.parse_line(buf, set) {
        Ok(ops) => ops,
        Err(err) => {
            log::debug!("syntax parse error: {err}");
            return Box::default();
        }
    };

    let ascii = line.is_ascii();
    let to_col = |byte: usize| -> u32 {
        let byte = byte.min(line.len());
        if ascii {
            byte as u32
        } else {
            line[..byte].chars().count() as u32
        }
    };

    let mut spans: Vec<StyleSpan> = Vec::new();
    let mut push = |start: usize, end: usize, style: Option<SyntaxStyle>| {
        let (Some(style), true) = (style, end > start) else {
            return;
        };
        let (s, e) = (to_col(start), to_col(end));
        if s >= e {
            return;
        }
        if let Some(last) = spans.last_mut()
            && last.style == style
            && last.end == s
        {
            last.end = e;
            return;
        }
        spans.push(StyleSpan {
            start: s,
            end: e,
            style,
        });
    };

    let mut current = current_style(&state.scopes);
    let mut pos = 0;
    for (at, op) in ops {
        if at > pos {
            push(pos, at, current);
            pos = at;
        }
        if state.scopes.apply(&op).is_err() {
            log::debug!("scope stack error");
        }
        current = current_style(&state.scopes);
    }
    push(pos, line.len(), current);
    spans.into_boxed_slice()
}

fn current_style(scopes: &ScopeStack) -> Option<SyntaxStyle> {
    scopes
        .as_slice()
        .iter()
        .rev()
        .find_map(|scope| style_for_scope(&scope.build_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    fn styles_at(h: &Highlighter, line: usize, src: &str, word: &str) -> Option<SyntaxStyle> {
        let col = src.lines().nth(line).unwrap().find(word).unwrap() as u32;
        h.line_spans(line)
            .unwrap()
            .iter()
            .find(|s| s.start <= col && col < s.end)
            .map(|s| s.style)
    }

    const PY: &str = "import os\n# a comment\ndef foo(x):\n    return \"str\" + 42\n";

    #[test]
    fn language_ids() {
        for id in [
            "python3", "python", "rust", "c", "cpp", "sh", "js", "markdown", "json", "xml", "diff",
        ] {
            assert!(Highlighter::has_language(id), "{id}");
        }
        assert!(!Highlighter::has_language("no-such-language"));
        assert!(Highlighter::for_language("no-such-language").is_plain());
        assert_eq!(
            Highlighter::for_language("python3").syntax_name(),
            Some("Python")
        );
    }

    #[test]
    fn file_names() {
        assert_eq!(
            Highlighter::for_file("src/main.rs", None).syntax_name(),
            Some("Rust")
        );
        assert_eq!(
            Highlighter::for_file("foo.py", None).syntax_name(),
            Some("Python")
        );
        assert_eq!(
            Highlighter::for_file("Makefile", None).syntax_name(),
            Some("Makefile")
        );
        assert_eq!(
            Highlighter::for_file("script", Some("#!/bin/bash")).syntax_name(),
            Some("Bourne Again Shell (bash)")
        );
        assert!(Highlighter::for_file("README", None).is_plain());
    }

    #[test]
    fn python_styles() {
        let h = Highlighter::for_language("python3");
        let l = lines(PY);
        assert!(h.highlight(&l, l.len(), None));
        assert_eq!(
            styles_at(&h, 0, PY, "import"),
            Some(SyntaxStyle::Preprocessor)
        );
        assert_eq!(styles_at(&h, 1, PY, "comment"), Some(SyntaxStyle::Comment));
        assert_eq!(styles_at(&h, 1, PY, "#"), Some(SyntaxStyle::Comment));
        assert_eq!(styles_at(&h, 2, PY, "def"), Some(SyntaxStyle::Keyword));
        assert_eq!(styles_at(&h, 2, PY, "foo"), Some(SyntaxStyle::Function));
        assert_eq!(styles_at(&h, 3, PY, "return"), Some(SyntaxStyle::Statement));
        assert_eq!(styles_at(&h, 3, PY, "str"), Some(SyntaxStyle::String));
        assert_eq!(styles_at(&h, 3, PY, "\""), Some(SyntaxStyle::String));
        assert_eq!(styles_at(&h, 3, PY, "42"), Some(SyntaxStyle::Number));
    }

    #[test]
    fn rust_and_c_styles() {
        let src = "fn main() { let x: u32 = 1; }";
        let h = Highlighter::for_language("rust");
        let l = lines(src);
        h.highlight(&l, 1, None);
        assert_eq!(styles_at(&h, 0, src, "fn"), Some(SyntaxStyle::Keyword));
        assert_eq!(styles_at(&h, 0, src, "main"), Some(SyntaxStyle::Function));
        assert_eq!(styles_at(&h, 0, src, "u32"), Some(SyntaxStyle::Keyword));
        let src = "#include <stdio.h>\nint main(void) { return 0; }";
        let h = Highlighter::for_language("c");
        let l = lines(src);
        h.highlight(&l, 2, None);
        assert_eq!(
            styles_at(&h, 0, src, "include"),
            Some(SyntaxStyle::Preprocessor)
        );
        assert_eq!(styles_at(&h, 1, src, "int"), Some(SyntaxStyle::Keyword));
        assert_eq!(styles_at(&h, 1, src, "0"), Some(SyntaxStyle::Number));
    }

    #[test]
    fn spans_use_char_columns() {
        let src = "s = \"héllo\" # ü";
        let h = Highlighter::for_language("python3");
        let l = lines(src);
        h.highlight(&l, 1, None);
        let spans = h.line_spans(0).unwrap();
        let string = spans
            .iter()
            .find(|s| s.style == SyntaxStyle::String)
            .unwrap();
        assert_eq!((string.start, string.end), (4, 11));
        let comment = spans
            .iter()
            .find(|s| s.style == SyntaxStyle::Comment)
            .unwrap();
        assert_eq!((comment.start, comment.end), (12, 15));
    }

    fn full(h: &Highlighter, l: &[String]) -> Vec<Vec<StyleSpan>> {
        h.highlight(l, l.len(), None);
        (0..l.len()).map(|i| h.line_spans(i).unwrap()).collect()
    }

    fn fresh(id: &str, l: &[String]) -> Vec<Vec<StyleSpan>> {
        full(&Highlighter::for_language(id), l)
    }

    fn big_python() -> Vec<String> {
        let mut l = Vec::new();
        for i in 0..400 {
            l.push(format!("def f{i}(a, b):"));
            l.push(format!("    return a + b * {i}  # c"));
        }
        l
    }

    #[test]
    fn incremental_edit_matches_full_parse() {
        let mut l = big_python();
        let mut h = Highlighter::for_language("python3");
        full(&h, &l);
        // Open a docstring: everything after becomes a string.
        l.insert(10, "\"\"\"".to_owned());
        h.lines_changed(10, 0, 1);
        assert_eq!(full(&h, &l), fresh("python3", &l));
        // Close it again further down.
        l.insert(300, "\"\"\"".to_owned());
        h.lines_changed(300, 0, 1);
        assert_eq!(full(&h, &l), fresh("python3", &l));
        // Remove both, replace a line.
        l.remove(300);
        h.lines_changed(300, 1, 0);
        l.remove(10);
        h.lines_changed(10, 1, 0);
        l[50] = "x = 'unterminated".to_owned();
        h.lines_changed(50, 1, 1);
        assert_eq!(full(&h, &l), fresh("python3", &l));
    }

    #[test]
    fn convergence_stops_early() {
        let mut l = big_python();
        let mut h = Highlighter::for_language("python3");
        full(&h, &l);
        l[100] = "    return 1".to_owned();
        h.lines_changed(100, 1, 1);
        assert!(!h.is_valid_through(101));
        // Only ask for a few lines; convergence should validate all.
        assert!(h.highlight(&l, 101, None));
        assert!(h.is_valid_through(l.len()));
        assert_eq!(full(&h, &l), fresh("python3", &l));
    }

    #[test]
    fn multiple_pending_edits() {
        let mut l = big_python();
        let mut h = Highlighter::for_language("python3");
        full(&h, &l);
        l[5] = "y = 1".to_owned();
        h.lines_changed(5, 1, 1);
        l.insert(600, "s = \"\"\"".to_owned());
        h.lines_changed(600, 0, 1);
        assert_eq!(full(&h, &l), fresh("python3", &l));
    }

    #[test]
    fn budgeted_highlighting_makes_progress() {
        let l = big_python();
        let h = Highlighter::for_language("python3");
        let mut done = false;
        for _ in 0..10_000 {
            if h.highlight(&l, l.len(), Some(Duration::from_micros(200))) {
                done = true;
                break;
            }
        }
        assert!(done);
        assert_eq!(
            (0..l.len())
                .map(|i| h.line_spans(i).unwrap())
                .collect::<Vec<_>>(),
            fresh("python3", &l)
        );
    }

    #[test]
    fn approximate_far_region() {
        let l = big_python();
        let mut big = Vec::new();
        for _ in 0..4 {
            big.extend(l.iter().cloned());
        }
        let h = Highlighter::for_language("python3");
        let vis = big.len() - 40..big.len();
        assert!(!h.highlight_visible(&big, vis.clone(), Some(Duration::from_micros(1))));
        let exact = fresh("python3", &big);
        for i in vis.clone() {
            assert_eq!(h.line_spans(i).unwrap(), exact[i]);
        }
        // Exact highlighting replaces the approximation once done.
        h.highlight(&big, big.len(), None);
        assert!(h.is_valid_through(big.len()));
    }

    #[test]
    fn plain_has_no_spans() {
        let h = Highlighter::plain();
        assert!(h.highlight(&["x".to_owned()], 1, None));
        assert!(h.line_spans(0).is_none());
    }

    #[test]
    fn scope_mapping() {
        assert_eq!(
            style_for_scope("comment.line.number-sign.python"),
            Some(SyntaxStyle::Comment)
        );
        assert_eq!(
            style_for_scope("constant.numeric.integer.decimal.c"),
            Some(SyntaxStyle::Number)
        );
        assert_eq!(
            style_for_scope("keyword.operator.arithmetic"),
            Some(SyntaxStyle::Operator)
        );
        assert_eq!(style_for_scope("keywordx"), None);
        assert_eq!(style_for_scope("source.python"), None);
        assert_eq!(style_fallback("def:keyword"), Some("def:statement"));
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `cargo test --release -p meld-widgets highlight_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn highlight_speed() {
        let mut l = Vec::new();
        for i in 0..20_000 {
            l.push(format!("def function_{i}(value, scale=2):"));
            l.push("    \"\"\"Return a scaled value.\"\"\"".to_owned());
            l.push(format!("    result = value * scale + {i}"));
            l.push("    if result > 100:".to_owned());
            l.push("        return result - 100".to_owned());
            l.push("    return result".to_owned());
            l.push(String::new());
        }
        let h = Highlighter::for_language("python3");
        let t = Instant::now();
        h.highlight(&l, l.len(), None);
        let el = t.elapsed();
        println!(
            "{} lines in {:?} ({:.0} lines/s)",
            l.len(),
            el,
            l.len() as f64 / el.as_secs_f64()
        );
        let mut calls = 0;
        let h = Highlighter::for_language("python3");
        while !h.highlight(&l, l.len(), Some(Duration::from_millis(8))) {
            calls += 1;
        }
        println!("budgeted: {calls} calls");
    }
}
