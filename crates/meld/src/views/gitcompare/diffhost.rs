//! The two-pane file comparison inside a Git Compare tab.
//!
//! This module is the only place of Git Compare that knows what compares
//! the files. Python embeds a `FileDiff(2)` and attaches the Why column to
//! its right; here the tab talks to [`DiffHost`], whose methods are exactly
//! the embedding API of `crate::views::filediff::FileDiff`:
//!
//! - `DiffHost::embedded(shared)`, `set_files(Vec<EmbeddedFile>, shared)`
//! - `pane_geometry(pane) -> Option<PaneGeometry>` (0-based line `i` is at
//!   `y = i * line_height - scroll_y` from the top of the pane's text)
//! - `scroll_to_line(pane, line)`, `first_change()`, `is_loading()`
//! - the tab methods `update`, `command`, `view`, `toolbar`, `menu_items`,
//!   `unsaved`, `request_close`, `subscription`, `settings_changed`
//!
//! plus two that only this adapter has: [`DiffHost::header_height`] (how
//! far below the top of the view the panes' text starts, so that the Why
//! column lines up) and [`DiffHost::set_hover`] (lines of the hovered card,
//! drawn highlighted; may be a no-op).
//!
//! # TEMPORARY STAND-IN
//!
//! Until `views/filediff/` lands, `DiffHost` is a minimal read-only
//! comparison: two `meld_widgets` text panes and a link map, driven by
//! `meld_diff::FileComparison`. To switch to the real `FileDiff`, replace
//! everything below the `STAND-IN` marker with:
//!
//! ```ignore
//! pub use crate::views::filediff::{EmbeddedFile, Message, PaneGeometry};
//! pub struct DiffHost(crate::views::filediff::FileDiff);
//! // and forward every method above to `self.0`; `header_height` returns
//! // the height of FileDiff's file-label row above the panes, `set_hover`
//! // forwards if FileDiff grows line highlights, else does nothing.
//! ```

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{Space, column, container, row, stack, text};
use iced::{Color, Element, Length, Subscription, Task, Vector};
use meld_widgets::text_pane::{DEFAULT_OVERSCROLL_LINES, max_scroll_y};
use meld_widgets::{
    Action, Buffer, ChunkKind, DiffChunk, Highlighter, InlineHighlight, LineHighlight, Metrics,
    PaneStyle, ScrollGeometry, line_highlights, link_map, sync_scroll, text_pane,
};

use crate::command::Command;
use crate::shared::Shared;
use crate::ui::{self, menu};
use crate::views::Event;

// ---------------------------------------------------------------- STAND-IN
// Everything below is the temporary stand-in for the real FileDiff.

/// A file to show in one pane.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedFile {
    pub path: PathBuf,
    pub label: String,
    /// The target can be the real working-tree file, which may be edited
    /// (the stand-in shows everything read-only).
    pub read_only: bool,
}

/// Where a pane's lines are: 0-based line `i` is at
/// `y = i * line_height - scroll_y` from the top of the pane's text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneGeometry {
    pub scroll_y: f32,
    pub line_height: f32,
    pub viewport_height: f32,
    pub line_count: usize,
}

/// Height of the file-label row above the panes.
const LABEL_HEIGHT: f32 = 28.0;

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(u64, Arc<Loaded>),
    Pane(usize, Action),
}

#[derive(Debug)]
pub struct Loaded {
    texts: [String; 2],
    chunks: Vec<DiffChunk>,
    inline: [Vec<InlineHighlight>; 2],
    error: Option<String>,
}

struct Pane {
    buffer: Buffer,
    highlighter: Highlighter,
    scroll: Vector,
    viewport_height: f32,
    label: String,
    name: String,
}

impl Pane {
    fn empty() -> Self {
        let mut buffer = Buffer::from_text("");
        buffer.set_read_only(true);
        Pane {
            buffer,
            highlighter: Highlighter::plain(),
            scroll: Vector::ZERO,
            viewport_height: 600.0,
            label: String::new(),
            name: String::new(),
        }
    }

    fn geometry(&self, line_height: f32) -> ScrollGeometry {
        ScrollGeometry {
            scroll_y: self.scroll.y,
            viewport_height: self.viewport_height,
            line_height,
            line_count: self.buffer.line_count(),
            overscroll_lines: DEFAULT_OVERSCROLL_LINES,
        }
    }
}

