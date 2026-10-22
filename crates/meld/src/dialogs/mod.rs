//! Window-level dialogs: preferences, keyboard shortcuts, about, and the
//! unsaved-changes prompt on quit.

pub mod preferences;

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Length};

use crate::command::SHORTCUTS;
use crate::ui::{self, modal::dialog};

pub fn shortcuts<'a, M: Clone + 'a>(on_close: M) -> Element<'a, M> {
    let mut sections = column![].spacing(18);
    for (heading, entries) in SHORTCUTS {
        let mut list = column![text(*heading).size(15).font(ui::bold())].spacing(4);
        for (keys, action) in *entries {
            list = list.push(
                row![
                    text(*action).size(14).width(Length::Fill),
                    text(*keys).size(13).style(ui::dim_text)
                ]
                .spacing(16),
            );
        }
        sections = sections.push(list);
    }
    dialog(
        "Keyboard Shortcuts",
        scrollable(container(sections).padding([0, 12])).height(480).width(560),
        vec![button(text("Close")).on_press(on_close).into()],
    )
}

pub fn about<'a, M: Clone + 'a>(on_close: M) -> Element<'a, M> {
    let body = column![
        text("Meld").size(28).font(ui::bold()),
        text(concat!("Version ", env!("CARGO_PKG_VERSION"))).size(14),
        text("Visual diff and merge tool targeted at developers. Compare files, \
              folders and version controlled projects, with two- and three-way \
              comparisons and AI Lens explanations for Git changes.")
            .size(14),
        text("Copyright © 2002-2009 Stephen Kennedy\nCopyright © 2009-2026 Kai Willadsen\nRust and iced port: the Meld developers")
            .size(13)
            .style(ui::dim_text),
        text("Licensed under the GNU General Public License, version 2 or later.")
            .size(13)
            .style(ui::dim_text),
    ]
    .spacing(10)
    .max_width(460);
    dialog("About Meld", body, vec![button(text("Close")).on_press(on_close).into()])
}

pub fn confirm_quit<'a, M: Clone + 'a>(unsaved: &'a [String], on_cancel: M, on_quit: M) -> Element<'a, M> {
    let mut list = column![text("If you quit without saving, changes to these documents will be lost:").size(14)]
        .spacing(4);
    for name in unsaved {
        list = list.push(text(format!("  • {name}")).size(14));
    }
    dialog(
        "Quit without saving?",
        list,
        vec![
            button(text("Cancel")).style(button::secondary).on_press(on_cancel).into(),
            button(text("Quit Without Saving")).style(button::danger).on_press(on_quit).into(),
        ],
    )
}
