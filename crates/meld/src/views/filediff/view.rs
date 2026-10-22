//! Drawing the comparison (`filediff.ui`): file headers, message areas,
//! panes with action gutters and link maps, overview maps, the find bar
//! and status bars; the header-bar toolbar and the main menu section.

use std::fmt;

use iced::widget::{
    Space, button, checkbox, column, container, pick_list, row, space, stack, text, text_input,
    tooltip,
};
use iced::{Alignment, Background, Element, Length, Theme};
use meld_files::NewlineType;
use meld_widgets::action_gutter::Direction;
use meld_widgets::{action_gutter, chunk_map, link_map, text_pane};

use super::find::FindMessage;
use super::{Act, FileDiff, LoadState, Message, OVERSCROLL_LINES, PopupKind};
use crate::shared::Shared;
use crate::ui::{self, icon, menu};

const GUTTER_WIDTH: f32 = 20.0;
const LINK_WIDTH: f32 = 50.0;
/// Width between two panes: gutter, link map, gutter.
const BETWEEN: f32 = GUTTER_WIDTH * 2.0 + LINK_WIDTH;

/// Height of the file header row above the panes.
pub const HEADER_ROW_HEIGHT: f32 = 30.0;
/// Height of a pane's message area in embedded comparisons.
pub const EMBEDDED_BANNER_HEIGHT: f32 = 64.0;

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewlineChoice(pub NewlineType);

impl fmt::Display for NewlineChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            NewlineType::Lf => "LF",
            NewlineType::CrLf => "CRLF",
            NewlineType::Cr => "CR",
        })
    }
}

const NEWLINES: [NewlineChoice; 3] = [
    NewlineChoice(NewlineType::Lf),
    NewlineChoice(NewlineType::CrLf),
    NewlineChoice(NewlineType::Cr),
];

fn act(a: Act) -> Message {
    Message::Act(a)
}

/// Which status bar items fit a pane `width` wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StatusItems {
    pub display_label: bool,
    pub encoding: bool,
    pub newline: bool,
    pub language: bool,
    pub long_position: bool,
    pub overwrite: bool,
}

pub(super) fn status_items(width: f32) -> StatusItems {
    StatusItems {
        display_label: width > 300.0,
        long_position: width > 240.0,
        newline: width > 340.0,
        language: width > 400.0,
        encoding: width > 480.0,
        overwrite: width > 540.0,
    }
}

pub(super) fn encoding_label(p: &super::Pane) -> String {
    if p.file.encoding.bom {
        format!("{} (BOM)", p.file.encoding.name())
    } else {
        p.file.encoding.name().to_string()
    }
}

pub(super) fn language_label(p: &super::Pane) -> &'static str {
    p.language
        .and_then(|id| meld_files::LANGUAGES.iter().find(|l| l.id == id))
        .map(|l| l.name)
        .unwrap_or(tr!("Plain Text"))
}

pub(super) fn newline_picker<'a>(pane: usize, newline: NewlineType) -> Element<'a, Message> {
    pick_list(
        NEWLINES,
        Some(NewlineChoice(newline)),
        move |c: NewlineChoice| act(Act::SetNewline(pane, c.0)),
    )
    .text_size(12)
    .padding([2, 6])
    .style(|theme: &Theme, status| {
        let mut s = pick_list::default(theme, status);
        s.background = Background::Color(iced::Color::TRANSPARENT);
        s.border = s.border.width(0);
        s
    })
    .into()
}

fn small_button<'a>(
    label: impl text::IntoFragment<'a>,
    message: Option<Message>,
) -> button::Button<'a, Message> {
    button(text(label).size(12).wrapping(text::Wrapping::None))
        .padding([2, 6])
        .style(ui::flat_button)
        .on_press_maybe(message)
}

fn icon_button<'a>(glyph: &'a str, tip: &'a str, message: Option<Message>) -> Element<'a, Message> {
    let b = button(text(glyph).size(14).center())
        .padding([2, 6])
        .style(ui::flat_button)
        .on_press_maybe(message);
    tooltip(
        b,
        container(text(tip).size(13))
            .padding([4, 8])
            .style(ui::tooltip_box),
        tooltip::Position::Bottom,
    )
    .into()
}

