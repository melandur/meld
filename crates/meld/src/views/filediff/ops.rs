//! Editing, undo, chunk actions, navigation, scrolling and the actions
//! behind shortcuts, buttons and menus.

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use iced::Task;
use meld_diff::{Chunk, EditOutcome, SyncpointAction};
use meld_widgets::{
    Action, ChunkAction, EditAction, InlineHighlight, LineFlash, Motion, Position as WPos, ScrollGeometry, WrapLayout,
    sync_scroll,
};

use super::chunks::{self, CursorDetails, Derived};
use super::edits::{char_column, to_text_edits, widget_replacement};
use super::find::{self, FindMessage};
use super::io;
use super::{
    Act, Banner, BannerAction, CloseState, Dialog, DialogMessage, FileDiff, Flash, Message, MsgId, OVERSCROLL_LINES,
    PaneEdit, Pending, Popup, PopupKind, wrap_mode,
};
use crate::shared::Shared;
use crate::ui::BannerKind;
use crate::views::Event;

const PANE_LEFT: isize = -1;
const PANE_RIGHT: isize = 1;

impl FileDiff {
    // ----- Pane actions and edits -----

    pub(super) fn pane_action(&mut self, pane: usize, action: Action) -> Task<Message> {
        if pane >= self.panes.len() {
            return Task::none();
        }
        match action {
            Action::Scrolled { x, y } => {
                self.panes[pane].scroll = iced::Vector::new(x, y);
                self.sync_scroll(pane);
            }
            Action::Resized(size) => {
                let first = self.panes[pane].viewport.height <= 0.0;
                self.panes[pane].viewport = size;
                if first {
                    let pending = std::mem::take(&mut self.pending_centers);
                    for (p, line) in pending {
                        self.center_line(p, line);
                    }
                }
            }
            Action::WrapColumns(columns) => {
                self.relayout(pane, |w| w.set_columns(columns));
            }
            Action::Focused(true) => {
                self.focused = Some(pane);
                self.last_focused = Some(pane);
                self.update_cursor(pane, true);
            }
            Action::Focused(false) => {
                if self.focused == Some(pane) {
                    self.focused = None;
                }
            }
            Action::ContextMenu(position, point) => {
                // Like GtkTextView, a secondary click outside the selection
                // moves the cursor.
                let buffer = &mut self.panes[pane].buffer;
                let in_selection = buffer.selection().is_some_and(|(s, e)| s <= position && position <= e);
                if !in_selection {
                    buffer.set_cursor(position, false);
                }
                self.focused = Some(pane);
                self.last_focused = Some(pane);
                self.update_cursor(pane, true);
                // Placed at the pointer by an overlay, wherever the view is.
                self.popup = Some(Popup::Context {
                    pane,
                    x: point.x,
                    y: point.y,
                });
            }
            Action::Edit(edit) => {
                if !self.editable_now(pane) {
                    return Task::none();
                }
                self.popup = None;
                let buffer = &mut self.panes[pane].buffer;
                if self.overwrite && matches!(edit, EditAction::Insert(_)) && buffer.selection().is_none() {
                    let c = buffer.cursor();
                    if c.column < buffer.line(c.line).chars().count() {
                        buffer.perform(Action::Select(Motion::Right));
                    }
                }
                let before = buffer.line_count().max(1);
                if let Some(edit) = buffer.perform(Action::Edit(edit)) {
                    self.record_user_edit(pane, edit, before);
                }
                self.focused = Some(pane);
                self.last_focused = Some(pane);
                self.update_cursor(pane, false);
            }
            other => {
                self.popup = None;
                let before = self.panes[pane].buffer.line_count().max(1);
                if let Some(edit) = self.panes[pane].buffer.perform(other) {
                    self.record_user_edit(pane, edit, before);
                }
                self.focused = Some(pane);
                self.last_focused = Some(pane);
                self.update_cursor(pane, false);
            }
        }
        Task::none()
    }

    /// A user edit in a pane: record it and update the comparison.
    fn record_user_edit(&mut self, pane: usize, edit: meld_widgets::Edit, before: usize) {
        self.undo.add_action(PaneEdit {
            pane,
            edit: edit.clone(),
        });
        self.edit_comparison(pane, &edit, before);
        self.after_change();
    }

    /// Mirrors a widget edit (already applied to the pane) in the
    /// comparison; `before` is the pane's line count before it.
    fn edit_comparison(&mut self, pane: usize, edit: &meld_widgets::Edit, before: usize) {
        self.panes[pane].highlighter.edit(edit);
        let newline = self.panes[pane].file.newline.as_str();
        let Some(cmp) = self.cmp.as_mut() else {
            return;
        };
        let started = std::time::Instant::now();
        for te in to_text_edits(edit, before, newline) {
            match cmp.edit(pane, &te) {
                Ok(outcome) => {
                    for applied in &outcome.edits {
                        self.syncpoints
                            .adjust_for_edit(pane, applied.change.start_line, applied.change.size_change);
                    }
                }
                Err(err) => log::error!("edit failed: {err}"),
            }
        }
        log::trace!("edit_comparison: {:?}", started.elapsed());
        self.check_sync(pane);
    }

    /// Debug check that the widget and comparison buffers agree; resyncs
    /// the comparison from the widget buffer if they don't.
    fn check_sync(&mut self, pane: usize) {
        let Some(cmp) = self.cmp.as_mut() else {
            return;
        };
        let widget = self.panes[pane].buffer.lines();
        let same = match cmp.buffer(pane) {
            Some(b) => {
                let lines = b.lines();
                if widget.is_empty() {
                    lines.len() == 1 && lines[0].is_empty()
                } else if lines.len() != widget.len() {
                    false
                } else {
                    // Comparing everything is too slow for big files.
                    !cfg!(debug_assertions) || lines.len() > 20_000 || lines == widget
                }
            }
            None => true,
        };
        if !same {
            log::error!("pane {pane} buffers out of sync; resynchronising");
            debug_assert!(same, "pane buffers out of sync");
            let text = widget.join("\n");
            let _ = cmp.set_text(pane, &text);
        }
    }

    /// Replays edits made by the comparison (chunk actions, merges) on
    /// the widget buffer, as one undo step.
    fn apply_outcome(&mut self, outcome: EditOutcome) {
        let pane = outcome.pane;
        self.undo.begin_group();
        for applied in &outcome.edits {
            let p = &mut self.panes[pane];
            let (range, lines) = widget_replacement(&applied.edit, p.buffer.lines());
            let edit = p.buffer.replace_lines(range, lines);
            p.highlighter.edit(&edit);
            self.undo.add_action(PaneEdit { pane, edit });
            self.syncpoints
                .adjust_for_edit(pane, applied.change.start_line, applied.change.size_change);
        }
        self.undo.end_group();
        self.check_sync(pane);
    }