pub struct DiffHost {
    panes: [Pane; 2],
    chunks: Vec<DiffChunk>,
    reversed: Vec<DiffChunk>,
    inline: [Vec<InlineHighlight>; 2],
    highlights: [Vec<LineHighlight>; 2],
    styles: [PaneStyle; 2],
    metrics: Metrics,
    font: iced::Font,
    generation: u64,
    loading: bool,
    error: Option<String>,
    hover: [Vec<Range<usize>>; 2],
}

fn styles_for(shared: &Shared) -> [PaneStyle; 2] {
    let style = if shared.style.is_dark() {
        PaneStyle::dark()
    } else {
        PaneStyle::light()
    };
    let new = style.for_new_side();
    [style, new]
}

impl DiffHost {
    pub fn embedded(shared: &mut Shared) -> Self {
        DiffHost {
            panes: [Pane::empty(), Pane::empty()],
            chunks: Vec::new(),
            reversed: Vec::new(),
            inline: [Vec::new(), Vec::new()],
            highlights: [Vec::new(), Vec::new()],
            styles: styles_for(shared),
            metrics: Metrics::new(shared.mono_size()),
            font: shared.mono_font(),
            generation: 0,
            loading: false,
            error: None,
            hover: [Vec::new(), Vec::new()],
        }
    }

    pub fn set_files(&mut self, files: Vec<EmbeddedFile>, _shared: &mut Shared) -> Task<Message> {
        self.generation += 1;
        self.loading = true;
        self.hover = [Vec::new(), Vec::new()];
        for (pane, file) in self.panes.iter_mut().zip(&files) {
            pane.label = file.label.clone();
            pane.name = file
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        let generation = self.generation;
        let paths: Vec<PathBuf> = files.into_iter().map(|f| f.path).collect();
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = sender.send(load(&paths));
        });
        Task::perform(receiver, move |loaded| {
            let loaded = loaded.unwrap_or_else(|_| Loaded {
                texts: Default::default(),
                chunks: Vec::new(),
                inline: Default::default(),
                error: Some("The comparison stopped".into()),
            });
            Message::Loaded(generation, Arc::new(loaded))
        })
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn pane_geometry(&self, pane: usize) -> Option<PaneGeometry> {
        let p = self.panes.get(pane)?;
        if self.loading {
            return None;
        }
        Some(PaneGeometry {
            scroll_y: p.scroll.y,
            line_height: self.metrics.line_height,
            viewport_height: p.viewport_height,
            line_count: p.buffer.line_count(),
        })
    }

    pub fn header_height(&self) -> f32 {
        LABEL_HEIGHT
    }

    /// The first changed line, in the target (right) pane.
    pub fn first_change(&self) -> Option<usize> {
        self.chunks.first().map(|c| c.b.start)
    }

    /// Scrolls `pane` so that `line` is in the middle, and the other pane
    /// along with it.
    pub fn scroll_to_line(&mut self, pane: usize, line: usize) -> Task<Message> {
        let lh = self.metrics.line_height;
        let Some(p) = self.panes.get_mut(pane) else {
            return Task::none();
        };
        let max = max_scroll_y(p.buffer.line_count(), lh, DEFAULT_OVERSCROLL_LINES, p.viewport_height);
        p.scroll.y = (line as f32 * lh - p.viewport_height / 2.0).clamp(0.0, max);
        self.sync_from(pane);
        Task::none()
    }

    /// Lines (0-based) of the hovered card in the base and target panes.
    pub fn set_hover(&mut self, lines: [Vec<Range<usize>>; 2]) {
        self.hover = lines;
    }

    fn sync_from(&mut self, source: usize) {
        let lh = self.metrics.line_height;
        let other = 1 - source;
        let chunks = if source == 0 { &self.chunks } else { &self.reversed };
        let y = sync_scroll(&self.panes[source].geometry(lh), &self.panes[other].geometry(lh), chunks);
        self.panes[other].scroll.y = y;
        self.panes[other].scroll.x = self.panes[source].scroll.x;
    }