fn bar_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color)),
        ..container::Style::default()
    }
}

impl FileDiff {
    pub(super) fn main_view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let n = self.panes.len();
        let maps_width = if self.show_overview_map && n > 0 {
            self.map_style.width() * n as f32 + 2.0
        } else {
            0.0
        };

        // Row 0: file headers.
        let mut headers = row![].align_y(Alignment::Center);
        for i in 0..n {
            if i > 0 {
                headers = headers.push(Space::new().width(BETWEEN));
            }
            headers = headers.push(container(self.file_header(i)).width(Length::FillPortion(1)));
        }
        if maps_width > 0.0 {
            headers = headers.push(Space::new().width(maps_width));
        }
        let headers = container(headers)
            .height(HEADER_ROW_HEIGHT)
            .align_y(Alignment::Center)
            .style(bar_style);

        // Row 1: the panes, each with its message area above it
        // (`msgarea_mgr` in each pane's box).
        let lh = self.metrics.line_height;
        let mut body = row![].height(Length::Fill);
        for i in 0..n {
            if i > 0 {
                body = body.push(self.gutter(i - 1, i, Direction::Right));
                body = body.push(self.link(i - 1));
                body = body.push(self.gutter(i, i - 1, Direction::Left));
            }
            let mut cell = column![];
            if let Some(b) = &self.panes[i].banner {
                let banner = ui::pane_banner(
                    b.kind,
                    b.primary.as_str(),
                    b.secondary.clone(),
                    b.actions
                        .iter()
                        .map(|(label, a)| (label.clone(), Message::Banner(i, *a)))
                        .collect(),
                );
                cell = cell.push(if self.embedded {
                    // A fixed height keeps `pane_geometry` exact for the parent.
                    container(banner).height(EMBEDDED_BANNER_HEIGHT).clip(true)
                } else {
                    container(banner)
                });
            }
            cell = cell.push(self.pane_view(i));
            body = body.push(container(cell).width(Length::FillPortion(1)));
        }
        if maps_width > 0.0 {
            let max_rows = self
                .panes
                .iter()
                .map(|p| p.wrap.total_rows(p.buffer.display_line_count()))
                .max()
                .unwrap_or(1);
            let mut maps = row![].spacing(0).padding([0, 1]);
            for i in 0..n {
                let p = &self.panes[i];
                let chunks = self.derived.maps.get(i).map(Vec::as_slice).unwrap_or(&[]);
                maps = maps.push(
                    chunk_map(chunks, &self.styles[i])
                        .lines(p.buffer.display_line_count(), max_rows)
                        .wrap(Some(&p.wrap))
                        .viewport(p.scroll.y, p.viewport.height, lh)
                        .overscroll_lines(OVERSCROLL_LINES)
                        .map_style(self.map_style)
                        .source(&p.buffer)
                        .width(self.map_style.width())
                        .on_scroll(move |s| Message::MapScroll(i, s)),
                );
            }
            body = body.push(maps);
        }

