//! The tab's dialogs and popup menus: save before closing, discard
//! changes, mark resolved, errors, Format as Patch, the encoding and
//! language selectors (`bufferselectors.py`), display options, the copy
//! and text filter menus and the pane context menu.

use iced::widget::{
    Space, button, checkbox, column, container, mouse_area, opaque, row, scrollable, space, stack, text,
    text_input,
};
use iced::{Element, Length, Padding};
use meld_diff::SyncpointAction;

use super::{Act, Dialog, DialogMessage, FileDiff, Message, Popup};
use crate::shared::Shared;
use crate::ui::{self, menu, modal};

/// Encodings offered by the encoding selector.
const ENCODINGS: &[&str] = &[
    "UTF-8",
    "UTF-16LE",
    "UTF-16BE",
    "ISO-8859-1",
    "ISO-8859-2",
    "ISO-8859-3",
    "ISO-8859-4",
    "ISO-8859-5",
    "ISO-8859-6",
    "ISO-8859-7",
    "ISO-8859-8",
    "ISO-8859-10",
    "ISO-8859-13",
    "ISO-8859-14",
    "ISO-8859-15",
    "ISO-8859-16",
    "windows-1250",
    "windows-1251",
    "windows-1252",
    "windows-1253",
    "windows-1254",
    "windows-1255",
    "windows-1256",
    "windows-1257",
    "windows-1258",
    "windows-874",
    "IBM866",
    "KOI8-R",
    "KOI8-U",
    "macintosh",
    "x-mac-cyrillic",
    "Shift_JIS",
    "EUC-JP",
    "ISO-2022-JP",
    "EUC-KR",
    "GBK",
    "gb18030",
    "Big5",
];

fn dialog_message(m: DialogMessage) -> Message {
    Message::Dialog(m)
}

fn menu_view<'a>(items: Vec<menu::Item<Message>>) -> Element<'a, Message> {
    let mut list = column![].width(300);
    for item in items {
        let el: Element<'a, Message> = match item {
            menu::Item::Action {
                label,
                shortcut,
                message,
            } => {
                let mut r = row![text(label).size(14).width(Length::Fill)].spacing(12);
                if let Some(s) = shortcut {
                    r = r.push(text(s).size(12).style(ui::dim_text));
                }
                button(r)
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(ui::flat_button)
                    .on_press_maybe(message)
                    .into()
            }
            menu::Item::Toggle {
                label,
                checked,
                message,
            } => button(
                row![
                    text(if checked { "✓" } else { " " }).size(14).width(18),
                    text(label).size(14)
                ]
                .spacing(4),
            )
            .width(Length::Fill)
            .padding([6, 10])
            .style(ui::flat_button)
            .on_press(message)
            .into(),
            menu::Item::Separator => container(ui::separator(false)).padding([4, 0]).into(),
            menu::Item::Heading(label) => container(text(label).size(12).style(ui::dim_text))
                .padding([6, 10])
                .into(),
        };
        list = list.push(el);
    }
    container(list).padding(6).style(ui::popover).into()
}