    pub fn update(&mut self, message: Message, _shared: &mut Shared, _events: &mut Vec<Event>) -> Task<Message> {
        match message {
            Message::Loaded(generation, loaded) => {
                if generation != self.generation {
                    return Task::none();
                }
                self.loading = false;
                self.error = loaded.error.clone();
                for (i, pane) in self.panes.iter_mut().enumerate() {
                    let mut buffer = Buffer::from_text(&loaded.texts[i]);
                    buffer.set_read_only(true);
                    let first = buffer.lines().first().cloned();
                    pane.highlighter = Highlighter::for_file(&pane.name, first.as_deref());
                    pane.buffer = buffer;
                    pane.scroll = Vector::ZERO;
                }
                self.chunks = loaded.chunks.clone();
                self.reversed = self.chunks.iter().map(DiffChunk::reversed).collect();
                self.inline = loaded.inline.clone();
                self.highlights = [
                    line_highlights(&self.chunks, true, None),
                    line_highlights(&self.chunks, false, None),
                ];
            }
            Message::Pane(i, action) => match action {
                Action::Scrolled { x, y } => {
                    self.panes[i].scroll = Vector::new(x, y);
                    self.sync_from(i);
                }
                Action::Resized(size) => self.panes[i].viewport_height = size.height,
                Action::Focused(_) | Action::ContextMenu(..) | Action::Edit(_) => {}
                other => {
                    self.panes[i].buffer.perform(other);
                }
            },
        }
        Task::none()
    }

    pub fn command(&mut self, command: Command, _shared: &mut Shared, _events: &mut Vec<Event>) -> Task<Message> {
        let lh = self.metrics.line_height;
        let pane = &self.panes[1];
        let middle = ((pane.scroll.y + pane.viewport_height / 2.0) / lh).floor() as usize;
        let target = match command {
            Command::NextChange => self.chunks.iter().map(|c| c.b.start).find(|&s| s > middle),
            Command::PreviousChange => self.chunks.iter().rev().map(|c| c.b.start).find(|&s| s < middle),
            _ => None,
        };
        match target {
            Some(line) => self.scroll_to_line(1, line),
            None => Task::none(),
        }
    }

    pub fn settings_changed(&mut self, shared: &Shared) -> Task<Message> {
        self.styles = styles_for(shared);
        self.metrics = Metrics::new(shared.mono_size());
        self.font = shared.mono_font();
        Task::none()
    }

    pub fn unsaved(&self) -> Vec<String> {
        Vec::new()
    }

    pub fn request_close(&mut self, _shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        events.push(Event::Close { status: 0 });
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn toolbar<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        iced::widget::space::horizontal().into()
    }

    pub fn menu_items(&self, _shared: &Shared) -> Vec<menu::Item<Message>> {
        Vec::new()
    }

    pub fn view<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        if let Some(error) = &self.error {
            return container(text(error.as_str()).style(ui::dim_text))
                .center(Length::Fill)
                .into();
        }
        let lh = self.metrics.line_height;
        let label = |i: usize| {
            container(
                text(self.panes[i].label.as_str())
                    .size(13)
                    .wrapping(text::Wrapping::None),
            )
            .clip(true)
            .padding([0, 8])
            .width(Length::Fill)
            .height(LABEL_HEIGHT)
            .align_y(iced::Center)
        };
        let pane = |i: usize| -> Element<'a, Message> {
            let p = &self.panes[i];
            let pane: Element<'a, Message> = text_pane(&p.buffer, &self.styles[i])
                .metrics(self.metrics)
                .font(self.font)
                .scroll(p.scroll)
                .highlights(&self.highlights[i])
                .inline_highlights(&self.inline[i])
                .syntax(&p.highlighter)
                .on_action(move |a| Message::Pane(i, a))
                .into();
            if self.hover[i].is_empty() {
                return pane;
            }
            stack![pane, hover_layer(&self.hover[i], p.scroll.y, lh)].into()
        };
        let links = link_map(&self.chunks, &self.styles[0])
            .scroll(self.panes[0].scroll.y, self.panes[1].scroll.y)
            .line_height(lh);
        column![
            row![label(0), Space::new().width(50), label(1)].height(LABEL_HEIGHT),
            row![pane(0), links, pane(1)].height(Length::Fill),
        ]
        .into()
    }
}

