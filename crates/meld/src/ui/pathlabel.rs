//! The drop-down of a path label (`meld/ui/pathlabel.py`).
//!
//! Clicking a path label opens a popover with the full path, its folders
//! as a row of buttons to pick a parent folder, and buttons to copy the
//! picked path or show it in the file manager.

use std::path::{Path, PathBuf};

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Background, Border, Element, Length, Theme};

/// The path and its ancestors, outermost first: `/a/b` gives `/`, `/a`,
/// `/a/b`.
pub fn segments(path: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = path.ancestors().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).collect();
    out.reverse();
    out
}

/// The label of a segment button: the last component, or the whole path
/// for the file system root.
pub fn segment_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// What the popover's buttons send.
pub struct Actions<M> {
    /// A segment was picked; its index in [`segments`].
    pub pick: Box<dyn Fn(usize) -> M>,
    /// Copy the picked path.
    pub copy: M,
    /// Show the picked path's containing folder.
    pub open_folder: M,
}

/// The popover for `path`, with segment `picked` (an index into
/// [`segments`]) chosen; `None` picks the full path.
pub fn popover<'a, M: Clone + 'a>(path: &Path, picked: Option<usize>, actions: Actions<M>) -> Element<'a, M> {
    let all = segments(path);
    let picked = picked.filter(|&i| i < all.len()).unwrap_or(all.len().saturating_sub(1));
    let shown = all.get(picked).cloned().unwrap_or_else(|| path.to_path_buf());

    let mut crumbs = row![].spacing(2).align_y(iced::Center);
    for (i, segment) in all.iter().enumerate() {
        if i > 0 {
            crumbs = crumbs.push(text("›").size(13).style(super::dim_text));
        }
        let label = text(segment_label(segment)).size(13).font(if i == picked {
            super::bold()
        } else {
            iced::Font::DEFAULT
        });
        crumbs = crumbs.push(
            button(label)
                .padding([2, 6])
                .style(super::tab_button(i == picked))
                .on_press((actions.pick)(i)),
        );
    }

    let shown_text = shown.display().to_string();
    let body = column![
        // Scrolled to the end, so the innermost folders show first.
        scrollable(crumbs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(4).scroller_width(4).spacing(4),
            ))
            .anchor_right()
            .width(Length::Fill),
        row![
            text("Path").size(13).style(super::dim_text),
            container(text(shown_text).size(13).wrapping(text::Wrapping::WordOrGlyph))
                .padding([4, 8])
                .width(Length::Fill)
                .style(field),
        ]
        .spacing(12)
        .align_y(iced::Center),
        row![
            space::horizontal(),
            button(text("Copy Path").size(13))
                .padding([4, 10])
                .style(button::secondary)
                .on_press(actions.copy.clone()),
            button(text("Open Containing Folder").size(13))
                .padding([4, 10])
                .style(button::secondary)
                .on_press(actions.open_folder.clone()),
        ]
        .spacing(6),
    ]
    .spacing(8);
    container(body).padding(10).max_width(560).style(super::popover).into()
}

fn field(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color)),
        border: Border::default().rounded(4).width(1).color(palette.background.strong.color),
        ..container::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments() {
        assert_eq!(
            segments(Path::new("/home/me/src")),
            [PathBuf::from("/"), PathBuf::from("/home"), PathBuf::from("/home/me"), PathBuf::from("/home/me/src")]
        );
        assert_eq!(segment_label(Path::new("/")), "/");
        assert_eq!(segment_label(Path::new("/home/me")), "me");
        assert_eq!(segments(Path::new("")), Vec::<PathBuf>::new());
    }
}
