//! Drawing the comparison (`filediff.ui`): file headers, message areas,
//! panes with action gutters and link maps, overview maps, the find bar
//! and status bars; the header-bar toolbar and the main menu section.

use std::fmt;

use iced::widget::{
    Space, button, checkbox, column, container, pick_list, row, space, stack, text, text_input, tooltip,
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
const MAP_WIDTH: f32 = 20.0;
/// Width between two panes: gutter, link map, gutter.
const BETWEEN: f32 = GUTTER_WIDTH * 2.0 + LINK_WIDTH;

/// Height of the file header row above the panes.
pub const HEADER_ROW_HEIGHT: f32 = 30.0;
/// Height of the message area row of embedded comparisons.
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

fn small_button<'a>(label: impl text::IntoFragment<'a>, message: Option<Message>) -> button::Button<'a, Message> {
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
        container(text(tip).size(13)).padding([4, 8]).style(ui::tooltip_box),
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
            MAP_WIDTH * n as f32 + 2.0
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

        // Message areas.
        let banners = if self.panes.iter().any(|p| p.banner.is_some()) {
            let mut r = row![];
            for i in 0..n {
                if i > 0 {
                    r = r.push(Space::new().width(BETWEEN));
                }
                let cell: Element<'a, Message> = match &self.panes[i].banner {
                    Some(b) => ui::banner(
                        b.kind,
                        b.primary.as_str(),
                        b.secondary.clone(),
                        b.actions
                            .iter()
                            .map(|(label, a)| (label.clone(), Message::Banner(i, *a)))
                            .collect(),
                    ),
                    None => Space::new().into(),
                };
                r = r.push(container(cell).width(Length::FillPortion(1)));
            }
            if maps_width > 0.0 {
                r = r.push(Space::new().width(maps_width));
            }
            Some(r)
        } else {
            None
        };

        // Row 1: the panes.
        let lh = self.metrics.line_height;
        let mut body = row![].height(Length::Fill);
        for i in 0..n {
            if i > 0 {
                body = body.push(self.gutter(i - 1, i, Direction::Right));
                body = body.push(self.link(i - 1));
                body = body.push(self.gutter(i, i - 1, Direction::Left));
            }
            body = body.push(container(self.pane_view(i)).width(Length::FillPortion(1)));
        }
        if maps_width > 0.0 {
            let max_lines = self
                .panes
                .iter()
                .map(|p| p.buffer.display_line_count())
                .max()
                .unwrap_or(1);
            let mut maps = row![].spacing(0).padding([0, 1]);
            for i in 0..n {
                let p = &self.panes[i];
                let chunks = self.derived.maps.get(i).map(Vec::as_slice).unwrap_or(&[]);
                maps = maps.push(
                    chunk_map(chunks, &self.styles[i])
                        .lines(p.buffer.display_line_count(), max_lines)
                        .viewport(p.scroll.y, p.viewport.height, lh)
                        .overscroll_lines(OVERSCROLL_LINES)
                        .width(MAP_WIDTH)
                        .on_scroll(move |s| Message::MapScroll(i, s)),
                );
            }
            body = body.push(maps);
        }

        let body: Element<'a, Message> = if self.cmp.is_none() && self.is_loading() {
            let label = if self.panes.iter().any(|p| p.file.state == LoadState::Loading) {
                "Loading files…"
            } else {
                "Computing differences… (Escape to stop)"
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
        if let Some(b) = banners {
            if self.embedded {
                // A fixed height keeps `header_height` exact for the parent.
                content = content.push(container(b).height(EMBEDDED_BANNER_HEIGHT).clip(true));
            } else {
                content = content.push(b);
            }
        }
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
            return ("Unnamed file".into(), String::new());
        };
        let targets: Vec<Option<&std::path::PathBuf>> = self.panes.iter().map(|p| p.file.target()).collect();
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
                ("🔓", "Changes will not be saved to this file; click to prevent editing")
            } else {
                ("🔒", "This file is read-only; click to allow editing")
            };
            r = r.push(icon_button(glyph, tip, Some(act(Act::ToggleReadOnly(i)))));
        }
        let save_tip = if p.file.writable { "Save" } else { "Save As…" };
        r = r.push(icon_button(
            icon::SAVE,
            save_tip,
            modified.then_some(act(Act::SavePane(i))),
        ));
        r = r.push(icon_button(
            icon::FOLDER,
            "Open a file in this pane",
            Some(act(Act::OpenFile(i))),
        ));
        let label_text = text(label).size(13).wrapping(text::Wrapping::None);
        let label_el: Element<'_, Message> = if full.is_empty() {
            label_text.style(ui::dim_text).into()
        } else {
            tooltip(
                label_text,
                container(text(full).size(13)).padding([4, 8]).style(ui::tooltip_box),
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
            self.derived.highlights.get(i).map(Vec::as_slice).unwrap_or(&[])
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
            Some(pair) => link_map(&pair.chunks, &self.neutral_style)
                .scroll(self.panes[left].scroll.y, self.panes[right].scroll.y)
                .line_height(lh)
                .line_counts(
                    self.panes[left].buffer.display_line_count(),
                    self.panes[right].buffer.display_line_count(),
                )
                .current(pair.position(self.cursor.chunk))
                .width(LINK_WIDTH)
                .into(),
            None => link_map(&[], &self.neutral_style).width(LINK_WIDTH).into(),
        }
    }

    fn find_bar(&self) -> Element<'_, Message> {
        let f = &self.find;
        let error = (f.no_match || f.invalid) && !f.text.is_empty();
        let entry = text_input("Find", &f.text)
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
            icon_button(icon::UP, "Previous Result", Some(Message::Find(FindMessage::Previous))),
            icon_button(icon::DOWN, "Next Result", Some(Message::Find(FindMessage::Next))),
            checkbox(f.match_case)
                .label("Match case")
                .text_size(13)
                .on_toggle(|b| Message::Find(FindMessage::MatchCase(b))),
            checkbox(f.whole_word)
                .label("Whole word")
                .text_size(13)
                .on_toggle(|b| Message::Find(FindMessage::WholeWord(b))),
            checkbox(f.regex)
                .label("Regular expression")
                .text_size(13)
                .on_toggle(|b| Message::Find(FindMessage::Regex(b))),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if f.wrapped {
            first = first.push(text("↻ Wrapped").size(13).style(ui::dim_text));
        }
        first = first.push(space::horizontal());
        first = first.push(icon_button(
            "⇄",
            "Switch between Find and Find-and-Replace",
            Some(Message::Find(FindMessage::ToggleReplace)),
        ));
        first = first.push(icon_button(
            icon::CLOSE,
            "Close",
            Some(Message::Find(FindMessage::Close)),
        ));
        let mut col = column![first].spacing(6);
        if f.replace_mode {
            col = col.push(
                row![
                    text_input("Replace", &f.replacement)
                        .size(14)
                        .padding([4, 8])
                        .width(260)
                        .on_input(|t| Message::Find(FindMessage::Replacement(t)))
                        .on_submit(Message::Find(FindMessage::Replace)),
                    button(text("Replace").size(13))
                        .padding([4, 10])
                        .on_press(Message::Find(FindMessage::Replace)),
                    button(text("Replace All").size(13))
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
                container(iced::widget::responsive(move |size| self.status_bar(i, size.width)))
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

    /// `MeldStatusBar`
    fn status_bar(&self, i: usize, width: f32) -> Element<'_, Message> {
        let p = &self.panes[i];
        let cursor = p.buffer.cursor();
        let column = p.buffer.display_column(cursor);
        let mut r = row![space::horizontal()]
            .spacing(4)
            .align_y(Alignment::Center)
            .padding([0, 6]);
        if width > 420.0 {
            r = r.push(small_button("Display ▾", Some(act(Act::Display))));
        }
        let encoding = if p.file.encoding.bom {
            format!("{} (BOM)", p.file.encoding.name())
        } else {
            p.file.encoding.name().to_string()
        };
        if width > 300.0 {
            r = r.push(small_button(encoding, Some(act(Act::ChooseEncoding(i)))));
        }
        r = r.push(
            pick_list(
                NEWLINES,
                Some(NewlineChoice(p.file.newline)),
                move |c: NewlineChoice| act(Act::SetNewline(i, c.0)),
            )
            .text_size(12)
            .padding([2, 6])
            .style(|theme: &Theme, status| {
                let mut s = pick_list::default(theme, status);
                s.background = Background::Color(iced::Color::TRANSPARENT);
                s.border = s.border.width(0);
                s
            }),
        );
        let language = p
            .language
            .and_then(|id| meld_files::LANGUAGES.iter().find(|l| l.id == id))
            .map(|l| l.name)
            .unwrap_or("Plain Text");
        r = r.push(small_button(language, Some(act(Act::ChooseLanguage(i)))));
        if let Some((pane, line)) = &self.go_to
            && *pane == i
        {
            r = r.push(
                text_input("Line", line)
                    .id(self.go_to_id.clone())
                    .size(12)
                    .padding([2, 6])
                    .width(80)
                    .on_input(Message::GoToLine)
                    .on_submit(Message::GoToLineDone),
            );
        } else {
            r = r.push(small_button(
                format!("Ln {}, Col {}", cursor.line + 1, column + 1),
                (self.focus_pane() == Some(i)).then(|| act(Act::GoToLine)),
            ));
        }
        if width > 360.0 {
            r = r.push(
                text(if self.overwrite { "OVR" } else { "INS" })
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
            "Previous change (Alt+Up)",
            self.cursor.prev.map(|_| act(Act::PrevChange)),
        ));
        r = r.push(ui::tool_button(
            icon::DOWN,
            "Next change (Alt+Down)",
            self.cursor.next.map(|_| act(Act::NextChange)),
        ));
        if self.panes.len() == 3 {
            r = r.push(ui::tool_button(
                icon::CONFLICT_PREV,
                "Previous conflict (Ctrl+J)",
                self.cursor.prev_conflict.map(|_| act(Act::PrevConflict)),
            ));
            r = r.push(ui::tool_button(
                icon::CONFLICT_NEXT,
                "Next conflict (Ctrl+K)",
                self.cursor.next_conflict.map(|_| act(Act::NextConflict)),
            ));
        }
        if self.panes.len() > 1 {
            r = r.push(Space::new().width(8));
            r = r.push(ui::tool_button(
                icon::LEFT,
                "Push current change to the left (Alt+Left)",
                s.push_left.then_some(act(Act::PushLeft)),
            ));
            r = r.push(ui::tool_button(
                icon::RIGHT,
                "Push current change to the right (Alt+Right)",
                s.push_right.then_some(act(Act::PushRight)),
            ));
            r = r.push(ui::tool_button(
                "⎘",
                "Copy chunks",
                (s.copy_left || s.copy_right).then_some(act(Act::Popup(Some(PopupKind::CopyMenu)))),
            ));
            r = r.push(ui::tool_button(
                "⌦",
                "Delete change (Alt+Delete)",
                s.delete.then_some(act(Act::DeleteChange)),
            ));
        }
        r = r.push(Space::new().width(8));
        r = r.push(
            button(text("Text Filters ▾").size(13))
                .padding([4, 8])
                .style(ui::flat_button)
                .on_press(act(Act::Popup(Some(PopupKind::Filters)))),
        );
        r.into()
    }

    pub(super) fn menu(&self, _shared: &Shared) -> Vec<menu::Item<Message>> {
        use menu::Item;
        let action = |label: &str, shortcut: Option<&'static str>, enabled: bool, a: Act| Item::Action {
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
            action("Save As…", Some("Ctrl+Shift+S"), focus.is_some(), Act::SaveAs),
            action("Save All", Some("Ctrl+Shift+L"), any_modified, Act::SaveAll),
            action("Revert Files…", None, true, Act::Revert),
            action(
                "Open Externally",
                Some("Ctrl+Shift+O"),
                focus.is_some_and(|p| self.panes[p].file.path.is_some()),
                Act::OpenExternal,
            ),
            Item::Separator,
            action("Refresh Comparison", Some("Ctrl+R"), self.cmp.is_some(), Act::Refresh),
            action("Find…", Some("Ctrl+F"), true, Act::Find),
            Item::Separator,
            Item::toggle("Overview Map", self.show_overview_map, act(Act::ToggleOverview)),
            Item::toggle("Lock Scrolling", self.lock_scrolling, act(Act::ToggleLockScrolling)),
        ];
        if n == 2 {
            items.push(action("Swap Panes", Some("Alt+\\"), true, Act::Swap));
        }
        if n > 1 {
            items.push(Item::Separator);
            items.push(action("Merge All from Left", None, merge_left, Act::MergeAllLeft));
            items.push(action("Merge All from Right", None, merge_right, Act::MergeAllRight));
            if n == 3 {
                items.push(action("Merge All", None, merge_all, Act::MergeAll));
            }
            items.push(action("Format as Patch…", None, true, Act::FormatPatch));
        }
        items
    }
}