    /// `on_diffs_changed` and `_after_text_modified`: refresh everything
    /// derived from the chunks.
    pub(super) fn after_change(&mut self) {
        let n = self.panes.len();
        for (i, pane) in self.panes.iter_mut().enumerate() {
            pane.sync_lines = self.syncpoints.lines(i);
        }
        let Some(cmp) = self.cmp.as_ref() else {
            self.derived = Derived::default();
            return;
        };
        let started = std::time::Instant::now();
        if let Some(pane) = self.focus_pane() {
            let line = self.panes[pane].buffer.cursor().line;
            self.cursor = CursorDetails::locate(cmp.differ(), pane, line);
        }
        self.derived = Derived::build(cmp.differ(), n, self.cursor.chunk);
        let built = started.elapsed();
        self.invalidate_inline();
        let inline = started.elapsed();
        self.check_identical();
        log::trace!(
            "after_change: derived {built:?}, inline {inline:?}, total {:?}",
            started.elapsed()
        );
    }

    /// Forgets inline highlights after the chunks changed.
    fn invalidate_inline(&mut self) {
        for pane in &mut self.panes {
            pane.inline.clear();
        }
        let count = self.cmp.as_ref().map_or(0, |c| c.differ().diff_count());
        self.inline_done = vec![false; count];
        self.ensure_inline_visible();
    }

    /// Lines of `pane` in view, with a screen of margin above and below.
    fn lines_around_view(&self, pane: usize) -> std::ops::Range<usize> {
        let p = &self.panes[pane];
        let height = if p.viewport.height > 0.0 {
            p.viewport.height
        } else {
            1000.0
        };
        p.wrap.visible_lines(
            p.scroll.y - height,
            3.0 * height,
            self.metrics.line_height,
            p.buffer.line_count(),
        )
    }