impl FileDiff {
    pub(super) fn with_popup<'a>(&'a self, base: Element<'a, Message>, _shared: &'a Shared) -> Element<'a, Message> {
        let Some(popup) = self.popup else {
            return base;
        };
        let s = self.current_sensitivity();
        let act = |a: Act| Some(Message::Act(a));
        let (items, positioned): (Vec<menu::Item<Message>>, bool) = match popup {
            Popup::CopyMenu => (
                vec![
                    menu::Item::Action {
                        label: "Copy change above the left chunk".into(),
                        shortcut: Some("Alt+["),
                        message: act(Act::CopyLeftUp).filter(|_| s.copy_left),
                    },
                    menu::Item::Action {
                        label: "Copy change below the left chunk".into(),
                        shortcut: Some("Alt+;"),
                        message: act(Act::CopyLeftDown).filter(|_| s.copy_left),
                    },
                    menu::Item::Action {
                        label: "Copy change above the right chunk".into(),
                        shortcut: Some("Alt+]"),
                        message: act(Act::CopyRightUp).filter(|_| s.copy_right),
                    },
                    menu::Item::Action {
                        label: "Copy change below the right chunk".into(),
                        shortcut: Some("Alt+'"),
                        message: act(Act::CopyRightDown).filter(|_| s.copy_right),
                    },
                ],
                false,
            ),
            Popup::Filters => {
                let mut items: Vec<menu::Item<Message>> = self
                    .text_filters
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        if f.regex.is_some() {
                            menu::Item::toggle(f.name.clone(), f.active, Message::Act(Act::ToggleTextFilter(i)))
                        } else {
                            menu::Item::disabled(format!("{} (invalid)", f.name))
                        }
                    })
                    .collect();
                if items.is_empty() {
                    items.push(menu::Item::disabled("No text filters"));
                }
                items.push(menu::Item::Separator);
                items.push(menu::Item::toggle(
                    "Ignore blank lines",
                    self.ignore_blank_lines,
                    Message::Act(Act::ToggleIgnoreBlank),
                ));
                (items, false)
            }
            Popup::Context { pane, .. } => {
                let p = &self.panes[pane];
                let editable = self.editable_now(pane);
                let has_selection = p.buffer.selection().is_some();
                let mut items = vec![
                    menu::Item::Action {
                        label: "Undo".into(),
                        shortcut: Some("Ctrl+Z"),
                        message: act(Act::Undo).filter(|_| self.undo.can_undo()),
                    },
                    menu::Item::Action {
                        label: "Redo".into(),
                        shortcut: Some("Ctrl+Shift+Z"),
                        message: act(Act::Redo).filter(|_| self.undo.can_redo()),
                    },
                    menu::Item::Separator,
                    menu::Item::Action {
                        label: "Cut".into(),
                        shortcut: Some("Ctrl+X"),
                        message: act(Act::Cut).filter(|_| has_selection && editable),
                    },
                    menu::Item::Action {
                        label: "Copy".into(),
                        shortcut: Some("Ctrl+C"),
                        message: act(Act::Copy).filter(|_| has_selection),
                    },
                    menu::Item::Action {
                        label: "Paste".into(),
                        shortcut: Some("Ctrl+V"),
                        message: act(Act::Paste).filter(|_| editable),
                    },
                    menu::Item::Separator,
                    menu::Item::Action {
                        label: "Open Containing Folder".into(),
                        shortcut: None,
                        message: act(Act::OpenFolder).filter(|_| p.file.path.is_some()),
                    },
                    menu::Item::Action {
                        label: "Copy Full Path".into(),
                        shortcut: None,
                        message: act(Act::CopyFullPath).filter(|_| p.file.path.is_some()),
                    },
                ];
                if self.panes.len() > 1 {
                    items.push(menu::Item::Separator);
                    if let Some((_, action)) = self.syncpoint_action()
                        && action != SyncpointAction::Disabled
                    {
                        let label = match action {
                            SyncpointAction::Move => "Move Synchronization Point",
                            SyncpointAction::Delete => "Remove Synchronization Point",
                            SyncpointAction::Match => "Match Synchronization Point",
                            _ => "Add Synchronization Point",
                        };
                        items.push(menu::Item::action(label, None, Message::Act(Act::SyncPoint)));
                    }
                    items.push(menu::Item::action(
                        "Clear Synchronization Points",
                        None,
                        Message::Act(Act::ClearSyncPoints),
                    ));
                }
                (items, true)
            }
        };
        if positioned {
            // At the pointer, in window coordinates, wherever this view is
            // embedded.
            let Popup::Context { x, y, .. } = popup else {
                unreachable!()
            };
            return menu::at_point(base, menu_view(items), iced::Point::new(x, y), Message::ClosePopup);
        }
        let menu = opaque(menu_view(items));
        let layer: Element<'a, Message> = container(row![space::horizontal(), menu])
            .padding(Padding {
                top: 2.0,
                right: 8.0,
                ..Padding::ZERO
            })
            .into();
        stack![
            base,
            mouse_area(container(layer).width(Length::Fill).height(Length::Fill)).on_press(Message::ClosePopup)
        ]
        .into()
    }

    pub(super) fn with_dialog<'a>(&'a self, base: Element<'a, Message>, _shared: &'a Shared) -> Element<'a, Message> {
        let Some(dialog) = &self.dialog else {
            return base;
        };
        let cancel = dialog_message(DialogMessage::Cancel);
        let content: Element<'a, Message> = match dialog {
            Dialog::ConfirmClose { save } => {
                let mut list = column![
                    text(
                        "Open documents contain unsaved changes. Changes which are not saved will be permanently lost."
                    )
                    .size(14)
                ]
                .spacing(8);
                for (i, checked) in save.iter().enumerate() {
                    let modified = self.is_modified(i);
                    let mut cb = checkbox(*checked && modified)
                        .label(self.panes[i].file.label())
                        .text_size(14);
                    if modified {
                        cb = cb.on_toggle(move |b| dialog_message(DialogMessage::CloseCheck(i, b)));
                    }
                    list = list.push(cb);
                }
                modal::dialog(
                    "Save changes to documents before closing?",
                    list,
                    vec![
                        button(text("Cancel"))
                            .style(button::secondary)
                            .on_press(cancel.clone())
                            .into(),
                        button(text("Discard"))
                            .style(button::danger)
                            .on_press(dialog_message(DialogMessage::CloseDiscard))
                            .into(),
                        button(text("Save"))
                            .on_press(dialog_message(DialogMessage::CloseSave))
                            .into(),
                    ],
                )
            }
            Dialog::ConfirmDiscard { unsaved, .. } => {
                let mut list =
                    column![text("Changes made to the following documents will be permanently lost:").size(14)]
                        .spacing(4);
                for name in unsaved {
                    list = list.push(text(format!("\t• {name}")).size(14));
                }
                modal::dialog(
                    "Discard unsaved changes to documents?",
                    list,
                    vec![
                        button(text("Cancel"))
                            .style(button::secondary)
                            .on_press(cancel.clone())
                            .into(),
                        button(text("Discard"))
                            .style(button::danger)
                            .on_press(dialog_message(DialogMessage::DiscardConfirm))
                            .into(),
                    ],
                )
            }
            Dialog::Resolve => modal::dialog(
                "Mark conflict as resolved?",
                text("If the conflict was resolved successfully, you may mark it as resolved now.").size(14),
                vec![
                    button(text("Cancel"))
                        .style(button::secondary)
                        .on_press(cancel.clone())
                        .into(),
                    button(text("Mark Resolved"))
                        .on_press(dialog_message(DialogMessage::Resolve))
                        .into(),
                ],
            ),
            Dialog::Error { primary, secondary } => modal::dialog(
                primary.as_str(),
                text(secondary.as_str()).size(14),
                vec![button(text("Close")).on_press(cancel.clone()).into()],
            ),
            Dialog::Patch {
                left,
                reverse,
                text: patch,
            } => {
                let mut body = column![].spacing(10);
                if self.panes.len() == 3 {
                    body = body.push(
                        row![
                            text("Use differences between:").size(14),
                            checkbox(*left)
                                .label("Left and middle panes")
                                .text_size(14)
                                .on_toggle(|_| dialog_message(DialogMessage::PatchLeft(true))),
                            checkbox(!*left)
                                .label("Middle and right panes")
                                .text_size(14)
                                .on_toggle(|_| dialog_message(DialogMessage::PatchLeft(false))),
                        ]
                        .spacing(12),
                    );
                }
                body = body.push(
                    checkbox(*reverse)
                        .label("Reverse patch direction")
                        .text_size(14)
                        .on_toggle(|b| dialog_message(DialogMessage::PatchReverse(b))),
                );
                body = body.push(
                    container(
                        scrollable(
                            container(
                                text(patch.as_str())
                                    .font(self.font)
                                    .size(13)
                                    .wrapping(iced::widget::text::Wrapping::None),
                            )
                            .padding(8),
                        )
                        .direction(iced::widget::scrollable::Direction::Both {
                            vertical: Default::default(),
                            horizontal: Default::default(),
                        })
                        .width(Length::Fill)
                        .height(400),
                    )
                    .width(600)
                    .style(|theme: &iced::Theme| container::Style {
                        background: Some(theme.extended_palette().background.base.color.into()),
                        border: iced::Border::default()
                            .width(1)
                            .color(theme.extended_palette().background.strong.color),
                        ..Default::default()
                    }),
                );
                modal::dialog(
                    "Format as Patch",
                    body,
                    vec![
                        button(text("Copy to Clipboard"))
                            .style(button::secondary)
                            .on_press(dialog_message(DialogMessage::PatchCopy))
                            .into(),
                        button(text("Cancel"))
                            .style(button::secondary)
                            .on_press(cancel.clone())
                            .into(),
                        button(text("Save Patch"))
                            .on_press(dialog_message(DialogMessage::PatchSave))
                            .into(),
                    ],
                )
            }
            Dialog::Encoding { pane, filter } => {
                let current = self.panes[*pane].file.encoding.name();
                let needle = filter.to_lowercase();
                let mut list = column![];
                for &label in ENCODINGS.iter().filter(|l| l.to_lowercase().contains(&needle)) {
                    let name = meld_files::TextEncoding::for_label(label)
                        .map(|e| e.name())
                        .unwrap_or(label);
                    let selected = name == current;
                    list = list.push(
                        button(text(label).size(14))
                            .width(Length::Fill)
                            .padding([4, 10])
                            .style(ui::tab_button(selected))
                            .on_press(dialog_message(DialogMessage::PickEncoding(label))),
                    );
                }
                self.selector_dialog("Reload with Encoding", filter, list.into(), cancel.clone())
            }
            Dialog::Language { pane, filter } => {
                let current = self.panes[*pane].language;
                let needle = filter.to_lowercase();
                let mut list = column![];
                if "plain text".contains(&needle) {
                    list = list.push(
                        button(text("Plain Text").size(14))
                            .width(Length::Fill)
                            .padding([4, 10])
                            .style(ui::tab_button(current.is_none()))
                            .on_press(dialog_message(DialogMessage::PickLanguage(None))),
                    );
                }
                let mut languages: Vec<&meld_files::LanguageSpec> = meld_files::LANGUAGES
                    .iter()
                    .filter(|l| l.name.to_lowercase().contains(&needle))
                    .collect();
                languages.sort_by_key(|l| l.name.to_lowercase());
                for lang in languages {
                    list = list.push(
                        button(text(lang.name).size(14))
                            .width(Length::Fill)
                            .padding([4, 10])
                            .style(ui::tab_button(current == Some(lang.id)))
                            .on_press(dialog_message(DialogMessage::PickLanguage(Some(lang.id)))),
                    );
                }
                self.selector_dialog("Highlighting Mode", filter, list.into(), cancel.clone())
            }
            Dialog::Display { pane } => {
                let o = self.view_options;
                let pane = (*pane).min(self.panes.len() - 1);
                let p = &self.panes[pane];
                // The status bar items a narrow pane hides are here too.
                let row_item = |label: &'a str, value: Element<'a, Message>| -> Element<'a, Message> {
                    row![text(label).size(14).width(150), value]
                        .spacing(8)
                        .align_y(iced::Alignment::Center)
                        .into()
                };
                let body = column![
                    checkbox(o.show_line_numbers)
                        .label("Show line numbers")
                        .on_toggle(|_| Message::Act(Act::ToggleLineNumbers)),
                    checkbox(o.show_whitespace)
                        .label("Show whitespace")
                        .on_toggle(|_| Message::Act(Act::ToggleWhitespace)),
                    checkbox(o.highlight_current_line)
                        .label("Highlight current line")
                        .on_toggle(|_| Message::Act(Act::ToggleCurrentLine)),
                    checkbox(o.wrap)
                        .label("Text wrapping")
                        .on_toggle(|_| Message::Act(Act::ToggleWrap)),
                    ui::separator(false),
                    row_item(
                        "Text encoding",
                        button(text(super::view::encoding_label(p)).size(13))
                            .padding([4, 10])
                            .style(ui::flat_button)
                            .on_press(Message::Act(Act::ChooseEncoding(pane)))
                            .into(),
                    ),
                    row_item(
                        "Highlighting",
                        button(text(super::view::language_label(p)).size(13))
                            .padding([4, 10])
                            .style(ui::flat_button)
                            .on_press(Message::Act(Act::ChooseLanguage(pane)))
                            .into(),
                    ),
                    row_item("Line endings", super::view::newline_picker(pane, p.file.newline)),
                    checkbox(self.overwrite)
                        .label("Overwrite mode")
                        .on_toggle(|_| Message::Act(Act::ToggleOverwrite)),
                ]
                .spacing(10);
                modal::dialog(
                    "Display",
                    body,
                    vec![button(text("Close")).on_press(cancel.clone()).into()],
                )
            }
        };
        modal::modal(base, content, cancel)
    }

    fn selector_dialog<'a>(
        &'a self,
        title: &'a str,
        filter: &'a str,
        list: Element<'a, Message>,
        cancel: Message,
    ) -> Element<'a, Message> {
        let body = column![
            text_input("Search", filter)
                .id(self.dialog_input_id.clone())
                .size(14)
                .padding([4, 8])
                .on_input(|t| dialog_message(DialogMessage::Filter(t))),
            container(scrollable(list).height(360)).width(360),
            Space::new().height(0),
        ]
        .spacing(8);
        modal::dialog(
            title,
            body,
            vec![button(text("Cancel")).style(button::secondary).on_press(cancel).into()],
        )
    }
}
