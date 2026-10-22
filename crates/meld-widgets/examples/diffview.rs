//! Two editable panes with link map, action gutters, chunk maps, syntax
//! highlighting and synced scrolling.
//!
//! Keys: Alt+Up/Down jump between changes, Ctrl+Z / Ctrl+Shift+Z undo and
//! redo (handled by the app through `keyboard::listen`, proving the pane
//! leaves them uncaptured), Ctrl+D toggles the dark style, Ctrl+W toggles
//! whitespace markers. Pass a file count argument (`diffview 100000`) for a
//! large generated file.

use iced::keyboard::{self, Key, key};
use iced::widget::{column, container, row, text};
use iced::{Element, Length, Subscription, Task, Theme, Vector};
use meld_widgets::action_gutter::Direction;
use meld_widgets::text_pane::{DEFAULT_OVERSCROLL_LINES, max_scroll_y};
use meld_widgets::{
    Action, ActionMode, Buffer, ChunkAction, ChunkKind, DiffChunk, Edit, Highlighter,
    InlineHighlight, LineHighlight, MapChunk, MapScroll, Metrics, PaneStyle, ScrollGeometry,
    action_gutter, chunk_map, line_highlights, link_map, map_chunks, sync_scroll, text_pane,
};

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Meld widgets — diffview")
        .subscription(App::subscription)
        .theme(App::theme)
        .window_size((1400.0, 900.0))
        .run()
}