    /// Merged chunk indices visible in any pane (with a screen of margin).
    fn visible_chunks(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for i in 0..self.panes.len() {
            let (Some(hl), Some(merged)) = (self.derived.highlights.get(i), self.derived.pane_merged.get(i)) else {
                continue;
            };
            let vis = self.lines_around_view(i);
            let (first, last) = (vis.start, vis.end + 1);
            let start = hl.partition_point(|h| h.lines.end < first);
            let end = hl.partition_point(|h| h.lines.start <= last);
            out.extend_from_slice(&merged[start..end.max(start)]);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Computes inline highlights of the visible replace chunks that
    /// don't have them yet (`on_diffs_changed`, done lazily).
    pub(super) fn ensure_inline_visible(&mut self) {
        if self.panes.len() < 2 || self.cmp.is_none() {
            return;
        }
        let todo: Vec<usize> = self
            .visible_chunks()
            .into_iter()
            .filter(|&i| self.inline_done.get(i) == Some(&false))
            .collect();
        if todo.is_empty() {
            return;
        }
        let Some(cmp) = self.cmp.as_ref() else {
            return;
        };
        let mut too_long = false;
        for index in todo {
            self.inline_done[index] = true;
            let Some(merged) = cmp.differ().all_changes().get(index) else {
                continue;
            };
            let hl = cmp.inline_highlights(&mut self.inline_cache, merged, self.force_highlight);
            too_long |= hl.too_long;
            for r in hl.ranges {
                let Some(buf) = cmp.buffer(r.pane) else {
                    continue;
                };
                let target = &mut self.panes[r.pane].inline;
                for line in r.start.line..=r.end.line {
                    let Some(text) = buf.line(line) else {
                        break;
                    };
                    let start = if line == r.start.line { r.start.col } else { 0 };
                    let end = if line == r.end.line { r.end.col } else { text.len() };
                    let (s, e) = (char_column(text, start), char_column(text, end));
                    if s < e {
                        target.push(meld_widgets::InlineHighlight { line, columns: s..e });
                    }
                }
            }
        }
        self.inline_cache.clean(cmp.differ().diff_count());
        if too_long
            && !self.force_highlight
            && !self
                .panes
                .iter()
                .any(|p| p.banner.as_ref().is_some_and(|b| b.id == MsgId::SlowHighlight))
        {
            // `_prompt_long_highlighting`
            for (i, pane) in self.panes.iter_mut().enumerate() {
                if pane.banner.is_some() {
                    continue;
                }
                pane.banner = Some(Banner {
                    id: MsgId::SlowHighlight,
                    kind: BannerKind::Info,
                    primary: "Change highlighting incomplete".into(),
                    secondary: (i == 0).then(|| {
                        "Some changes were not highlighted because they were too large. You can force Meld to take longer to highlight larger changes, though this may be slow.".into()
                    }),
                    actions: vec![
                        ("Hide".into(), BannerAction::Hide),
                        ("Keep highlighting".into(), BannerAction::KeepHighlighting),
                    ],
                });
            }
        }
    }

    /// The "identical" and "compared to itself" messages.
    fn check_identical(&mut self) {
        let Some(cmp) = self.cmp.as_ref() else {
            return;
        };
        let n = self.panes.len();
        let duplicate = self
            .self_compare
            .and_then(|p| self.panes.get(p))
            .map(|p| p.file.label());
        if let Some(dup) = duplicate {
            for pane in &mut self.panes {
                if pane.banner.is_none() {
                    pane.banner = Some(Banner {
                        id: MsgId::SelfCompare,
                        kind: BannerKind::Warning,
                        primary: format!("File {dup} is being compared to itself"),
                        secondary: None,
                        actions: vec![("Hide".into(), BannerAction::Hide)],
                    });
                }
            }
            return;
        }
        if cmp.differ().sequences_identical() {
            let has_message = self
                .panes
                .iter()
                .any(|p| p.banner.as_ref().is_some_and(|b| b.id != MsgId::Same));
            if n == 1 || has_message {
                return;
            }
            let active_filters = self.text_filters.iter().any(|f| f.active);
            let newlines: Vec<_> = self.panes.iter().map(|p| p.file.newlines.clone()).collect();
            let different_newlines = !meld_files::all_same(&newlines);
            let labels: Vec<String> = self.panes.iter().map(|p| p.file.label()).collect();
            for pane in self.panes.iter_mut() {
                let mut primary = "Files are identical".to_string();
                let mut secondary = None;
                let mut actions = vec![("Hide".to_string(), BannerAction::Hide)];
                if active_filters {
                    secondary = Some(
                        "Text filters are being used, and may be masking differences between files. Would you like to compare the unfiltered files?"
                            .to_string(),
                    );
                    actions.push(("Show without filters".into(), BannerAction::ShowUnfiltered));
                } else if different_newlines {
                    primary = "Files differ in line endings only".into();
                    secondary = Some(format!(
                        "Files are identical except for differing line endings:\n{}",
                        labels
                            .iter()
                            .zip(&newlines)
                            .map(|(label, nl)| format!("\t{label}: {}", nl.label()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                }
                pane.banner = Some(Banner {
                    id: MsgId::Same,
                    kind: BannerKind::Info,
                    primary,
                    secondary,
                    actions,
                });
            }
        } else {
            for pane in &mut self.panes {
                if pane.banner.as_ref().is_some_and(|b| b.id == MsgId::Same) {
                    pane.banner = None;
                }
            }
        }
    }

    /// `on_cursor_position_changed`
    pub(super) fn update_cursor(&mut self, pane: usize, force: bool) {
        let Some(cmp) = self.cmp.as_ref() else {
            return;
        };
        let line = self.panes[pane].buffer.cursor().line;
        if !force && self.cursor.pane == Some(pane) && self.cursor.line == Some(line) {
            return;
        }
        let new = CursorDetails::locate(cmp.differ(), pane, line);
        let changed = new.chunk != self.cursor.chunk;
        self.cursor = new;
        if changed || force {
            self.derived.set_current(new.chunk);
        }
    }

    // ----- Undo -----

    fn undo_redo(&mut self, redo: bool) {
        if self.cmp.is_none() || self.job.is_some() {
            return;
        }
        let actions = if redo { self.undo.redo() } else { self.undo.undo() };
        let Some(actions) = actions else {
            return;
        };
        for action in actions {
            let edit = if redo {
                action.edit.clone()
            } else {
                action.edit.inverse()
            };
            let pane = action.pane;
            let before = self.panes[pane].buffer.line_count().max(1);
            match self.panes[pane].buffer.apply(&edit) {
                Ok(()) => self.edit_comparison(pane, &edit, before),
                Err(err) => log::error!("undo failed: {err}"),
            }
        }
        self.after_change();
    }

    // ----- Chunk actions -----

    pub(super) fn gutter_action(&mut self, from: usize, to: usize, index: usize, action: ChunkAction) {
        let Some(chunk) = self.derived.pair(from, to).and_then(|p| p.raw.get(index)).copied() else {
            return;
        };
        let action = match action {
            ChunkAction::Replace => meld_diff::ChunkAction::Replace,
            ChunkAction::Delete => meld_diff::ChunkAction::Delete,
            ChunkAction::CopyUp => meld_diff::ChunkAction::CopyUp,
            ChunkAction::CopyDown => meld_diff::ChunkAction::CopyDown,
        };
        self.chunk_action(action, from, to, chunk);
    }

    fn chunk_action(&mut self, action: meld_diff::ChunkAction, src: usize, dst: usize, chunk: Chunk) {
        use meld_diff::ChunkAction as CA;
        let edited = if action == CA::Delete { src } else { dst };
        if !self.editable_now(edited) {
            return;
        }
        let Some(cmp) = self.cmp.as_mut() else {
            return;
        };
        match cmp.chunk_action(action, src, dst, &chunk) {
            Ok(outcome) => {
                self.apply_outcome(outcome);
                if action == CA::Replace {
                    self.panes[dst].buffer.set_cursor(WPos::new(chunk.start_b, 0), false);
                }
                self.after_change();
                // `copy_chunk`, `replace_chunk`, `delete_chunk`: briefly
                // highlight the changed lines.
                let len = chunk.end_a - chunk.start_a;
                let (insert, conflict) = {
                    let colors = &self.styles[edited].chunks;
                    (colors.insert.fill, colors.conflict.fill)
                };
                let (start, color) = match action {
                    CA::CopyUp => (chunk.start_b, insert),
                    CA::CopyDown => (chunk.end_b, insert),
                    CA::Replace if len == 0 => (chunk.start_b, conflict),
                    CA::Replace => (chunk.start_b, insert),
                    CA::Delete => (chunk.start_a, conflict),
                };
                let end = if action == CA::Delete { start } else { start + len };
                self.add_flash(edited, start, end, color, 1.0, 500, false);
            }
            Err(err) => log::error!("chunk action failed: {err}"),
        }
    }

    fn action_chunk(&self, src: usize, dst: usize) -> Option<Chunk> {
        let n = self.panes.len();
        if src >= n || dst >= n {
            return None;
        }
        let cmp = self.cmp.as_ref()?;
        cmp.differ().get_chunk(self.cursor.chunk?, src, Some(dst))
    }

    fn action_panes(&self, direction: isize, reverse: bool) -> Option<(usize, usize)> {
        let src = self.focus_pane()? as isize;
        let dst = src + direction;
        if dst < 0 || dst as usize >= self.panes.len() {
            return None;
        }
        let (src, dst) = (src as usize, dst as usize);
        Some(if reverse { (dst, src) } else { (src, dst) })
    }

    fn sensitivity(&self) -> chunks::Sensitivity {
        let Some(cmp) = self.cmp.as_ref() else {
            return Default::default();
        };
        let editable: Vec<bool> = (0..self.panes.len()).map(|i| self.editable_now(i)).collect();
        let pane = self.cursor.pane.filter(|_| self.focus_pane().is_some());
        chunks::sensitivity(cmp.differ(), self.panes.len(), pane, self.cursor.chunk, &editable)
    }

    pub(super) fn current_sensitivity(&self) -> chunks::Sensitivity {
        self.sensitivity()
    }

    fn push_pull(&mut self, act: Act) {
        use meld_diff::ChunkAction as CA;
        let s = self.sensitivity();
        let n = self.panes.len();
        let (enabled, panes, action) = match act {
            Act::PushLeft => (
                s.push_left,
                if n == 2 {
                    Some((1, 0))
                } else {
                    self.action_panes(PANE_LEFT, false)
                },
                CA::Replace,
            ),
            Act::PushRight => (
                s.push_right,
                if n == 2 {
                    Some((0, 1))
                } else {
                    self.action_panes(PANE_RIGHT, false)
                },
                CA::Replace,
            ),
            Act::PullLeft => (s.pull_left, self.action_panes(PANE_LEFT, true), CA::Replace),
            Act::PullRight => (s.pull_right, self.action_panes(PANE_RIGHT, true), CA::Replace),
            Act::CopyLeftUp => (s.copy_left, self.action_panes(PANE_LEFT, false), CA::CopyUp),
            Act::CopyRightUp => (s.copy_right, self.action_panes(PANE_RIGHT, false), CA::CopyUp),
            Act::CopyLeftDown => (s.copy_left, self.action_panes(PANE_LEFT, false), CA::CopyDown),
            Act::CopyRightDown => (s.copy_right, self.action_panes(PANE_RIGHT, false), CA::CopyDown),
            _ => return,
        };
        if !enabled {
            return;
        }
        let Some((src, dst)) = panes else {
            return;
        };
        if let Some(chunk) = self.action_chunk(src, dst) {
            self.chunk_action(action, src, dst, chunk);
        }
    }

    fn delete_change(&mut self) {
        if !self.sensitivity().delete {
            return;
        }
        let Some(pane) = self.focus_pane() else {
            return;
        };
        let Some(chunk) = self
            .cmp
            .as_ref()
            .zip(self.cursor.chunk)
            .and_then(|(cmp, c)| cmp.differ().get_chunk(c, pane, None))
        else {
            return;
        };
        self.chunk_action(meld_diff::ChunkAction::Delete, pane, pane, chunk);
    }

    /// `_set_merge_action_sensitivity`: (merge all from left, from right,
    /// merge all).
    pub(super) fn merge_sensitivity(&self) -> (bool, bool, bool) {
        let Some(cmp) = self.cmp.as_ref() else {
            return (false, false, false);
        };
        let (left, right) = match self.focus_pane() {
            Some(p) if self.editable_now(p) => cmp.differ().has_mergeable_changes(p),
            _ => (false, false),
        };
        let all = if self.panes.len() == 3 && self.editable_now(1) {
            let (a, b) = cmp.differ().has_mergeable_changes(1);
            a || b
        } else {
            false
        };
        (left, right, all)
    }

    fn merge_all(&mut self, act: Act) {
        let (left, right, all) = self.merge_sensitivity();
        let result = match act {
            Act::MergeAll if all => self.cmp.as_mut().map(|c| (c.merge_all_non_conflicting(), 0)),
            Act::MergeAllLeft if left => self
                .action_panes(PANE_LEFT, true)
                .and_then(|(src, dst)| self.cmp.as_mut().map(|c| (c.pull_all_non_conflicting(src, dst), src))),
            Act::MergeAllRight if right => self
                .action_panes(PANE_RIGHT, true)
                .and_then(|(src, dst)| self.cmp.as_mut().map(|c| (c.pull_all_non_conflicting(src, dst), src))),
            _ => None,
        };
        if let Some((Ok(outcome), resync)) = result {
            self.apply_outcome(outcome);
            self.after_change();
            self.sync_scroll(resync);
        }
    }

    // ----- Navigation and scrolling -----

    fn geometry(&self, pane: usize) -> ScrollGeometry<'_> {
        let p = &self.panes[pane];
        ScrollGeometry {
            scroll_y: p.scroll.y,
            viewport_height: if p.viewport.height > 0.0 {
                p.viewport.height
            } else {
                600.0
            },
            line_height: self.metrics.line_height,
            line_count: p.buffer.display_line_count(),
            overscroll_lines: OVERSCROLL_LINES,
            wrap: Some(&p.wrap),
        }
    }

    /// Brings the wrap layouts up to date with the buffers and the wrap
    /// setting.
    pub(super) fn refresh_wraps(&mut self) {
        let mode = wrap_mode(self.wrap_setting, self.view_options.wrap);
        for pane in 0..self.panes.len() {
            if self.panes[pane].wrap.mode() != mode {
                self.relayout(pane, |w| w.set_mode(mode));
            } else {
                let p = &mut self.panes[pane];
                p.wrap.update(&p.buffer);
            }
        }
    }

    /// Changes the wrap layout of `pane`, keeping the line at the top of
    /// the view in place.
    fn relayout(&mut self, pane: usize, change: impl FnOnce(&mut WrapLayout)) {
        let lh = self.metrics.line_height;
        let p = &mut self.panes[pane];
        let count = p.buffer.display_line_count();
        let (line, frac) = p.wrap.line_at_y(p.scroll.y, lh, count);
        change(&mut p.wrap);
        if p.wrap.update(&p.buffer) {
            let y = p.wrap.y_for_line(line, lh) + frac * p.wrap.line_height(line, lh);
            let viewport = if p.viewport.height > 0.0 {
                p.viewport.height
            } else {
                600.0
            };
            let max = (p.wrap.content_height(count, lh, OVERSCROLL_LINES) - viewport).max(0.0);
            p.scroll.y = y.clamp(0.0, max).floor();
            if p.wrap.is_wrapping() {
                p.scroll.x = 0.0;
            }
        }
    }

    /// Recomputes find matches and text filter dimming around the view of
    /// each pane when the text, the view or the search changed.
    pub(super) fn refresh_decorations(&mut self) {
        let search = self.find.compile();
        let filters = self.active_filters();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        if let Some(re) = &search {
            re.as_str().hash(&mut h);
            self.find.regex.hash(&mut h);
        }
        for f in &filters {
            f.as_str().hash(&mut h);
        }
        let base = h.finish();
        for pane in 0..self.panes.len() {
            let vis = self.lines_around_view(pane);
            let p = &self.panes[pane];
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (base, vis.start, vis.end, p.buffer.revision(), p.buffer.line_count()).hash(&mut h);
            (p.buffer.lines().as_ptr() as usize).hash(&mut h);
            let key = h.finish();
            if p.decorations_key == Some(key) {
                continue;
            }
            let lines = p.buffer.lines();
            let vis = vis.start.min(lines.len())..vis.end.min(lines.len());
            let matches = match &search {
                Some(re) => find::matches_in(re, lines, vis.clone(), self.find.regex),
                None => Vec::new(),
            };
            let mut dimmed = Vec::new();
            if !filters.is_empty() {
                for line in vis {
                    let text = &lines[line];
                    if let Ok(f) = meld_diff::filters::apply_text_filters(text, &filters) {
                        for (a, b) in f.ranges {
                            dimmed.push(InlineHighlight {
                                line,
                                columns: char_column(text, a)..char_column(text, b),
                            });
                        }
                    }
                }
            }
            let p = &mut self.panes[pane];
            p.matches = matches;
            p.dimmed = dimmed;
            p.decorations_key = Some(key);
        }
    }

    /// Starts a fading highlight of `start..end` in `pane`
    /// (`add_fading_highlight`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_flash(
        &mut self,
        pane: usize,
        start: usize,
        end: usize,
        color: iced::Color,
        alpha: f32,
        millis: u64,
        stroke: bool,
    ) {
        if pane >= self.panes.len() {
            return;
        }
        self.flashes.push(Flash {
            pane,
            start,
            end,
            color: iced::Color { a: alpha, ..color },
            started: Instant::now(),
            duration: Duration::from_millis(millis),
            stroke,
        });
        self.animate(Instant::now());
    }

    /// Advances the fading highlights to `now`.
    pub(super) fn animate(&mut self, now: Instant) {
        self.flashes
            .retain(|f| now.saturating_duration_since(f.started) < f.duration);
        for p in &mut self.panes {
            p.flashes.clear();
        }
        for f in &self.flashes {
            let t = now.saturating_duration_since(f.started).as_secs_f32() / f.duration.as_secs_f32().max(1e-3);
            let alpha = f.color.a * (1.0 - t.clamp(0.0, 1.0));
            if let Some(p) = self.panes.get_mut(f.pane) {
                p.flashes.push(LineFlash {
                    start: f.start,
                    end: f.end,
                    color: iced::Color { a: alpha, ..f.color },
                    stroke: f.stroke,
                });
            }
        }
    }

    /// `_sync_vscroll` / `_sync_hscroll` from `master`.
    pub(super) fn sync_scroll(&mut self, master: usize) {
        if self.lock_scrolling {
            return;
        }
        let x = self.panes[master].scroll.x;
        for p in &mut self.panes {
            p.scroll.x = x;
        }
        let n = self.panes.len();
        if n < 2 {
            return;
        }
        const INFLUENCE: [[usize; 2]; 3] = [[1, 2], [0, 2], [1, 0]];
        let mut m = master;
        for &i in &INFLUENCE[master][..n - 1] {
            let chunks = self.derived.pair(m, i).map(|p| p.chunks.as_slice()).unwrap_or(&[]);
            let y = sync_scroll(&self.geometry(m), &self.geometry(i), chunks);
            self.panes[i].scroll.y = y;
            if i == 1 {
                m = 1;
            }
        }
    }

    /// Scrolls `pane` so that `line` is in the middle of the view.
    pub(super) fn center_line(&mut self, pane: usize, line: usize) {
        let g = self.geometry(pane);
        let max = (g.upper() - g.viewport_height).max(0.0);
        let top = self.panes[pane].wrap.y_for_line(line, g.line_height);
        let y = (top + g.line_height / 2.0 - g.viewport_height / 2.0).clamp(0.0, max);
        if self.panes[pane].viewport.height <= 0.0 {
            self.pending_centers.push((pane, line));
        }
        self.panes[pane].scroll.y = y.floor();
    }

    fn line_visible(&self, pane: usize, line: usize) -> bool {
        let g = self.geometry(pane);
        let top = self.panes[pane].wrap.y_for_line(line, g.line_height);
        top >= g.scroll_y && top + g.line_height <= g.scroll_y + g.viewport_height
    }

    /// `go_to_chunk`
    pub(super) fn go_to_chunk(&mut self, target: Option<usize>, pane: Option<usize>, _centered: bool) {
        let Some(target) = target else {
            return;
        };
        let n = self.panes.len();
        let pane = pane.or(self.focus_pane()).unwrap_or(if n > 1 { 1 } else { 0 });
        let Some(chunk) = self.cmp.as_ref().and_then(|c| c.differ().get_chunk(target, pane, None)) else {
            return;
        };
        let buffer = &mut self.panes[pane].buffer;
        if buffer.cursor().line != chunk.start_a {
            buffer.set_cursor(WPos::new(chunk.start_a, 0), false);
        }
        // `scroll_to_chunk_index`: centre each pane on its chunk start.
        let starts = self
            .cmp
            .as_ref()
            .map(|c| c.differ().get_chunk_starts(target))
            .unwrap_or_default();
        for (p, start) in starts.iter().enumerate().take(n) {
            if let Some(start) = start {
                self.center_line(p, *start);
            }
        }
        if !self.line_visible(pane, chunk.start_a) {
            self.center_line(pane, chunk.start_a);
        }
        self.update_cursor(pane, true);
        // Briefly outline the chunk for orientation.
        let color = self.focus_colors.get(pane).copied().unwrap_or(iced::Color::BLACK);
        self.add_flash(pane, chunk.start_a, chunk.end_a, color, 0.3, 400, true);
    }

    fn next_diff(&mut self, down: bool) {
        let target = if down { self.cursor.next } else { self.cursor.prev };
        self.go_to_chunk(target, None, false);
    }

    /// `next_diff(use_viewport=True)` for the mouse wheel over a link
    /// map: when the next or previous change is off screen, go to the
    /// one after or before the middle of the view instead.
    pub(super) fn next_diff_in_view(&mut self, down: bool) {
        let Some(cmp) = self.cmp.as_ref() else {
            return;
        };
        let n = self.panes.len();
        let pane = self
            .cursor
            .pane
            .or(self.focus_pane())
            .unwrap_or(if n > 1 { 1 } else { 0 })
            .min(n - 1);
        let mut target = if down { self.cursor.next } else { self.cursor.prev };
        let g = self.geometry(pane);
        let wrap = &self.panes[pane].wrap;
        let in_view = target
            .and_then(|t| cmp.differ().get_chunk(t, pane, None))
            .is_some_and(|c| {
                let y = wrap.y_for_line(c.start_a, g.line_height);
                y >= g.scroll_y && y <= g.scroll_y + g.viewport_height
            });
        if !in_view {
            let half = g.scroll_y + g.viewport_height / 2.0;
            let (line, _) = wrap.line_at_y(half, g.line_height, g.line_count);
            let loc = chunks::CursorDetails::locate(cmp.differ(), pane, line);
            target = if down { loc.next } else { loc.prev };
            log::debug!("link map scroll: pane {pane}, middle line {line}, target {target:?}");
        }
        self.go_to_chunk(target, Some(pane), false);
    }

    /// `move_cursor`
    fn move_cursor(&mut self, pane: usize, line: usize, focus: bool) -> Task<Message> {
        let line = line.min(self.panes[pane].buffer.line_count().saturating_sub(1));
        self.panes[pane].buffer.set_cursor(WPos::new(line, 0), false);
        if !self.line_visible(pane, line) {
            self.center_line(pane, line);
            self.sync_scroll(pane);
        }
        self.last_focused = Some(pane);
        self.update_cursor(pane, true);
        if focus {
            iced::widget::operation::focus(self.panes[pane].id.clone())
        } else {
            Task::none()
        }
    }

    fn move_cursor_pane(&mut self, delta: isize) -> Task<Message> {
        let n = self.panes.len() as isize;
        let Some(pane) = self.focus_pane() else {
            return Task::none();
        };
        let new_pane = ((pane as isize + delta).rem_euclid(n)) as usize;
        let line = self.panes[pane].buffer.cursor().line;
        let new_line = match self.cmp.as_ref() {
            Some(cmp) => {
                let counts: Vec<usize> = self.panes.iter().map(|p| p.buffer.line_count().max(1)).collect();
                let new_cursor = self.panes[new_pane].buffer.cursor().line;
                chunks::corresponding_line(
                    cmp.differ(),
                    &counts,
                    self.cursor.chunk,
                    line,
                    pane,
                    new_pane,
                    new_cursor,
                )
            }
            None => line,
        };
        self.move_cursor(new_pane, new_line, true)
    }

    // ----- Find and go to line -----

    fn selected_text(&self, pane: usize) -> Option<String> {
        self.panes[pane].buffer.selected_text().filter(|t| !t.contains('\n'))
    }

    fn start_find(&mut self, replace: bool) -> Task<Message> {
        let pane = self.focus_pane().unwrap_or(0);
        self.find.visible = true;
        self.find.replace_mode = replace;
        if let Some(text) = self.selected_text(pane) {
            self.find.text = text;
        }
        self.last_focused = Some(pane);
        Task::batch([
            iced::widget::operation::focus(self.find_id.clone()),
            iced::widget::operation::select_all(self.find_id.clone()),
        ])
    }

    /// `_find_text`: search from the cursor (`offset` characters after
    /// the start of the selection).
    fn find_text(&mut self, offset: usize, backwards: bool) -> bool {
        let Some(pane) = self.focus_pane() else {
            return false;
        };
        self.find.wrapped = false;
        let Some(re) = self.find.compile() else {
            self.find.invalid = self.find.regex && !self.find.text.is_empty();
            self.find.no_match = false;
            return false;
        };
        self.find.invalid = false;
        let buffer = &self.panes[pane].buffer;
        let start = match buffer.selection() {
            Some((s, _)) => s,
            None => buffer.cursor(),
        };
        let from = if backwards {
            start
        } else {
            let len = buffer.line(start.line).chars().count();
            if start.column + offset > len && start.line + 1 < buffer.line_count() {
                WPos::new(start.line + 1, 0)
            } else {
                WPos::new(start.line, start.column + offset)
            }
        };
        let lines = buffer.lines();
        self.find.no_match = !find::has_match(&re, lines, self.find.regex);
        match find::search(&re, lines, from, backwards, self.find.regex) {
            Some(((s, e), wrapped)) => {
                self.find.wrapped = wrapped;
                self.panes[pane].buffer.select(s, e);
                if !self.line_visible(pane, s.line) {
                    self.center_line(pane, s.line);
                    self.sync_scroll(pane);
                }
                self.update_cursor(pane, false);
                true
            }
            None => false,
        }
    }

    pub(super) fn find_message(&mut self, message: FindMessage) -> Task<Message> {
        match message {
            FindMessage::Text(text) => {
                self.find.text = text;
                self.find_text(0, false);
            }
            FindMessage::Replacement(text) => self.find.replacement = text,
            FindMessage::MatchCase(on) => {
                self.find.match_case = on;
                self.find_text(0, false);
            }
            FindMessage::WholeWord(on) => {
                self.find.whole_word = on;
                self.find_text(0, false);
            }
            FindMessage::Regex(on) => {
                self.find.regex = on;
                self.find_text(0, false);
            }
            FindMessage::Next => {
                self.find_text(1, false);
            }
            FindMessage::Previous => {
                self.find_text(0, true);
            }
            FindMessage::ToggleReplace => self.find.replace_mode = !self.find.replace_mode,
            FindMessage::Replace => self.replace_one(),
            FindMessage::ReplaceAll => self.replace_all(),
            FindMessage::Close => {
                self.find.visible = false;
                self.find.wrapped = false;
                if let Some(pane) = self.focus_pane() {
                    return iced::widget::operation::focus(self.panes[pane].id.clone());
                }
            }
        }
        Task::none()
    }

    fn replace_one(&mut self) {
        let Some(pane) = self.focus_pane() else {
            return;
        };
        if !self.editable_now(pane) {
            return;
        }
        let Some(re) = self.find.compile() else {
            return;
        };
        let old = self.panes[pane].buffer.selection();
        let found = self.find_text(0, false);
        let new = self.panes[pane].buffer.selection();
        // Only replace an already-selected match at the cursor.
        if let (true, Some((s, e)), Some(newsel)) = (found, old, new)
            && (s, e) == newsel
        {
            let matched = self.panes[pane].buffer.text_range(s, e);
            let replacement = find::expand_replacement(&re, &matched, &self.find.replacement, self.find.regex);
            let before = self.panes[pane].buffer.line_count().max(1);
            let edit = self.panes[pane].buffer.replace_range(s, e, &replacement);
            self.record_user_edit(pane, edit, before);
            self.find_text(0, false);
        }
    }

    fn replace_all(&mut self) {
        let Some(pane) = self.focus_pane() else {
            return;
        };
        if !self.editable_now(pane) {
            return;
        }
        let Some(re) = self.find.compile() else {
            return;
        };
        let changes = find::replace_all(
            &re,
            self.panes[pane].buffer.lines(),
            &self.find.replacement,
            self.find.regex,
        );
        if changes.is_empty() {
            return;
        }
        if let find::Replaced::Text(lines) = changes {
            // Matches spanning lines: one edit of the whole text.
            let cursor = self.panes[pane].buffer.cursor();
            let count = self.panes[pane].buffer.line_count();
            let before = count.max(1);
            let edit = self.panes[pane].buffer.replace_lines(0..count, lines);
            self.record_user_edit(pane, edit, before);
            self.panes[pane].buffer.set_cursor(cursor, false);
            return;
        }
        let find::Replaced::Lines(changes) = changes else {
            return;
        };
        let cursor = self.panes[pane].buffer.cursor();
        self.undo.begin_group();
        for (line, text) in changes {
            let before = self.panes[pane].buffer.line_count().max(1);
            let edit = self.panes[pane].buffer.replace_lines(line..line + 1, vec![text]);
            self.undo.add_action(PaneEdit {
                pane,
                edit: edit.clone(),
            });
            self.edit_comparison(pane, &edit, before);
        }
        self.undo.end_group();
        self.panes[pane].buffer.set_cursor(cursor, false);
        self.after_change();
    }

    pub(super) fn go_to_line_text(&mut self, text: String) -> Task<Message> {
        let text: String = text.chars().filter(char::is_ascii_digit).collect();
        let Some((pane, _)) = self.go_to else {
            return Task::none();
        };
        self.go_to = Some((pane, text.clone()));
        if let Ok(line) = text.parse::<usize>() {
            let line = line.saturating_sub(1);
            if self.cursor.pane != Some(pane) || self.cursor.line != Some(line) {
                return self.move_cursor(pane, line, false);
            }
        }
        Task::none()
    }

    // ----- Sync points -----

    pub(super) fn syncpoint_action(&self) -> Option<(usize, SyncpointAction)> {
        let pane = self.focus_pane()?;
        if self.panes.len() < 2 {
            return None;
        }
        let line = self.panes[pane].buffer.cursor().line;
        Some((pane, self.syncpoints.action(pane, line)))
    }

    fn refresh_sync_points(&mut self) -> Task<Message> {
        let valid = !self.syncpoints.valid_points().is_empty();
        for pane in &mut self.panes {
            if valid {
                pane.banner = Some(Banner {
                    id: MsgId::Syncpoints,
                    kind: BannerKind::Info,
                    primary: "Live comparison updating disabled".into(),
                    secondary: Some(
                        "Live updating of comparisons is disabled when synchronization points are active. You can still manually refresh the comparison, and live updates will resume when synchronization points are cleared."
                            .into(),
                    ),
                    actions: vec![],
                });
            } else if pane.banner.as_ref().is_some_and(|b| b.id == MsgId::Syncpoints) {
                pane.banner = None;
            }
        }
        for (i, pane) in self.panes.iter_mut().enumerate() {
            pane.sync_lines = self.syncpoints.lines(i);
        }
        self.refresh_comparison()
    }

    // ----- Actions -----

    pub(super) fn act(&mut self, act: Act, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        if !matches!(act, Act::Popup(_)) {
            self.popup = None;
        }
        match act {
            Act::PrevChange => self.next_diff(false),
            Act::NextChange => self.next_diff(true),
            Act::PrevConflict => {
                let (t, p) = (self.cursor.prev_conflict, self.cursor.pane);
                self.go_to_chunk(t, p, false);
            }
            Act::NextConflict => {
                let (t, p) = (self.cursor.next_conflict, self.cursor.pane);
                self.go_to_chunk(t, p, false);
            }
            Act::PushLeft
            | Act::PushRight
            | Act::PullLeft
            | Act::PullRight
            | Act::CopyLeftUp
            | Act::CopyLeftDown
            | Act::CopyRightUp
            | Act::CopyRightDown => self.push_pull(act),
            Act::DeleteChange => self.delete_change(),
            Act::MergeAll | Act::MergeAllLeft | Act::MergeAllRight => self.merge_all(act),
            Act::Undo => self.undo_redo(false),
            Act::Redo => self.undo_redo(true),
            Act::Save => {
                if let Some(pane) = self.focus_pane() {
                    return self.save_file(pane, false, false);
                }
            }
            Act::SaveAs => {
                if let Some(pane) = self.focus_pane() {
                    return self.save_file(pane, true, false);
                }
            }
            Act::SavePane(pane) => return self.save_file(pane, false, false),
            Act::SaveAll => {
                let tasks: Vec<_> = (0..self.panes.len())
                    .filter(|&i| self.is_modified(i))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|i| self.save_file(i, false, false))
                    .collect();
                return Task::batch(tasks);
            }
            Act::Revert => return self.confirm_discard(None, Pending::Revert),
            Act::Refresh => return self.refresh_comparison(),
            Act::Find => return self.start_find(false),
            Act::FindReplace => return self.start_find(true),
            Act::FindNext => {
                if self.find.text.is_empty() {
                    return self.start_find(false);
                }
                self.find_text(1, false);
            }
            Act::FindPrevious => {
                if self.find.text.is_empty() {
                    return self.start_find(false);
                }
                self.find_text(0, true);
            }
            Act::GoToLine => {
                if let Some(pane) = self.focus_pane() {
                    let line = self.panes[pane].buffer.cursor().line + 1;
                    self.go_to = Some((pane, line.to_string()));
                    return Task::batch([
                        iced::widget::operation::focus(self.go_to_id.clone()),
                        iced::widget::operation::select_all(self.go_to_id.clone()),
                    ]);
                }
            }
            Act::OpenExternal | Act::OpenFolder => {
                let Some(pane) = self.focus_pane() else {
                    return Task::none();
                };
                let Some(path) = self.panes[pane].file.path.clone() else {
                    return Task::none();
                };
                let line = self.panes[pane].buffer.cursor().line + 1;
                let settings = meld_files::ExternalEditorSettings {
                    use_system_editor: shared.settings.use_system_editor,
                    custom_editor_command: shared.settings.custom_editor_command.clone(),
                };
                let parent = act == Act::OpenFolder;
                return Task::perform(
                    io::blocking(move || {
                        let argv = meld_files::open_external_command(&path, line, &settings, parent)
                            .map_err(|e| e.to_string())?;
                        meld_files::spawn_detached(&argv).map_err(|e| e.to_string())
                    }),
                    Message::Done,
                );
            }
            Act::CopyFullPath => {
                if let Some(path) = self.focus_pane().and_then(|p| self.panes[p].file.path.clone()) {
                    return iced::clipboard::write(path.display().to_string());
                }
            }
            Act::Cut | Act::Copy => {
                if let Some(pane) = self.focus_pane()
                    && let Some(text) = self.panes[pane].buffer.selected_text()
                {
                    let task = iced::clipboard::write(text);
                    if act == Act::Cut && self.editable_now(pane) {
                        let _ = self.pane_action(pane, Action::Edit(EditAction::Delete));
                    }
                    return task;
                }
            }
            Act::Paste => {
                if let Some(pane) = self.focus_pane() {
                    return iced::clipboard::read().map(move |t| Message::Paste(pane, t));
                }
            }
            Act::Swap => return self.swap(shared),
            Act::NextPane => return self.move_cursor_pane(1),
            Act::PrevPane => return self.move_cursor_pane(-1),
            Act::FormatPatch => {
                if self.panes.len() > 1 {
                    let text = self.make_patch(true, false);
                    self.dialog = Some(Dialog::Patch {
                        left: true,
                        reverse: false,
                        text,
                    });
                }
            }
            Act::ToggleOverview => {
                self.show_overview_map = !self.show_overview_map;
                shared.settings.show_overview_map = self.show_overview_map;
                events.push(Event::SettingsChanged);
            }
            Act::ToggleLockScrolling => self.lock_scrolling = !self.lock_scrolling,
            Act::ToggleTextFilter(i) => {
                if let Some(f) = self.text_filters.get_mut(i)
                    && f.regex.is_some()
                {
                    f.active = !f.active;
                    return self.refresh_comparison();
                }
            }
            Act::ToggleIgnoreBlank => {
                shared.settings.ignore_blank_lines = !shared.settings.ignore_blank_lines;
                if self.embedded {
                    // The parent may not forward settings changes.
                    self.ignore_blank_lines = shared.settings.ignore_blank_lines;
                    events.push(Event::SettingsChanged);
                    return self.refresh_comparison();
                }
                events.push(Event::SettingsChanged);
            }
            Act::SyncPoint => {
                if let Some((pane, action)) = self.syncpoint_action() {
                    let line = self.panes[pane].buffer.cursor().line;
                    match action {
                        SyncpointAction::Add | SyncpointAction::Match => self.syncpoints.add(pane, line),
                        SyncpointAction::Move => {
                            self.syncpoints.move_unmatched(pane, line);
                        }
                        SyncpointAction::Delete => {
                            self.syncpoints.remove(pane, line);
                        }
                        SyncpointAction::Disabled => return Task::none(),
                    }
                    return self.refresh_sync_points();
                }
            }
            Act::ClearSyncPoints => {
                self.syncpoints.clear();
                return self.refresh_sync_points();
            }
            Act::ToggleLineNumbers => self.view_options.show_line_numbers = !self.view_options.show_line_numbers,
            Act::ToggleWhitespace => self.view_options.show_whitespace = !self.view_options.show_whitespace,
            Act::ToggleCurrentLine => {
                self.view_options.highlight_current_line = !self.view_options.highlight_current_line
            }
            Act::ToggleWrap => {
                self.view_options.wrap = !self.view_options.wrap;
                self.refresh_wraps();
            }
            Act::ToggleReadOnly(pane) => {
                if let Some(p) = self.panes.get_mut(pane) {
                    p.editable = !p.editable;
                }
                self.refresh_read_only();
            }
            Act::ChooseEncoding(pane) => {
                self.dialog = Some(Dialog::Encoding {
                    pane,
                    filter: String::new(),
                });
                return iced::widget::operation::focus(self.dialog_input_id.clone());
            }
            Act::ChooseLanguage(pane) => {
                self.dialog = Some(Dialog::Language {
                    pane,
                    filter: String::new(),
                });
                return iced::widget::operation::focus(self.dialog_input_id.clone());
            }
            Act::SetNewline(pane, newline) => {
                if let Some(p) = self.panes.get_mut(pane)
                    && p.file.newline != newline
                {
                    p.file.newline = newline;
                    p.extra_modified = true;
                }
            }
            Act::OpenFile(pane) => {
                let title = match pane {
                    0 => "Open Left File",
                    1 if self.panes.len() == 3 => "Open Middle File",
                    _ => "Open Right File",
                };
                let dir = self.panes[pane].file.path.as_ref().and_then(|p| p.parent()).map(std::path::PathBuf::from);
                return crate::ui::chooser::pick_file(title, dir).map(move |path| Message::OpenChosen(pane, path));
            }
            Act::Display(pane) => self.dialog = Some(Dialog::Display { pane }),
            Act::ToggleOverwrite => self.overwrite = !self.overwrite,
            Act::Popup(kind) => {
                self.popup = match (kind, self.popup) {
                    (Some(PopupKind::CopyMenu), Some(Popup::CopyMenu)) => None,
                    (Some(PopupKind::Filters), Some(Popup::Filters)) => None,
                    (Some(PopupKind::CopyMenu), _) => Some(Popup::CopyMenu),
                    (Some(PopupKind::Filters), _) => Some(Popup::Filters),
                    (None, _) => None,
                };
            }
        }
        Task::none()
    }

    /// `action_swap`
    fn swap(&mut self, _shared: &mut Shared) -> Task<Message> {
        if self.panes.len() != 2 {
            return Task::none();
        }
        let unnamed = self.panes.iter().any(|p| p.file.path.is_none());
        let modified = (0..2).any(|i| self.is_modified(i));
        if unnamed || modified {
            self.dialog = Some(Dialog::Error {
                primary: "Can't swap unsaved files".into(),
                secondary: "Files must be saved to disk before swapping panes.".into(),
            });
            return Task::none();
        }
        if self.extras.tab_label.is_some() {
            self.dialog = Some(Dialog::Error {
                primary: "Can't swap version control comparisons".into(),
                secondary: "Swapping panes is not supported in version control mode.".into(),
            });
            return Task::none();
        }
        self.panes.swap(0, 1);
        for pane in &mut self.panes {
            pane.language_chosen = false;
        }
        let paths = self.panes.iter().map(|p| p.file.path.clone()).collect();
        // Force a reload of both panes with the swapped labels.
        for pane in &mut self.panes {
            let label = pane.file.custom_label.take();
            let path = pane.file.path.take();
            pane.file = super::PaneFile::new();
            pane.file.custom_label = label;
            pane.file.path = path;
        }
        self.load_files(paths, vec![None, None])
    }

    pub(super) fn make_patch(&self, left: bool, reverse: bool) -> String {
        let texts: Vec<String> = self.panes.iter().map(|p| p.buffer.lines().join("\n")).collect();
        let mut labels: Vec<String> = self.panes.iter().map(|p| p.file.label()).collect();
        while labels.len() < 3 {
            labels.push("<unnamed>".into());
        }
        let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let side = if left || self.panes.len() < 3 {
            meld_diff::PatchSide::Left
        } else {
            meld_diff::PatchSide::Right
        };
        meld_diff::make_patch(&text_refs, &label_refs, side, reverse).unwrap_or_default()
    }

    pub(super) fn dialog_message(
        &mut self,
        message: DialogMessage,
        shared: &mut Shared,
        events: &mut Vec<Event>,
    ) -> Task<Message> {
        match message {
            DialogMessage::Cancel => {
                if matches!(self.dialog, Some(Dialog::ConfirmClose { .. })) {
                    self.close_state = CloseState::Normal;
                }
                if matches!(self.dialog, Some(Dialog::Resolve)) {
                    // Cancel still closes, without resolving.
                    self.dialog = None;
                    return self.finish_close(events);
                }
                self.dialog = None;
            }
            DialogMessage::CloseCheck(i, on) => {
                if let Some(Dialog::ConfirmClose { save }) = &mut self.dialog
                    && let Some(s) = save.get_mut(i)
                {
                    *s = on;
                }
            }
            DialogMessage::CloseSave => {
                let Some(Dialog::ConfirmClose { save }) = self.dialog.take() else {
                    return Task::none();
                };
                let tasks: Vec<_> = save
                    .iter()
                    .enumerate()
                    .filter(|(i, s)| **s && self.is_modified(*i))
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|i| self.save_file(i, false, false))
                    .collect();
                if tasks.is_empty() {
                    return self.finish_close(events);
                }
                return Task::batch(tasks);
            }
            DialogMessage::CloseDiscard => {
                self.dialog = None;
                return self.finish_close(events);
            }
            DialogMessage::DiscardConfirm => {
                if let Some(Dialog::ConfirmDiscard { then, .. }) = self.dialog.take() {
                    return self.run_pending(then);
                }
            }
            DialogMessage::Resolve => {
                self.dialog = None;
                let path = self.panes.get(1).and_then(|p| p.file.target().cloned());
                if let Some(path) = path {
                    return Task::perform(io::blocking(move || io::resolve(path)), Message::Resolved);
                }
                return self.finish_close(events);
            }
            DialogMessage::PatchLeft(left) => {
                if let Some(Dialog::Patch { reverse, .. }) = self.dialog {
                    let text = self.make_patch(left, reverse);
                    self.dialog = Some(Dialog::Patch { left, reverse, text });
                }
            }
            DialogMessage::PatchReverse(reverse) => {
                if let Some(Dialog::Patch { left, .. }) = self.dialog {
                    let text = self.make_patch(left, reverse);
                    self.dialog = Some(Dialog::Patch { left, reverse, text });
                }
            }
            DialogMessage::PatchCopy => {
                if let Some(Dialog::Patch { text, .. }) = self.dialog.take() {
                    return iced::clipboard::write(text);
                }
            }
            DialogMessage::PatchSave => {
                // Next to the compared files rather than in the chooser's
                // working folder.
                let dir = self.panes.iter().find_map(|p| p.file.target()?.parent().map(std::path::PathBuf::from));
                return crate::ui::chooser::save_file("Save Patch", dir, None).map(Message::PatchSaveChosen);
            }
            DialogMessage::Filter(text) => {
                if let Some(Dialog::Encoding { filter, .. } | Dialog::Language { filter, .. }) = &mut self.dialog {
                    *filter = text;
                }
            }
            DialogMessage::PickEncoding(label) => {
                if let Some(Dialog::Encoding { pane, .. }) = self.dialog.take()
                    && let Some(enc) = meld_files::TextEncoding::for_label(label)
                {
                    // `reload_with_encoding`
                    return self.confirm_discard(Some(vec![pane]), Pending::ReloadPane(pane, Some(enc.encoding)));
                }
            }
            DialogMessage::PickLanguage(lang) => {
                if let Some(Dialog::Language { pane, .. }) = self.dialog.take()
                    && let Some(p) = self.panes.get_mut(pane)
                {
                    p.language = lang;
                    p.language_chosen = true;
                    p.highlighter = match lang {
                        Some(id) => meld_widgets::Highlighter::for_language(id),
                        None => meld_widgets::Highlighter::plain(),
                    };
                }
            }
        }
        let _ = shared;
        Task::none()
    }
}