        let body: Element<'a, Message> = if self.cmp.is_none() && self.is_loading() {
            let label = if self
                .panes
                .iter()
                .any(|p| p.file.state == LoadState::Loading)
            {
                tr!("Loading files…")
            } else {
                tr!("Computing differences… (Escape to stop)")
            };
            stack![
                body,
                container(
                    container(text(format!("{} {label}", SPINNER[self.spinner % 4])).size(15))
                        .padding([10, 16])
                        .style(ui::popover)
                )
                .center(Length::Fill)
            ]
            .into()
        } else {
            body.into()
        };

        let mut content = column![headers, ui::separator(false)];
        content = content.push(body);
        if self.find.visible {
            content = content.push(ui::separator(false));
            content = content.push(self.find_bar());
        }
        content = content.push(ui::separator(false));
        content = content.push(self.status_row(maps_width));

        let base: Element<'a, Message> = content.into();
        let base = self.with_popup(base, shared);
        self.with_dialog(base, shared)
    }

    fn display_label(&self, pane: usize) -> (String, String) {
        let file = &self.panes[pane].file;
        if let Some(label) = file.custom_label.as_ref().filter(|l| !l.is_empty()) {
            let full = file
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| label.clone());
            return (label.clone(), full);
        }
        let Some(path) = file.target() else {
            return (tr!("Unnamed file").into(), String::new());
        };
        let targets: Vec<Option<&std::path::PathBuf>> =
            self.panes.iter().map(|p| p.file.target()).collect();
        let parent = meld_files::find_shared_parent_path(&targets);
        let shown = parent
            .and_then(|parent| meld_files::format_parent_relative_path(&parent, path).ok())
            .unwrap_or_else(|| meld_files::format_home_relative_path(path));
        (shown, path.display().to_string())
    }

    fn file_header(&self, i: usize) -> Element<'_, Message> {
        let p = &self.panes[i];
        let modified = self.is_modified(i);
        let (label, full) = self.display_label(i);
        let mut r = row![].spacing(2).align_y(Alignment::Center).padding([0, 4]);
        if !p.file.writable && p.file.path.is_some() {
            let (glyph, tip) = if p.editable {
                (
                    "🔓",
                    tr!("Changes will not be saved to this file; click to prevent editing"),
                )
            } else {
                (
                    "🔒",
                    tr!(
                        "This file can not be written to. You may click here to unlock this file and make changes anyway, but these changes must be saved to a new file."
                    ),
                )
            };
            r = r.push(icon_button(glyph, tip, Some(act(Act::ToggleReadOnly(i)))));
        }
        let save_tip = if p.file.writable {
            tr!("Save")
        } else {
            tr!("Save As…")
        };
        r = r.push(icon_button(
            icon::SAVE,
            save_tip,
            modified.then_some(act(Act::SavePane(i))),
        ));
        r = r.push(icon_button(
            icon::FOLDER,
            tr!("Open file in this pane"),
            Some(act(Act::OpenFile(i))),
        ));
        let label_text = text(label).size(13).wrapping(text::Wrapping::None);
        let label_el: Element<'_, Message> = if full.is_empty() {
            label_text.style(ui::dim_text).into()
        } else {
            tooltip(
                label_text,
                container(text(full).size(13))
                    .padding([4, 8])
                    .style(ui::tooltip_box),
                tooltip::Position::Bottom,
            )
            .into()
        };
        r = r.push(container(label_el).width(Length::Fill).clip(true));
        if modified {
            r = r.push(text(icon::MODIFIED).size(10));
        }
        r.into()
    }

    fn pane_view(&self, i: usize) -> Element<'_, Message> {
        let p = &self.panes[i];
        let highlights = if p.hover.is_empty() {
            self.derived
                .highlights
                .get(i)
                .map(Vec::as_slice)
                .unwrap_or(&[])
        } else {
            p.hover.as_slice()
        };
        let mut pane = text_pane(&p.buffer, &self.styles[i])
            .id(p.id.clone())
            .font(self.font)
            .metrics(self.metrics)
            .scroll(p.scroll)
            .highlights(highlights)
            .inline_highlights(&p.inline)
            .sync_points(&p.sync_lines)
            .show_line_numbers(self.view_options.show_line_numbers)
            .show_whitespace(self.view_options.show_whitespace)
            .highlight_current_line(self.view_options.highlight_current_line)
            .overscroll_lines(OVERSCROLL_LINES)
            .wrap(&p.wrap)
            .dimmed(&p.dimmed)
            .search_matches(&p.matches)
            .flashes(&p.flashes)
            .on_action(move |a| Message::Pane(i, a));
        if self.highlight_syntax {
            pane = pane.syntax(&p.highlighter);
        }
        pane.into()
    }

    fn gutter(&self, from: usize, to: usize, direction: Direction) -> Element<'_, Message> {
        let lh = self.metrics.line_height;
        let style = &self.styles[from];
        match self.derived.pair(from, to) {
            Some(pair) => action_gutter(&pair.chunks, style)
                .scroll(self.panes[from].scroll.y)
                .wrap(Some(&self.panes[from].wrap))
                .pane_height(self.panes[from].viewport.height)
                .line_height(lh)
                .editable(self.editable_now(from), self.editable_now(to))
                .direction(direction)
                .current(pair.position(self.cursor.chunk))
                .width(GUTTER_WIDTH)
                .on_action(move |index, action| Message::Gutter(from, to, index, action))
                .into(),
            None => action_gutter(&[], style).width(GUTTER_WIDTH).into(),
        }
    }

    fn link(&self, left: usize) -> Element<'_, Message> {
        let lh = self.metrics.line_height;
        let right = left + 1;
        match self.derived.pair(left, right) {
            Some(pair) => link_map(&pair.chunks, &self.link_style)
                .scroll(self.panes[left].scroll.y, self.panes[right].scroll.y)
                .wrap(Some(&self.panes[left].wrap), Some(&self.panes[right].wrap))
                .pane_heights(
                    self.panes[left].viewport.height,
                    self.panes[right].viewport.height,
                )
                .on_scroll(Message::LinkScroll)
                .line_height(lh)
                .line_counts(
                    self.panes[left].buffer.display_line_count(),
                    self.panes[right].buffer.display_line_count(),
                )
                .current(pair.position(self.cursor.chunk))
                .width(LINK_WIDTH)
                .into(),
            None => link_map(&[], &self.link_style)
                .on_scroll(Message::LinkScroll)
                .width(LINK_WIDTH)
                .into(),
        }
    }

    fn find_bar(&self) -> Element<'_, Message> {
        let f = &self.find;
        let error = (f.no_match || f.invalid) && !f.text.is_empty();
        let entry = text_input(tr!("Find"), &f.text)
            .id(self.find_id.clone())
            .size(14)
            .padding([4, 8])
            .width(260)
            .on_input(|t| Message::Find(FindMessage::Text(t)))
            .on_submit(Message::Find(FindMessage::Next))
            .style(move |theme: &Theme, status| {
                let mut style = text_input::default(theme, status);
                if error {
                    let danger = theme.extended_palette().danger.base.color;
                    style.border = style.border.color(danger).width(1.5);
                    style.background = Background::Color(danger.scale_alpha(0.15));
                }
                style
            });
        let mut first = row![
            entry,
            icon_button(
                icon::UP,
                tr!("Previous Result"),
                Some(Message::Find(FindMessage::Previous))
            ),
            icon_button(
                icon::DOWN,
                tr!("Next Result"),
                Some(Message::Find(FindMessage::Next))
            ),
            checkbox(f.match_case)
                .label(tr!("Match case"))
                .text_size(13)
                .on_toggle(|b| Message::Find(FindMessage::MatchCase(b))),
            checkbox(f.whole_word)
                .label(tr!("Whole word"))
                .text_size(13)
                .on_toggle(|b| Message::Find(FindMessage::WholeWord(b))),
            checkbox(f.regex)
                .label(tr!("Regular expression"))
                .text_size(13)
                .on_toggle(|b| Message::Find(FindMessage::Regex(b))),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if f.wrapped {
            first = first.push(
                text(format!("↻ {}", tr!("Wrapped")))
                    .size(13)
                    .style(ui::dim_text),
            );
        }
        first = first.push(space::horizontal());
        first = first.push(icon_button(
            "⇄",
            tr!("Switch between Find and Find-and-Replace"),
            Some(Message::Find(FindMessage::ToggleReplace)),
        ));
        first = first.push(icon_button(
            icon::CLOSE,
            tr!("Close"),
            Some(Message::Find(FindMessage::Close)),
        ));
        let mut col = column![first].spacing(6);
        if f.replace_mode {
            col = col.push(
                row![
                    text_input(tr!("Replace"), &f.replacement)
                        .size(14)
                        .padding([4, 8])
                        .width(260)
                        .on_input(|t| Message::Find(FindMessage::Replacement(t)))
                        .on_submit(Message::Find(FindMessage::Replace)),
                    button(text(tr!("Replace")).size(13))
                        .padding([4, 10])
                        .on_press(Message::Find(FindMessage::Replace)),
                    button(text(tr!("Replace All")).size(13))
                        .padding([4, 10])
                        .on_press(Message::Find(FindMessage::ReplaceAll)),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }
        container(col).padding([6, 10]).style(bar_style).into()
    }

    fn status_row(&self, maps_width: f32) -> Element<'_, Message> {
        let n = self.panes.len();
        let mut r = row![].align_y(Alignment::Center);
        for i in 0..n {
            if i > 0 {
                r = r.push(Space::new().width(BETWEEN));
            }
            r = r.push(
                container(iced::widget::responsive(move |size| {
                    self.status_bar(i, size.width)
                }))
                .height(24)
                .width(Length::FillPortion(1))
                .clip(true),
            );
        }
        if maps_width > 0.0 {
            r = r.push(Space::new().width(maps_width));
        }
        container(r).padding([1, 0]).style(bar_style).into()
    }

    /// `MeldStatusBar`. Narrow panes (three-way comparisons) hide the
    /// less used items first; the Display menu always offers them.
    fn status_bar(&self, i: usize, width: f32) -> Element<'_, Message> {
        let p = &self.panes[i];
        let cursor = p.buffer.cursor();
        let column = p.buffer.display_column(cursor);
        let shown = status_items(width);
        let mut r = row![space::horizontal()]
            .spacing(4)
            .align_y(Alignment::Center)
            .padding([0, 6]);
        r = r.push(small_button(
            if shown.display_label {
                format!("{} ▾", tr!("Display"))
            } else {
                "☰".to_string()
            },
            Some(act(Act::Display(i))),
        ));
        if shown.encoding {
            r = r.push(small_button(
                encoding_label(p),
                Some(act(Act::ChooseEncoding(i))),
            ));
        }
        if shown.newline {
            r = r.push(newline_picker(i, p.file.newline));
        }
        if shown.language {
            r = r.push(small_button(
                language_label(p),
                Some(act(Act::ChooseLanguage(i))),
            ));
        }
        if let Some((pane, line)) = &self.go_to
            && *pane == i
        {
            r = r.push(
                text_input(tr!("Line"), line)
                    .id(self.go_to_id.clone())
                    .size(12)
                    .padding([2, 6])
                    .width(80)
                    .on_input(Message::GoToLine)
                    .on_submit(Message::GoToLineDone),
            );
        } else {
            let label = if shown.long_position {
                tr!(
                    "Ln {line}, Col {column}",
                    line = cursor.line + 1,
                    column = column + 1
                )
            } else {
                format!("{}:{}", cursor.line + 1, column + 1)
            };
            r = r.push(small_button(
                label,
                (self.focus_pane() == Some(i)).then(|| act(Act::GoToLine)),
            ));
        }
        if shown.overwrite {
            r = r.push(
                text(if self.overwrite {
                    tr!("OVR")
                } else {
                    tr!("INS")
                })
                .size(12)
                .wrapping(text::Wrapping::None)
                .style(ui::dim_text),
            );
        }
        r.into()
    }

    pub(super) fn toolbar_view<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        let s = self.current_sensitivity();
        let mut r = row![].spacing(2).align_y(Alignment::Center);
        if self.is_loading() {
            r = r.push(text(SPINNER[self.spinner % 4]).size(16));
        }
        r = r.push(ui::tool_button(
            icon::UP,
            ui::with_shortcut(tr!("Go to the previous change"), "Alt+Up"),
            self.cursor.prev.map(|_| act(Act::PrevChange)),
        ));
        r = r.push(ui::tool_button(
            icon::DOWN,
            ui::with_shortcut(tr!("Go to the next change"), "Alt+Down"),
            self.cursor.next.map(|_| act(Act::NextChange)),
        ));
        if self.panes.len() == 3 {
            r = r.push(ui::tool_button(
                icon::CONFLICT_PREV,
                ui::with_shortcut(tr!("Go to the previous conflict"), "Ctrl+J"),
                self.cursor.prev_conflict.map(|_| act(Act::PrevConflict)),
            ));
            r = r.push(ui::tool_button(
                icon::CONFLICT_NEXT,
                ui::with_shortcut(tr!("Go to the next conflict"), "Ctrl+K"),
                self.cursor.next_conflict.map(|_| act(Act::NextConflict)),
            ));
        }
        if self.panes.len() > 1 {
            r = r.push(Space::new().width(8));
            r = r.push(ui::tool_button(
                icon::LEFT,
                ui::with_shortcut(tr!("Push current change to the left"), "Alt+Left"),
                s.push_left.then_some(act(Act::PushLeft)),
            ));
            r = r.push(ui::tool_button(
                icon::RIGHT,
                ui::with_shortcut(tr!("Push current change to the right"), "Alt+Right"),
                s.push_right.then_some(act(Act::PushRight)),
            ));
            r = r.push(ui::tool_button(
                "⎘",
                tr!("Copy chunks"),
                (s.copy_left || s.copy_right).then_some(act(Act::Popup(Some(PopupKind::CopyMenu)))),
            ));
            r = r.push(ui::tool_button(
                "⌦",
                ui::with_shortcut(tr!("Delete change"), "Alt+Delete"),
                s.delete.then_some(act(Act::DeleteChange)),
            ));
        }
        r = r.push(Space::new().width(8));
        r = r.push(
            button(text(format!("{} ▾", tr!("Text Filters"))).size(13))
                .padding([4, 8])
                .style(ui::flat_button)
                .on_press(act(Act::Popup(Some(PopupKind::Filters)))),
        );
        r.into()
    }

    pub(super) fn menu(&self, _shared: &Shared) -> Vec<menu::Item<Message>> {
        use menu::Item;
        let action =
            |label: &str, shortcut: Option<&'static str>, enabled: bool, a: Act| Item::Action {
                label: label.to_string(),
                shortcut,
                message: enabled.then_some(act(a)),
            };
        let n = self.panes.len();
        let focus = self.focus_pane();
        let modified_focus = focus.is_some_and(|p| self.is_modified(p));
        let any_modified = (0..n).any(|i| self.is_modified(i));
        let (merge_left, merge_right, merge_all) = self.merge_sensitivity();
        let _ = modified_focus;
        let mut items = vec![
            action(
                tr!("Save As…"),
                Some("Ctrl+Shift+S"),
                focus.is_some(),
                Act::SaveAs,
            ),
            action(
                tr!("Save All"),
                Some("Ctrl+Shift+L"),
                any_modified,
                Act::SaveAll,
            ),
            action(tr!("Revert Files…"), None, true, Act::Revert),
            action(
                tr!("Open Externally"),
                Some("Ctrl+Shift+O"),
                focus.is_some_and(|p| self.panes[p].file.path.is_some()),
                Act::OpenExternal,
            ),
            Item::Separator,
            action(
                tr!("Refresh Comparison"),
                Some("Ctrl+R"),
                self.cmp.is_some(),
                Act::Refresh,
            ),
            action(tr!("Find…"), Some("Ctrl+F"), true, Act::Find),
            Item::Separator,
            Item::toggle(
                tr!("Overview Map"),
                self.show_overview_map,
                act(Act::ToggleOverview),
            ),
            Item::toggle(
                tr!("Lock Scrolling"),
                self.lock_scrolling,
                act(Act::ToggleLockScrolling),
            ),
        ];
        if n == 2 {
            items.push(action(
                tr!("Swap Left and Right Panes"),
                Some("Alt+\\"),
                true,
                Act::Swap,
            ));
        }
        if n > 1 {
            items.push(Item::Separator);
            items.push(action(
                tr!("Merge All from Left"),
                None,
                merge_left,
                Act::MergeAllLeft,
            ));
            items.push(action(
                tr!("Merge All from Right"),
                None,
                merge_right,
                Act::MergeAllRight,
            ));
            if n == 3 {
                items.push(action(tr!("Merge All"), None, merge_all, Act::MergeAll));
            }
            items.push(action(
                tr!("Format as Patch…"),
                None,
                true,
                Act::FormatPatch,
            ));
        }
        items
    }
}