/// Translucent bars over the hovered card's lines.
fn hover_layer<'a, M: 'a>(ranges: &[Range<usize>], scroll_y: f32, lh: f32) -> Element<'a, M> {
    let mut sorted: Vec<Range<usize>> = ranges.to_vec();
    sorted.sort_by_key(|r| r.start);
    let mut col = column![];
    let mut y = 0.0;
    for range in sorted {
        let mut top = range.start as f32 * lh - scroll_y;
        let mut height = (range.end.max(range.start + 1) - range.start) as f32 * lh;
        if top < y {
            height -= y - top;
            top = y;
        }
        if height <= 0.0 {
            continue;
        }
        col = col.push(Space::new().height(top - y));
        col = col.push(
            container(Space::new())
                .width(Length::Fill)
                .height(height)
                .style(|_theme: &iced::Theme| container::Style {
                    background: Some(Color::from_rgba8(53, 132, 228, 0.18).into()),
                    ..container::Style::default()
                }),
        );
        y = top + height;
    }
    container(col).clip(true).width(Length::Fill).height(Length::Fill).into()
}

fn load(paths: &[PathBuf]) -> Loaded {
    let mut texts: [String; 2] = Default::default();
    for (i, path) in paths.iter().take(2).enumerate() {
        match std::fs::read(path) {
            Ok(bytes) => texts[i] = String::from_utf8_lossy(&bytes).into_owned(),
            Err(err) => {
                return Loaded {
                    texts,
                    chunks: Vec::new(),
                    inline: Default::default(),
                    error: Some(format!("Couldn’t read {}: {err}", path.display())),
                };
            }
        }
    }
    let (chunks, inline) = diff(&texts[0], &texts[1]);
    Loaded {
        texts,
        chunks,
        inline,
        error: None,
    }
}

/// Line chunks (left pane is `a`) and inline highlights of same-size
/// replace chunks.
fn diff(left: &str, right: &str) -> (Vec<DiffChunk>, [Vec<InlineHighlight>; 2]) {
    use meld_diff::{Tag, TextBuffer};
    let buffers = vec![TextBuffer::from_text(left), TextBuffer::from_text(right)];
    let Ok((comparison, _)) = meld_diff::FileComparison::new(buffers, Vec::new(), false) else {
        return (Vec::new(), Default::default());
    };
    let lines: [&[String]; 2] = [comparison.buffers()[0].lines(), comparison.buffers()[1].lines()];
    let mut chunks = Vec::new();
    let mut inline: [Vec<InlineHighlight>; 2] = Default::default();
    for chunk in comparison.differ().paired_all_single_changes(0, 1) {
        let kind = match chunk.tag {
            Tag::Equal => continue,
            Tag::Insert => ChunkKind::Insert,
            Tag::Delete => ChunkKind::Delete,
            Tag::Replace => ChunkKind::Replace,
            Tag::Conflict => ChunkKind::Conflict,
        };
        if kind == ChunkKind::Replace && chunk.end_a - chunk.start_a == chunk.end_b - chunk.start_b {
            for (la, lb) in chunk.range_a().zip(chunk.range_b()) {
                let (Some(a), Some(b)) = (lines[0].get(la), lines[1].get(lb)) else {
                    continue;
                };
                for m in meld_diff::inline_highlights(a, b) {
                    if m.tag == Tag::Equal {
                        continue;
                    }
                    if m.end_a > m.start_a {
                        inline[0].push(InlineHighlight {
                            line: la,
                            columns: m.start_a..m.end_a,
                        });
                    }
                    if m.end_b > m.start_b {
                        inline[1].push(InlineHighlight {
                            line: lb,
                            columns: m.start_b..m.end_b,
                        });
                    }
                }
            }
        }
        chunks.push(DiffChunk::new(kind, chunk.range_a(), chunk.range_b()));
    }
    (chunks, inline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stand_in_diff_finds_line_chunks() {
        let (chunks, inline) = diff("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], DiffChunk::new(ChunkKind::Replace, 1..2, 1..2));
        assert_eq!(chunks[1].kind, ChunkKind::Insert);
        assert_eq!(chunks[1].b, 3..4);
        assert!(!inline[0].is_empty() && !inline[1].is_empty());
    }
}
