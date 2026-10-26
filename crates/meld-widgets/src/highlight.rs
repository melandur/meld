//! Incremental syntax highlighting with syntect, producing spans tagged
//! with GtkSourceView style names (`def:keyword`, `def:string`, ...).
//!
//! The [`Highlighter`] keeps per-line spans plus parse-state checkpoints.
//! After an [`Edit`](crate::buffer::Edit) it re-parses from the edited line
//! until the parser state converges with the previous parse, so typing
//! only re-highlights a handful of lines. Work on the UI thread is lazy and
//! time-budgeted: the text pane asks for the visible lines while drawing
//! and keeps requesting frames until they are done.
//!
//! [`Highlighter::highlight_visible`] also hands the rest of a large buffer
//! to a background job, which parses it in parallel chunks (speculatively
//! from a fresh state, then fixed up from the true state until the two
//! converge) and streams exact spans and checkpoints back, so jumping
//! anywhere in a huge file soon shows exact colours from a nearby
//! checkpoint. Edits cancel a running job; the next draw restarts it from
//! the nearest exact checkpoint.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

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
/// While a background job is running, the visible range is parsed on the
/// UI thread only if it starts at most this many lines past the exact part.
const SYNC_REACH: usize = 256;

/// Background job sizes.
#[derive(Clone, Copy)]
struct Tuning {
    /// Leave at least this many unhighlighted lines before handing the
    /// rest of the buffer to a background job; smaller gaps are parsed on
    /// the UI thread as they scroll into view.
    background_min: usize,
    /// Minimum lines per speculative chunk.
    min_chunk: usize,
    /// Lines per exact piece streamed back.
    piece: usize,
    /// Fixed number of chunks to parse in parallel (default: per CPU).
    threads: Option<usize>,
}

const TUNING: Tuning = Tuning {
    background_min: 1024,
    min_chunk: 4096,
    piece: 1024,
    threads: None,
};

/// Small sizes so unit tests exercise chunking and fix-up on small inputs.
#[cfg(test)]
const TEST_TUNING: Tuning = Tuning {
    background_min: 64,
    min_chunk: 64,
    piece: 32,
    threads: Some(4),
};

/// Set by the benchmark to measure the real sizes.
#[cfg(test)]
static REAL_TUNING: AtomicBool = AtomicBool::new(false);

fn tuning() -> Tuning {
    #[cfg(test)]
    if !REAL_TUNING.load(Ordering::Relaxed) {
        return TEST_TUNING;
    }
    TUNING
}

/// A background job started right after an edit waits this long first, so
/// typing does not restart a full-buffer parse on every key.
const EDIT_DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Clone, PartialEq, Eq)]
struct State {
    parse: ParseState,
    scopes: ScopeStack,
}

// SAFETY: with the Oniguruma backend a `ParseState` holds `onig::Region`s
// (captures for backreferences), which wrap a raw `OnigRegion`. A region
// uniquely owns its malloc'd position arrays (deep-copied by `Clone`, freed
// by `Drop`) and has no thread affinity, so moving one to another thread is
// sound. States are only ever moved between threads, never shared.
unsafe impl Send for State {}