struct Pane {
    buffer: Buffer,
    highlighter: Highlighter,
    scroll: Vector,
    viewport_height: f32,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl Pane {
    fn new(text: &str) -> Self {
        let mut buffer = Buffer::from_text(text);
        buffer.set_indentation(4, true);
        Self {
            buffer,
            highlighter: Highlighter::for_file("example.py", None),
            scroll: Vector::ZERO,
            viewport_height: 600.0,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    fn geometry(&self, lh: f32) -> ScrollGeometry {
        ScrollGeometry {
            scroll_y: self.scroll.y,
            viewport_height: self.viewport_height,
            line_height: lh,
            line_count: self.buffer.line_count(),
            overscroll_lines: DEFAULT_OVERSCROLL_LINES,
        }
    }
}

struct App {
    panes: [Pane; 2],
    style: PaneStyle,
    /// Style for the right ("new") pane: insert/delete colours swapped.
    style_new: PaneStyle,
    dark: bool,
    show_whitespace: bool,
    metrics: Metrics,
    chunks: Vec<DiffChunk>,
    inline: [Vec<InlineHighlight>; 2],
    current: Option<usize>,
    focused: usize,
    // Derived from `chunks`, cached for `view`.
    highlights: [Vec<LineHighlight>; 2],
    maps: [Vec<MapChunk>; 2],
    reversed: Vec<DiffChunk>,
}

#[derive(Debug, Clone)]
enum Message {
    Pane(usize, Action),
    MapScroll(usize, MapScroll),
    Chunk(usize, usize, ChunkAction),
    Key(keyboard::Event),
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let big: Option<usize> = std::env::args().nth(1).and_then(|a| a.parse().ok());
        let (left, right) = sample(big.unwrap_or(60));
        let mut app = Self {
            panes: [Pane::new(&left), Pane::new(&right)],
            style: PaneStyle::light(),
            style_new: PaneStyle::light().for_new_side(),
            dark: false,
            show_whitespace: false,
            metrics: Metrics::new(14.0),
            chunks: Vec::new(),
            inline: [Vec::new(), Vec::new()],
            current: None,
            focused: 0,
            highlights: [Vec::new(), Vec::new()],
            maps: [Vec::new(), Vec::new()],
            reversed: Vec::new(),
        };
        app.panes[1].buffer.set_read_only(false);
        app.rediff();
        (app, Task::none())
    }

    fn theme(&self) -> Theme {
        if self.dark { Theme::Dark } else { Theme::Light }
    }

    fn subscription(&self) -> Subscription<Message> {
        keyboard::listen().map(Message::Key)
    }

    /// Chunks oriented from pane `from` to the other pane.
    fn chunks_from(&self, from: usize) -> Vec<DiffChunk> {
        if from == 0 {
            self.chunks.clone()
        } else {
            self.chunks.iter().map(DiffChunk::reversed).collect()
        }
    }

    fn rediff(&mut self) {
        let (chunks, inline) = diff(self.panes[0].buffer.lines(), self.panes[1].buffer.lines());
        self.chunks = chunks;
        self.inline = inline;
        self.maps = [
            map_chunks(&self.chunks, true),
            map_chunks(&self.chunks, false),
        ];
        self.reversed = self.chunks.iter().map(DiffChunk::reversed).collect();
        self.update_current();
    }

    fn update_current(&mut self) {
        let line = self.panes[self.focused].buffer.cursor().line;
        let side_a = self.focused == 0;
        self.current = self.chunks.iter().position(|c| {
            let r = if side_a { &c.a } else { &c.b };
            r.contains(&line) || (r.is_empty() && r.start == line)
        });
        self.highlights = [
            line_highlights(&self.chunks, true, self.current),
            line_highlights(&self.chunks, false, self.current),
        ];
    }

    fn sync_from(&mut self, source: usize) {
        let lh = self.metrics.line_height;
        let other = 1 - source;
        let chunks = self.chunks_from(source);
        let y = sync_scroll(
            &self.panes[source].geometry(lh),
            &self.panes[other].geometry(lh),
            &chunks,
        );
        self.panes[other].scroll.y = y;
        self.panes[other].scroll.x = self.panes[source].scroll.x;
    }

    fn record(&mut self, pane: usize, edit: Edit) {
        self.panes[pane].highlighter.edit(&edit);
        self.panes[pane].undo.push(edit);
        self.panes[pane].redo.clear();
        self.rediff();
    }

    fn update(&mut self, message: Message) {
        let lh = self.metrics.line_height;
        match message {
            Message::Pane(i, action) => match action {
                Action::Scrolled { x, y } => {
                    self.panes[i].scroll = Vector::new(x, y);
                    self.sync_from(i);
                }
                Action::Resized(size) => {
                    self.panes[i].viewport_height = size.height;
                }
                Action::Focused(true) => {
                    self.focused = i;
                    self.update_current();
                }
                Action::Focused(false) | Action::ContextMenu(..) => {}
                other => {
                    if let Some(edit) = self.panes[i].buffer.perform(other) {
                        self.record(i, edit);
                    }
                    self.focused = i;
                    self.update_current();
                }
            },
            Message::MapScroll(i, s) => {
                self.panes[i].scroll.y = s.scroll_y;
                self.sync_from(i);
            }
            Message::Chunk(src, index, action) => {
                let chunk = self.chunks_from(src)[index].clone();
                let dst = 1 - src;
                let lines: Vec<String> = self.panes[src].buffer.lines()[chunk.a.clone()].to_vec();
                let (pane, edit) = match action {
                    ChunkAction::Replace => (
                        dst,
                        self.panes[dst].buffer.replace_lines(chunk.b.clone(), lines),
                    ),
                    ChunkAction::Delete => (
                        src,
                        self.panes[src]
                            .buffer
                            .replace_lines(chunk.a.clone(), vec![]),
                    ),
                    ChunkAction::CopyUp => {
                        let at = chunk.b.start;
                        (dst, self.panes[dst].buffer.replace_lines(at..at, lines))
                    }
                    ChunkAction::CopyDown => {
                        let at = chunk.b.end;
                        (dst, self.panes[dst].buffer.replace_lines(at..at, lines))
                    }
                };
                self.record(pane, edit);
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                let f = self.focused;
                match key.as_ref() {
                    Key::Character("z") if modifiers.command() => {
                        let pane = &mut self.panes[f];
                        let (from, to) = if modifiers.shift() {
                            (&mut pane.redo, &mut pane.undo)
                        } else {
                            (&mut pane.undo, &mut pane.redo)
                        };
                        if let Some(edit) = from.pop() {
                            let e = if modifiers.shift() {
                                edit.clone()
                            } else {
                                edit.inverse()
                            };
                            if pane.buffer.apply(&e).is_ok() {
                                pane.highlighter.edit(&e);
                                to.push(edit);
                            }
                            self.rediff();
                        }
                    }
                    Key::Character("d") if modifiers.command() => {
                        self.dark = !self.dark;
                        self.style = if self.dark {
                            PaneStyle::dark()
                        } else {
                            PaneStyle::light()
                        };
                        self.style_new = self.style.for_new_side();
                    }
                    Key::Character("w") if modifiers.command() => {
                        self.show_whitespace = !self.show_whitespace;
                    }
                    Key::Named(key::Named::ArrowDown | key::Named::ArrowUp) if modifiers.alt() => {
                        let down = matches!(key.as_ref(), Key::Named(key::Named::ArrowDown));
                        let line = self.panes[f].buffer.cursor().line;
                        let starts: Vec<usize> =
                            self.chunks_from(f).iter().map(|c| c.a.start).collect();
                        let target = if down {
                            starts.iter().copied().find(|s| *s > line)
                        } else {
                            starts.iter().rev().copied().find(|s| *s < line)
                        };
                        if let Some(t) = target {
                            let pane = &mut self.panes[f];
                            pane.buffer
                                .set_cursor(meld_widgets::Position::new(t, 0), false);
                            let max = max_scroll_y(
                                pane.buffer.line_count(),
                                lh,
                                DEFAULT_OVERSCROLL_LINES,
                                pane.viewport_height,
                            );
                            pane.scroll.y =
                                (t as f32 * lh - pane.viewport_height / 2.0).clamp(0.0, max);
                            self.sync_from(f);
                            self.update_current();
                        }
                    }
                    _ => {}
                }
            }
            Message::Key(_) => {}
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let lh = self.metrics.line_height;
        let style = &self.style;
        let styles = [&self.style, &self.style_new];
        let max_lines = self
            .panes
            .iter()
            .map(|p| p.buffer.line_count())
            .max()
            .unwrap_or(1);

        let hl = &self.highlights;
        let maps = &self.maps;
        let reversed = &self.reversed;

        let pane = |i: usize| -> Element<'_, Message> {
            let p = &self.panes[i];
            text_pane(&p.buffer, styles[i])
                .metrics(self.metrics)
                .scroll(p.scroll)
                .highlights(&hl[i])
                .inline_highlights(&self.inline[i])
                .syntax(&p.highlighter)
                .show_whitespace(self.show_whitespace)
                .highlight_current_line(true)
                .sync_points(if i == 0 { &[3] } else { &[] })
                .on_action(move |a| Message::Pane(i, a))
                .into()
        };
        let map = |i: usize| -> Element<'_, Message> {
            let p = &self.panes[i];
            chunk_map(&maps[i], styles[i])
                .lines(p.buffer.line_count(), max_lines)
                .viewport(p.scroll.y, p.viewport_height, lh)
                .on_scroll(move |s| Message::MapScroll(i, s))
                .into()
        };

        let body = row![
            map(0),
            pane(0),
            action_gutter(&self.chunks, style)
                .scroll(self.panes[0].scroll.y)
                .line_height(lh)
                .current(self.current)
                .on_action(|c, a| Message::Chunk(0, c, a)),
            link_map(&self.chunks, style)
                .scroll(self.panes[0].scroll.y, self.panes[1].scroll.y)
                .line_height(lh)
                .current(self.current),
            action_gutter(reversed, &self.style_new)
                .scroll(self.panes[1].scroll.y)
                .line_height(lh)
                .direction(Direction::Left)
                .current(self.current)
                .on_action(|c, a| Message::Chunk(1, c, a)),
            pane(1),
            map(1),
        ]
        .height(Length::Fill);

        let c = self.panes[self.focused].buffer.cursor();
        let status = text(format!(
            "pane {}  Ln {}, Col {}  |  {} changes  |  current: {:?}  |  mode: {:?} (hold Shift/Ctrl)",
            self.focused + 1,
            c.line + 1,
            c.column + 1,
            self.chunks.len(),
            self.current,
            ActionMode::Replace,
        ))
        .size(12);

        column![body, container(status).padding(4)].into()
    }
}

/// Line LCS diff producing Meld-style chunks (left = `a`) and inline
/// highlights for same-size replace chunks.
fn diff(a: &[String], b: &[String]) -> (Vec<DiffChunk>, [Vec<InlineHighlight>; 2]) {
    // Trim common prefix/suffix so big similar files stay cheap.
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (n, m) = (am.len(), bm.len());
    let mut chunks = Vec::new();
    if n * m > 25_000_000 {
        if n + m > 0 {
            chunks.push(DiffChunk::new(
                ChunkKind::Replace,
                pre..pre + n,
                pre..pre + m,
            ));
        }
    } else {
        let mut dp = vec![0u32; (n + 1) * (m + 1)];
        let idx = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[idx(i, j)] = if am[i] == bm[j] {
                    dp[idx(i + 1, j + 1)] + 1
                } else {
                    dp[idx(i + 1, j)].max(dp[idx(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        let (mut si, mut sj) = (0, 0);
        let flush = |i: usize, j: usize, si: usize, sj: usize, out: &mut Vec<DiffChunk>| {
            if i > si || j > sj {
                let kind = match (i > si, j > sj) {
                    (true, true) => ChunkKind::Replace,
                    (true, false) => ChunkKind::Delete,
                    _ => ChunkKind::Insert,
                };
                out.push(DiffChunk::new(kind, pre + si..pre + i, pre + sj..pre + j));
            }
        };
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                flush(i, j, si, sj, &mut chunks);
                i += 1;
                j += 1;
                si = i;
                sj = j;
            } else if j >= m || (i < n && dp[idx(i + 1, j)] >= dp[idx(i, j + 1)]) {
                i += 1;
            } else {
                j += 1;
            }
        }
        flush(i, j, si, sj, &mut chunks);
    }

    let mut inline = [Vec::new(), Vec::new()];
    for c in &chunks {
        if c.kind != ChunkKind::Replace || c.a.len() != c.b.len() {
            continue;
        }
        for (la, lb) in c.a.clone().zip(c.b.clone()) {
            let (x, y): (Vec<char>, Vec<char>) = (a[la].chars().collect(), b[lb].chars().collect());
            let p = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
            let s = x[p..]
                .iter()
                .rev()
                .zip(y[p..].iter().rev())
                .take_while(|(p, q)| p == q)
                .count();
            if p < x.len() - s {
                inline[0].push(InlineHighlight {
                    line: la,
                    columns: p..x.len() - s,
                });
            }
            if p < y.len() - s {
                inline[1].push(InlineHighlight {
                    line: lb,
                    columns: p..y.len() - s,
                });
            }
        }
    }
    (chunks, inline)
}

fn sample(functions: usize) -> (String, String) {
    let mut left = String::from(
        "#!/usr/bin/env python3\n\"\"\"Sample module for the diff view.\"\"\"\n\nimport os\nimport sys\n\n",
    );
    let mut right = String::from(
        "#!/usr/bin/env python3\n\"\"\"Sample module for the diff view (edited).\"\"\"\n\nimport os\nimport re\nimport sys\n\n",
    );
    for i in 0..functions {
        let body = format!(
            "def function_{i}(value, scale=2):\n    \"\"\"Return a scaled value.\"\"\"\n    result = value * scale + {i}\n    if result > 100:\n        return result - 100\n    return result\n\n"
        );
        left.push_str(&body);
        match i % 7 {
            1 => right.push_str(&body.replace("scale + ", "scale - ")),
            3 => {}
            4 => {
                right.push_str(&body);
                right.push_str(&format!(
                    "class Helper{i}:\n    name = 'héllo wörld ✓'\n\tdef run(self):\n\t\treturn {i}.5e3  # tab indented\n\n"
                ));
            }
            5 => right
                .push_str(&body.replace("    if result > 100:\n        return result - 100\n", "")),
            _ => right.push_str(&body),
        }
    }
    left.push_str("if __name__ == '__main__':\n    sys.exit(function_0(int(sys.argv[1])))\n");
    right.push_str("if __name__ == '__main__':\n    print(function_0(int(sys.argv[1])))\n");
    (left, right)
}
