//! Custom iced 0.14 widgets for Meld's comparison views.
//!
//! Replaces `meld/sourceview.py` + `meld/meldbuffer.py` (GtkSourceView
//! editing and drawing), `meld/gutterrendererchunk.py` (line numbers with
//! chunk backgrounds), `meld/linkmap.py`, `meld/chunkmap.py`,
//! `meld/actiongutter.py`, the scroll syncing of `meld/filediff.py`, and
//! GtkSourceView's syntax highlighting.
//!
//! Public API:
//!
//! - [`buffer`]: [`Buffer`] (lines, cursor, selection, read-only,
//!   indentation), [`Position`], [`Action`], [`Motion`], [`EditAction`],
//!   [`Edit`] (line-level change with `inverse()`), [`ApplyError`].
//!   `Buffer::perform(Action) -> Option<Edit>`, `Buffer::apply(&Edit)`,
//!   `Buffer::replace_lines(range, lines) -> Edit`,
//!   `Buffer::replace_range(start, end, text) -> Edit`.
//! - [`text_pane`]: [`TextPane`] widget (`text_pane(&buffer, &style)` +
//!   builder: `on_action`, `scroll`/`scroll_y`/`scroll_x`, `metrics`,
//!   `font`, `highlights`, `inline_highlights`, `sync_points`, `syntax`,
//!   `show_line_numbers`, `show_whitespace`, `highlight_current_line`,
//!   `overscroll_lines`, `wrap`, `dimmed`, `search_matches`, `flashes`,
//!   `id`), [`Metrics`], `content_height`, `max_scroll_y`, `visible_lines`.
//! - [`wrap`]: [`WrapMode`], [`WrapLayout`] (visual rows per line and the
//!   line ↔ y mapping every widget below accepts via `.wrap(..)`),
//!   [`wrap_line`].
//! - [`link_map`]: [`LinkMap`] canvas between two panes; wheel scrolling
//!   over it emits `on_scroll(down)` (Meld jumps between changes).
//! - [`chunk_map`]: [`ChunkMap`] overview strip, emitting [`MapScroll`],
//!   drawn as chunk bars or a miniature of the text ([`MapStyle`]).
//! - [`action_gutter`]: [`ActionGutter`] chunk buttons emitting
//!   `(chunk index, ChunkAction)`; [`ActionMode`], [`ChunkAction`],
//!   [`classify`](action_gutter::classify).
//! - [`highlight`]: incremental syntect [`Highlighter`]
//!   (`for_language("python3")`, `for_file(name, first_line)`, `edit`,
//!   `highlight`, `highlight_visible`, `line_spans`) producing [`StyleSpan`]s named by
//!   GtkSourceView style ([`SyntaxStyle::name`], `style_fallback`).
//! - [`style`]: [`PaneStyle`] colours (text, gutter, selection, chunks,
//!   inline, maps) with `syntax_style(name) -> Option<TextStyle>` lookup;
//!   presets `light()`, `dark()`, `meld_classic()`; `for_new_side()`
//!   swaps insert/delete colours for the pane on the "new" side.
//! - [`chunk`]: input types [`DiffChunk`], [`ChunkKind`], [`LineHighlight`],
//!   [`InlineHighlight`], [`LineFlash`], [`MapChunk`], and converters
//!   `line_highlights`, `map_chunks`.
//! - [`scroll_sync`]: [`sync_scroll`] / [`calc_syncpoint`] for Meld-style
//!   linked scrolling.
//!
//! Wiring: the application owns one `Buffer`, `Highlighter` and scroll
//! offset per pane (and a `WrapLayout` when wrapping lines, updated on
//! `Action::WrapColumns` and after edits). On `Action::Scrolled { x, y }` from pane *i* it stores
//! the offsets, then computes the other panes' offsets with
//! [`sync_scroll`] and passes them all back via `.scroll_y(..)`. The same
//! offsets and [`Metrics::line_height`] feed the link maps, action gutters
//! and chunk maps so everything stays aligned. Every other action goes to
//! `Buffer::perform`; a returned `Edit` is forwarded to the diff engine,
//! the undo stack and `Highlighter::edit`.

pub mod action_gutter;
pub mod buffer;
pub mod chunk;
pub mod chunk_map;
pub mod highlight;
pub mod link_map;
pub mod scroll_sync;
pub mod style;
pub mod text_pane;
pub mod wrap;

pub use action_gutter::{ActionGutter, ActionMode, ChunkAction, Direction, action_gutter};
pub use buffer::{Action, ApplyError, Buffer, Edit, EditAction, Motion, Position};
pub use chunk::{
    ChunkKind, DiffChunk, InlineHighlight, LineFlash, LineHighlight, MapChunk, line_highlights,
    map_chunks,
};
pub use chunk_map::{ChunkMap, MapScroll, MapStyle, chunk_map};
pub use highlight::{Highlighter, StyleSpan, SyntaxStyle};
pub use link_map::{LinkMap, link_map};
pub use scroll_sync::{ScrollGeometry, calc_syncpoint, sync_scroll};
pub use style::{ChunkColor, ChunkColors, PaneStyle, TextStyle};
pub use text_pane::{Metrics, TextPane, text_pane};
pub use wrap::{WrapLayout, WrapMode, wrap_line};