impl State {
    fn new(syntax: &SyntaxReference) -> Self {
        Self {
            parse: ParseState::new(syntax),
            scopes: ScopeStack::new(),
        }
    }
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
    /// Background parse of the rest of the buffer, if one is running.
    job: Option<Job>,
    /// When the buffer was last edited.
    last_edit: Option<Instant>,
    /// Set when a background thread could not be started.
    no_jobs: bool,
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
    #[cfg(test)]
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
                job: None,
                last_edit: None,
                no_jobs: false,
            }),
        }
    }

    /// Name of the syntect syntax in use, if any.
    pub fn syntax_name(&self) -> Option<&'static str> {
        self.inner.borrow().syntax.map(|s| s.name.as_str())
    }

    /// Whether this highlighter produces no spans.
    #[cfg(test)]
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
        inner.job = None;
        inner.last_edit = None;
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
        // A running job parses a snapshot that no longer matches.
        inner.job = None;
        inner.last_edit = Some(Instant::now());
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
    #[cfg(test)]
    pub fn highlight(&self, lines: &[String], upto: usize, budget: Option<Duration>) -> bool {
        let mut inner = self.inner.borrow_mut();
        inner.poll_job(lines.len());
        inner.highlight(lines, upto, budget)
    }

    /// Highlights for display, never blocking for longer than `budget`.
    ///
    /// The rest of a large buffer is parsed by a background job that
    /// streams exact spans and parse-state checkpoints back; they are
    /// picked up here. Near the exact part, the visible lines are parsed
    /// on this thread from the nearest exact checkpoint. If the visible
    /// range is still far beyond it (e.g. right after jumping to the end
    /// of a huge file), its lines are additionally parsed from a fresh
    /// state nearby so they show approximate colours until the exact
    /// spans arrive. Returns whether the visible lines are exact.
    pub fn highlight_visible(
        &self,
        lines: &[String],
        visible: std::ops::Range<usize>,
        budget: Option<Duration>,
    ) -> bool {
        let mut inner = self.inner.borrow_mut();
        let Some(syntax) = inner.syntax else {
            return true;
        };
        inner.poll_job(lines.len());
        let end = visible.end.min(lines.len());
        // With a job running, leave far-away lines to it rather than
        // duplicating its work here.
        let far = inner.job.is_some() && visible.start > inner.valid + SYNC_REACH;
        let exact = !far && inner.highlight(lines, end, budget);
        inner.maybe_spawn_job(lines);
        if exact {
            return true;
        }
        if !far && visible.start < inner.valid + APPROX_GAP {
            return false;
        }
        let end = (visible.end + APPROX_CONTEXT).min(lines.len());
        let covered = inner.approx.as_ref().is_some_and(|(s, v)| {
            *s <= visible.start && s + v.len() >= visible.end.min(lines.len())
        });
        if !covered {
            let start = visible.start.saturating_sub(APPROX_CONTEXT).min(end);
            let mut state = State::new(syntax);
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
    #[cfg(test)]
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
    fn highlight(&mut self, lines: &[String], upto: usize, budget: Option<Duration>) -> bool {
        let upto = upto.min(lines.len());
        let Some(syntax) = self.syntax else {
            return true;
        };
        if self.valid >= upto {
            return true;
        }
        let deadline = budget.map(|b| Instant::now() + b);
        let set = syntax_set();

        // Resume from the last checkpoint at or before `valid`; every
        // checkpoint after it is from an earlier parse ("stale").
        let (mut line, mut state) = self.resume_point(syntax);
        let resume = line;
        let mut last_checkpoint = line;
        // Parse a little past what was asked for so that we reach a stale
        // checkpoint and can detect convergence.
        let stop = upto.max(self.reparse_until) + 2 * CHECKPOINT_INTERVAL;

        while line < lines.len() {
            if line > resume {
                if line >= self.reparse_until
                    && line < self.spans.len()
                    && self.stale_checkpoint(line).is_some_and(|s| *s == state)
                {
                    self.valid = self.spans.len();
                    self.reparse_until = 0;
                    break;
                }
                if line - last_checkpoint >= CHECKPOINT_INTERVAL {
                    self.store_checkpoint(line, state.clone());
                    last_checkpoint = line;
                } else {
                    self.drop_checkpoint(line);
                }
            }
            if line >= stop || (line > resume && deadline.is_some_and(|d| Instant::now() >= d)) {
                // Remember where we stopped so the next call resumes here.
                self.store_checkpoint(line, state.clone());
                break;
            }
            let spans = parse_line(&mut self.line_buf, &lines[line], &mut state, set);
            if line < self.spans.len() {
                self.spans[line] = spans;
            } else {
                self.spans.push(spans);
            }
            line += 1;
            self.valid = self.valid.max(line);
        }
        if line >= lines.len() {
            self.finish(lines.len());
        }
        self.valid >= upto
    }

    /// The last exact checkpoint at or before `valid`.
    fn resume_point(&self, syntax: &SyntaxReference) -> (usize, State) {
        let valid = self.valid;
        let idx = self.checkpoints.partition_point(|(l, _)| *l <= valid);
        match idx.checked_sub(1) {
            Some(i) => self.checkpoints[i].clone(),
            None => (0, State::new(syntax)),
        }
    }

    /// Marks all `len` lines as exact.
    fn finish(&mut self, len: usize) {
        self.valid = len;
        self.spans.truncate(len);
        self.checkpoints.retain(|(l, _)| *l < len);
        self.reparse_until = 0;
        self.job = None;
    }

    /// Takes in the pieces a background job has finished.
    fn poll_job(&mut self, len: usize) {
        let Some(job) = self.job.take() else {
            return;
        };
        if job.total != len {
            // The buffer was replaced without telling us.
            return;
        }
        let alive = loop {
            match job.rx.try_recv() {
                Ok(piece) => self.apply_piece(piece),
                Err(TryRecvError::Empty) => break true,
                Err(TryRecvError::Disconnected) => break false,
            }
        };
        if self.valid >= len {
            self.finish(len);
        } else if alive {
            self.job = Some(job);
        } else {
            // Only a failure (e.g. a panic) ends a job early; don't retry.
            log::warn!("background highlighting stopped early");
            self.no_jobs = true;
        }
    }

    fn apply_piece(&mut self, piece: Piece) {
        let end = piece.start + piece.spans.len();
        if end <= self.valid || piece.start > self.valid {
            return;
        }
        let from = self.valid;
        for (l, s) in (piece.start..).zip(piece.spans).skip(from - piece.start) {
            if l < self.spans.len() {
                self.spans[l] = s;
            } else {
                self.spans.push(s);
            }
        }
        let lo = self.checkpoints.partition_point(|(l, _)| *l <= from);
        let hi = self.checkpoints.partition_point(|(l, _)| *l <= end);
        self.checkpoints.splice(
            lo..hi,
            piece.checkpoints.into_iter().filter(|(l, _)| *l > from),
        );
        self.valid = end;
    }

    /// Hands the rest of the buffer to a background job if it is big.
    fn maybe_spawn_job(&mut self, lines: &[String]) {
        let Some(syntax) = self.syntax else {
            return;
        };
        if self.job.is_some() || self.no_jobs || self.valid + tuning().background_min > lines.len()
        {
            return;
        }
        let (from, state) = self.resume_point(syntax);
        let rest = &lines[from..];
        let mut text = String::with_capacity(rest.iter().map(|l| l.len() + 1).sum());
        let mut ends = Vec::with_capacity(rest.len());
        for l in rest {
            text.push_str(l);
            text.push('\n');
            ends.push(text.len());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let not_before = self.last_edit.map(|t| t + EDIT_DEBOUNCE);
        let src = JobText {
            syntax,
            from,
            text,
            ends,
            cancel: cancel.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("meld-highlight".into())
            .spawn(move || run_job(src, state, tx, not_before));
        match spawned {
            Ok(_) => {
                self.job = Some(Job {
                    total: lines.len(),
                    rx,
                    cancel,
                });
            }
            Err(err) => {
                log::warn!("cannot start background highlighting: {err}");
                self.no_jobs = true;
            }
        }
    }

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

/// A running background parse of lines `from..total` of a snapshot.
/// Dropping it cancels the job.
struct Job {
    total: usize,
    rx: Receiver<Piece>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Exact spans for lines `start..start + spans.len()`, plus the parse
/// states at the start of some lines in `start + 1..=start + spans.len()`
/// (always including the last).
struct Piece {
    start: usize,
    spans: Vec<Box<[StyleSpan]>>,
    checkpoints: Vec<(usize, State)>,
}

/// The snapshot a background job parses: lines `from..` of the buffer,
/// each followed by `'\n'`, in one string.
struct JobText {
    syntax: &'static SyntaxReference,
    from: usize,
    text: String,
    /// Byte offset just past each line's `'\n'`.
    ends: Vec<usize>,
    cancel: Arc<AtomicBool>,
}

impl JobText {
    fn line(&self, i: usize) -> &str {
        let start = i.checked_sub(1).map_or(0, |p| self.ends[p]);
        &self.text[start..self.ends[i]]
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Parses snapshot lines `range` from `state`, returning their spans
    /// and checkpoints (absolute line numbers) at multiples of
    /// [`CHECKPOINT_INTERVAL`] inside the range. `None` if cancelled.
    fn parse(&self, range: Range<usize>, state: &mut State) -> Option<Run> {
        let set = syntax_set();
        let mut run = Run {
            spans: Vec::with_capacity(range.len()),
            checkpoints: Vec::new(),
        };
        for i in range.clone() {
            if i.is_multiple_of(64) && self.cancelled() {
                return None;
            }
            run.spans.push(parse_text(self.line(i), state, set));
            if (i + 1).is_multiple_of(CHECKPOINT_INTERVAL) && i + 1 < range.end {
                run.checkpoints.push((self.from + i + 1, state.clone()));
            }
        }
        Some(run)
    }
}

struct Run {
    spans: Vec<Box<[StyleSpan]>>,
    checkpoints: Vec<(usize, State)>,
}

fn worker_threads() -> usize {
    tuning().threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(1))
            .clamp(1, 8)
    })
}

/// Background job body. The snapshot is split into chunks; all but the
/// first are parsed in parallel from a fresh state (speculatively). The
/// first is parsed exactly from `state` and streamed back in pieces; each
/// following chunk is then re-parsed from the true state at its start
/// until that state converges with the speculative parse, which for
/// ordinary code takes a few lines, and the rest of the speculative
/// results are exact.
fn run_job(src: JobText, mut state: State, tx: Sender<Piece>, not_before: Option<Instant>) {
    if let Some(t) = not_before {
        while Instant::now() < t {
            if src.cancelled() {
                return;
            }
            std::thread::sleep(
                Duration::from_millis(10).min(t.saturating_duration_since(Instant::now())),
            );
        }
    }
    let Tuning {
        min_chunk, piece, ..
    } = tuning();
    let n = src.ends.len();
    let k = n.div_ceil(min_chunk).clamp(1, worker_threads());
    let chunk = n.div_ceil(k).next_multiple_of(CHECKPOINT_INTERVAL);
    let chunks: Vec<Range<usize>> = (0..n)
        .step_by(chunk.max(1))
        .map(|a| a..(a + chunk).min(n))
        .collect();
    let src = &src;
    let send = |piece: Piece| {
        let ok = tx.send(piece).is_ok();
        if !ok {
            src.cancel.store(true, Ordering::Relaxed);
        }
        ok
    };
    std::thread::scope(|s| {
        let speculative: Vec<_> = chunks
            .iter()
            .skip(1)
            .map(|r| {
                let r = r.clone();
                s.spawn(move || {
                    let mut state = State::new(src.syntax);
                    src.parse(r, &mut state).map(|run| (run, state))
                })
            })
            .collect();

        // The first chunk is exact; stream it so the top shows up early.
        let Some(first) = chunks.first() else {
            return;
        };
        for a in first.clone().step_by(piece) {
            let b = (a + piece).min(first.end);
            let Some(mut run) = src.parse(a..b, &mut state) else {
                return;
            };
            run.checkpoints.push((src.from + b, state.clone()));
            if !send(make_piece(src, a, run)) {
                return;
            }
        }

        for (r, handle) in chunks.iter().skip(1).zip(speculative) {
            let Ok(Some((spec, spec_end))) = handle.join() else {
                src.cancel.store(true, Ordering::Relaxed);
                return;
            };
            if !fix_up(src, r.clone(), &mut state, spec, spec_end, &send) {
                return;
            }
        }
    });
}

fn make_piece(src: &JobText, start: usize, run: Run) -> Piece {
    Piece {
        start: src.from + start,
        spans: run.spans,
        checkpoints: run.checkpoints,
    }
}

/// Re-parses chunk `r` from its true start `state` until it converges
/// with the speculative parse `spec`, streaming the exact result.
fn fix_up(
    src: &JobText,
    r: Range<usize>,
    state: &mut State,
    mut spec: Run,
    spec_end: State,
    send: &impl Fn(Piece) -> bool,
) -> bool {
    let set = syntax_set();
    let mut out = Run {
        spans: Vec::new(),
        checkpoints: Vec::new(),
    };
    let mut start = r.start;
    let mut spec_cps = std::mem::take(&mut spec.checkpoints).into_iter().peekable();
    let mut line = r.start;
    let mut converged = false;
    while line < r.end {
        if line > r.start {
            let abs = src.from + line;
            while spec_cps.next_if(|(l, _)| *l < abs).is_some() {}
            if spec_cps
                .peek()
                .is_some_and(|(l, s)| *l == abs && s == state)
            {
                converged = true;
                break;
            }
        }
        if line.is_multiple_of(64) && src.cancelled() {
            return false;
        }
        out.spans.push(parse_text(src.line(line), state, set));
        line += 1;
        if line.is_multiple_of(CHECKPOINT_INTERVAL) && line < r.end {
            out.checkpoints.push((src.from + line, state.clone()));
        }
        if out.spans.len() >= tuning().piece && line < r.end {
            out.checkpoints.push((src.from + line, state.clone()));
            out.checkpoints.dedup_by(|a, b| a.0 == b.0);
            let done = std::mem::replace(
                &mut out,
                Run {
                    spans: Vec::new(),
                    checkpoints: Vec::new(),
                },
            );
            if !send(make_piece(src, start, done)) {
                return false;
            }
            start = line;
        }
    }
    if converged {
        out.spans.extend(spec.spans.drain(line - r.start..));
        out.checkpoints.extend(spec_cps);
        *state = spec_end;
    }
    out.checkpoints.push((src.from + r.end, state.clone()));
    out.checkpoints.dedup_by(|a, b| a.0 == b.0);
    send(make_piece(src, start, out))
}

thread_local! {
    /// Style of each scope seen so far on this thread.
    static STYLE_CACHE: RefCell<HashMap<Scope, Option<SyntaxStyle>>> =
        RefCell::new(HashMap::new());
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
    parse_text(buf, state, set)
}

/// Parses one line; `text` is the line followed by `'\n'`.
fn parse_text(text: &str, state: &mut State, set: &SyntaxSet) -> Box<[StyleSpan]> {
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.len() > MAX_LINE_BYTES {
        return Box::default();
    }
    let ops = match state.parse.parse_line(text, set) {
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

    STYLE_CACHE.with_borrow_mut(|cache| {
        let mut current = current_style(&state.scopes, cache);
        let mut pos = 0;
        for (at, op) in ops {
            if at > pos {
                push(pos, at, current);
                pos = at;
            }
            if state.scopes.apply(&op).is_err() {
                log::debug!("scope stack error");
            }
            current = current_style(&state.scopes, cache);
        }
        push(pos, line.len(), current);
    });
    spans.into_boxed_slice()
}

fn current_style(
    scopes: &ScopeStack,
    cache: &mut HashMap<Scope, Option<SyntaxStyle>>,
) -> Option<SyntaxStyle> {
    scopes.as_slice().iter().rev().find_map(|scope| {
        *cache
            .entry(*scope)
            .or_insert_with(|| style_for_scope(&scope.build_string()))
    })
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

    /// Draws `vis` like a pane would until the background job has made
    /// the whole buffer exact.
    fn finish_in_background(h: &Highlighter, l: &[String], vis: Range<usize>) {
        let t = Instant::now();
        while !h.is_valid_through(l.len()) {
            h.highlight_visible(l, vis.clone(), Some(Duration::from_micros(50)));
            assert!(
                t.elapsed() < Duration::from_secs(60),
                "background job stalled"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn spans_of(h: &Highlighter, len: usize) -> Vec<Vec<StyleSpan>> {
        (0..len).map(|i| h.line_spans(i).unwrap()).collect()
    }

    fn job_running(h: &Highlighter) -> bool {
        h.inner.borrow().job.is_some()
    }

    fn big8() -> Vec<String> {
        (0..8).flat_map(|_| big_python()).collect()
    }

    #[test]
    fn background_matches_sync() {
        let l = big8();
        let h = Highlighter::for_language("python3");
        assert!(h.highlight_visible(&l, 0..40, None));
        assert!(job_running(&h));
        finish_in_background(&h, &l, 0..40);
        assert!(!job_running(&h));
        assert_eq!(spans_of(&h, l.len()), fresh("python3", &l));
    }

    #[test]
    fn background_far_viewport() {
        let l = big8();
        let h = Highlighter::for_language("python3");
        let vis = l.len() - 40..l.len();
        assert!(!h.highlight_visible(&l, vis.clone(), Some(Duration::from_micros(1))));
        assert!(job_running(&h));
        finish_in_background(&h, &l, vis.clone());
        assert!(h.highlight_visible(&l, vis, Some(Duration::from_micros(1))));
        assert_eq!(spans_of(&h, l.len()), fresh("python3", &l));
    }

    #[test]
    fn background_strings_across_chunks() {
        // A docstring spanning several chunks: their speculative parses
        // are wrong and only converge after it closes.
        let mut l = big8();
        l.insert(100, "\"\"\"".to_owned());
        l.insert(3000, "\"\"\"".to_owned());
        let h = Highlighter::for_language("python3");
        h.highlight_visible(&l, 0..40, None);
        finish_in_background(&h, &l, 0..40);
        assert_eq!(spans_of(&h, l.len()), fresh("python3", &l));
        // Never converges: everything after line 10 is a string.
        let mut l = big8();
        l.insert(10, "\"\"\"".to_owned());
        let h = Highlighter::for_language("python3");
        h.highlight_visible(&l, 0..40, None);
        finish_in_background(&h, &l, 0..40);
        assert_eq!(spans_of(&h, l.len()), fresh("python3", &l));
    }

    #[test]
    fn background_states_with_captures() {
        // Rust raw strings and bash heredocs keep regex captures in the
        // parse state, which then crosses threads.
        let mut l = Vec::new();
        for i in 0..600 {
            l.push(format!("fn f{i}() -> &'static str {{ r##\"start"));
            l.push("  \"# still raw".to_owned());
            l.push(format!("end\"## }} // {i}"));
        }
        let h = Highlighter::for_language("rust");
        h.highlight_visible(&l, 0..40, None);
        finish_in_background(&h, &l, 0..40);
        assert_eq!(spans_of(&h, l.len()), fresh("rust", &l));
        let mut l = Vec::new();
        for i in 0..600 {
            l.push(format!("cat <<EOF{i}"));
            l.push("if then fi".to_owned());
            l.push(format!("EOF{i}"));
        }
        let h = Highlighter::for_language("sh");
        h.highlight_visible(&l, 0..40, None);
        finish_in_background(&h, &l, 0..40);
        assert_eq!(spans_of(&h, l.len()), fresh("sh", &l));
    }

    #[test]
    fn edits_during_and_after_background() {
        let mut l = big8();
        let mut h = Highlighter::for_language("python3");
        h.highlight_visible(&l, 0..40, None);
        assert!(job_running(&h));
        l.insert(500, "\"\"\"".to_owned());
        h.lines_changed(500, 0, 1);
        assert!(!job_running(&h));
        finish_in_background(&h, &l, 480..520);
        assert_eq!(spans_of(&h, l.len()), fresh("python3", &l));
        // Incremental re-highlighting relies on the job's checkpoints.
        l.insert(4000, "\"\"\"".to_owned());
        h.lines_changed(4000, 0, 1);
        assert_eq!(full(&h, &l), fresh("python3", &l));
        l.remove(500);
        h.lines_changed(500, 1, 0);
        assert_eq!(full(&h, &l), fresh("python3", &l));
        l[2000] = "x = 1".to_owned();
        h.lines_changed(2000, 1, 1);
        assert!(h.highlight(&l, 2001, None));
        assert!(h.is_valid_through(l.len()));
        assert_eq!(spans_of(&h, l.len()), fresh("python3", &l));
    }

    #[test]
    fn replaced_buffer_drops_job() {
        let l = big8();
        let h = Highlighter::for_language("python3");
        h.highlight_visible(&l, 0..40, None);
        assert!(job_running(&h));
        // Different length without an edit notification: never apply.
        let short = l[..100].to_vec();
        h.highlight(&short, short.len(), None);
        assert!(!job_running(&h));
        assert!(h.is_valid_through(short.len()));
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

    fn python_lines() -> Vec<String> {
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
        l
    }

    fn rust_lines() -> Vec<String> {
        let src = include_str!("text_pane.rs");
        let mut l = Vec::new();
        while l.len() < 140_000 {
            l.extend(src.lines().map(str::to_owned));
        }
        l
    }

    fn rate(n: usize, d: Duration) -> String {
        format!("{d:.2?} ({:.0} lines/s)", n as f64 / d.as_secs_f64())
    }

    /// Calls `highlight_visible` like a pane drawing frames until `vis`
    /// is exact; returns (time, frames, slowest call).
    fn frames(h: &Highlighter, l: &[String], vis: Range<usize>) -> (Duration, usize, Duration) {
        let t = Instant::now();
        let mut slowest = Duration::ZERO;
        let mut n = 0;
        loop {
            n += 1;
            let c = Instant::now();
            let exact = h.highlight_visible(l, vis.clone(), Some(Duration::from_millis(8)));
            slowest = slowest.max(c.elapsed());
            if exact {
                return (t.elapsed(), n, slowest);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn run(name: &str, id: &str, l: &[String]) {
        println!(
            "--- {name}: {} lines, {} worker threads",
            l.len(),
            worker_threads()
        );
        let set = syntax_set();
        let syntax = find_language(id).unwrap();
        let mut raw = Duration::MAX;
        let mut sync = Duration::MAX;
        for _ in 0..2 {
            let mut ps = ParseState::new(syntax);
            let mut buf = String::new();
            let t = Instant::now();
            for line in l {
                buf.clear();
                buf.push_str(line);
                buf.push('\n');
                let _ = ps.parse_line(&buf, set);
            }
            raw = raw.min(t.elapsed());
            let h = Highlighter::for_language(id);
            let t = Instant::now();
            h.highlight(l, l.len(), None);
            sync = sync.min(t.elapsed());
        }
        println!("raw syntect parse:      {}", rate(l.len(), raw));
        println!("sync full highlight:    {}", rate(l.len(), sync));

        let top = 0..60;
        let end = l.len() - 60..l.len();
        let h = Highlighter::for_language(id);
        let (d, n, slow) = frames(&h, l, top.clone());
        println!("open at top: exact in {d:.2?} ({n} frames, slowest call {slow:.2?})");
        let t = Instant::now();
        let mut slowest = Duration::ZERO;
        while !h.is_valid_through(l.len()) {
            let c = Instant::now();
            h.highlight_visible(l, top.clone(), Some(Duration::from_millis(8)));
            slowest = slowest.max(c.elapsed());
            std::thread::sleep(Duration::from_millis(1));
        }
        println!(
            "background whole file:  {} after opening (slowest call {slowest:.2?})",
            rate(l.len(), t.elapsed() + d)
        );

        let h = Highlighter::for_language(id);
        frames(&h, l, top.clone());
        let (d, n, slow) = frames(&h, l, end.clone());
        println!(
            "jump to end right after opening: exact in {d:.2?} ({n} frames, slowest call {slow:.2?})"
        );

        let mut l = l.to_vec();
        let mut h = Highlighter::for_language(id);
        frames(&h, &l, end.clone());
        while !h.is_valid_through(l.len()) {
            h.highlight_visible(&l, end.clone(), None);
            std::thread::sleep(Duration::from_millis(1));
        }
        // Insert a string/comment opener near the top; in Rust (`/*`) the
        // rest of the file changes and is re-parsed in the background.
        let opener = if id == "rust" { "/*" } else { "\"\"\"" };
        l.insert(10, opener.to_owned());
        h.lines_changed(10, 0, 1);
        let (d, _, slow) = frames(&h, &l, 0..60);
        println!(
            "opener {opener} inserted at top: visible exact in {d:.2?} (slowest call {slow:.2?})"
        );
        let (d, n, slow) = frames(&h, &l, l.len() - 60..l.len());
        println!("  then jump to end: exact in {d:.2?} ({n} frames, slowest call {slow:.2?})");
    }

    /// `cargo test --release -p meld-widgets highlight_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn highlight_speed() {
        REAL_TUNING.store(true, Ordering::Relaxed);
        run("python (synthetic)", "python3", &python_lines());
        run("rust (text_pane.rs repeated)", "rust", &rust_lines());
    }
}
