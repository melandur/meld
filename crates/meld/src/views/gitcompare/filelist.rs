//! The changed-files tree on the left of a Git Compare tab (the file list
//! of `gitcompare.py`): rows from [`build_file_tree`], collapsible folders,
//! status letters, line counts and file navigation (Alt+F7).

use std::collections::HashSet;

use iced::widget::{button, column, container, row, scrollable, text, tooltip};
use iced::{Color, Element, Length, Theme};
use meld_ailens::{FileEntry, FileRow, GitFileChange};

use super::ellipsis::{Mode, ellipsized};
use crate::ui;

/// How many pixels each tree level is indented.
const INDENT: f32 = 14.0;
/// Height of a row's text line; rows add the row spacing above and below.
const ROW_TEXT_HEIGHT: f32 = 18.0;

/// A visible row of the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ListRow {
    pub depth: usize,
    pub name: String,
    pub kind: RowKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    /// `key` is the folder's path from the root, unique within the tree.
    Folder { key: String, expanded: bool },
    File(FileEntry),
}

/// The rows to show: folders with their children unless collapsed.
pub fn visible_rows(tree: &[FileRow], collapsed: &HashSet<String>) -> Vec<ListRow> {
    fn walk(rows: &[FileRow], parent: &str, depth: usize, collapsed: &HashSet<String>, out: &mut Vec<ListRow>) {
        for row in rows {
            match &row.entry {
                Some(entry) => out.push(ListRow {
                    depth,
                    name: row.name.clone(),
                    kind: RowKind::File(entry.clone()),
                }),
                None => {
                    let key = folder_key(parent, &row.name);
                    let expanded = !collapsed.contains(&key);
                    out.push(ListRow {
                        depth,
                        name: row.name.clone(),
                        kind: RowKind::Folder {
                            key: key.clone(),
                            expanded,
                        },
                    });
                    if expanded {
                        walk(&row.children, &key, depth + 1, collapsed, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, "", 0, collapsed, &mut out);
    out
}

fn folder_key(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// Every file in the order the tree shows it, collapsed folders included.
pub fn ordered_entries(tree: &[FileRow]) -> Vec<&FileEntry> {
    fn walk<'a>(rows: &'a [FileRow], out: &mut Vec<&'a FileEntry>) {
        for row in rows {
            if let Some(entry) = &row.entry {
                out.push(entry);
            }
            walk(&row.children, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

/// The file `step` files away from `current` in tree order, stopping at
/// either end; the first file when nothing is selected.
pub fn relative_entry(tree: &[FileRow], current: Option<&FileEntry>, step: isize) -> Option<FileEntry> {
    let ordered = ordered_entries(tree);
    if ordered.is_empty() {
        return None;
    }
    let index = current
        .and_then(|c| ordered.iter().position(|e| *e == c))
        .map_or(-1, |i| i as isize);
    let index = (index + step).clamp(0, ordered.len() as isize - 1) as usize;
    Some(ordered[index].clone())
}

/// Folder keys that contain `entry`, so that selecting it can open them.
pub fn ancestor_keys(tree: &[FileRow], entry: &FileEntry) -> Vec<String> {
    fn walk(rows: &[FileRow], parent: &str, entry: &FileEntry, path: &mut Vec<String>) -> bool {
        for row in rows {
            if row.entry.as_ref() == Some(entry) {
                return true;
            }
            if row.entry.is_none() {
                let key = folder_key(parent, &row.name);
                path.push(key.clone());
                if walk(&row.children, &key, entry, path) {
                    return true;
                }
                path.pop();
            }
        }
        false
    }
    let mut path = Vec::new();
    walk(tree, "", entry, &mut path);
    path
}

/// The status letter as Source Control shows it: U for untracked files.
pub fn status_code(change: &GitFileChange) -> char {
    if change.untracked { 'U' } else { change.status.letter() }
}

pub fn status_name(code: char) -> &'static str {
    match code {
        'A' => "Added",
        'C' => "Copied",
        'D' => "Deleted",
        'M' => "Modified",
        'R' => "Renamed",
        'T' => "Type changed",
        'U' => "Untracked",
        _ => "Unknown",
    }
}

/// e.g. `+12 −3`, `binary`, or nothing when git gave no counts.
pub fn stats_text(entry: &FileEntry) -> String {
    if entry.binary() {
        "binary".into()
    } else if let Some(additions) = entry.additions {
        format!("+{additions} −{}", entry.deletions.unwrap_or(0))
    } else {
        String::new()
    }
}

pub fn row_tooltip(entry: &FileEntry) -> String {
    let change = &entry.change;
    let code = status_code(change);
    let mut tip = format!("{} · {}", change.path, status_name(code));
    if let Some(original) = change.original_path.as_ref().filter(|o| **o != change.path) {
        tip.push_str(&format!("\nfrom {original}"));
    }
    tip
}

fn status_color(code: char) -> Color {
    let hex = |rgb: u32| Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
    match code {
        'A' | 'U' => hex(0x26a269),
        'M' | 'T' => hex(0xe5a50a),
        'D' => hex(0xc01c28),
        'R' | 'C' => hex(0x3584e4),
        _ => hex(0x888888),
    }
}

/// Height of one row with `spacing` pixels above and below its text.
pub fn row_height(spacing: f32) -> f32 {
    ROW_TEXT_HEIGHT + 2.0 * spacing
}

/// The rows' spacing from `ailens-file-row-spacing`, within its range.
pub fn row_spacing(setting: i32) -> f32 {
    let (lo, hi) = meld_config::settings::AILENS_FILE_ROW_SPACING_RANGE;
    setting.clamp(lo, hi) as f32
}

/// Where to scroll the list (offset `offset`, `height` high) so that row
/// `index` shows; `None` if it does already. Above the first file only
/// folders are listed: scrolling to the top keeps them in view.
pub fn scroll_target(rows: &[ListRow], index: usize, row_height: f32, offset: f32, height: f32) -> Option<f32> {
    // The list's padding below the last row
    const PADDING: f32 = 4.0;
    let top = index as f32 * row_height;
    let first_file = rows[..index.min(rows.len())]
        .iter()
        .all(|r| matches!(r.kind, RowKind::Folder { .. }));
    if top < offset {
        Some(if first_file { 0.0 } else { top })
    } else if top + row_height + PADDING > offset + height {
        Some((top + row_height + PADDING - height).max(0.0))
    } else {
        None
    }
}

/// What clicking rows does.
pub struct Callbacks<M> {
    pub select: fn(FileEntry) -> M,
    pub toggle: fn(String) -> M,
}

/// The tree as a scrollable list.
pub fn view<'a, M: Clone + 'a>(
    rows: &'a [ListRow],
    current: Option<&'a FileEntry>,
    spacing: f32,
    scroll_id: iced::widget::Id,
    on_scroll: impl Fn(scrollable::Viewport) -> M + 'a,
    callbacks: Callbacks<M>,
) -> Element<'a, M> {
    let height = row_height(spacing);
    let mut list = column![].spacing(0).padding([2, 4]);
    for list_row in rows {
        let indent = list_row.depth as f32 * INDENT;
        let content: Element<'a, M> = match &list_row.kind {
            RowKind::Folder { key, expanded } => {
                let arrow = if *expanded { "⏷" } else { "⏵" };
                let body = row![
                    text(arrow).size(11).width(12).style(ui::dim_text),
                    ellipsized(list_row.name.as_str(), Mode::Middle)
                        .size(13)
                        .width(Length::Fill),
                ]
                .spacing(4)
                .align_y(iced::Center);
                let b = button(container(body).padding(iced::Padding {
                    left: indent,
                    ..iced::Padding::ZERO
                }))
                .width(Length::Fill)
                .height(height)
                .padding([0, 4])
                .style(row_style(false))
                .on_press((callbacks.toggle)(key.clone()));
                tip(b.into(), list_row.name.clone())
            }
            RowKind::File(entry) => {
                let code = status_code(&entry.change);
                let selected = current == Some(entry);
                // The name gives way, so that the counts and the status
                // stay readable
                let body = row![
                    ellipsized(list_row.name.as_str(), Mode::Middle)
                        .size(13)
                        .width(Length::Fill),
                    text(stats_text(entry))
                        .size(11)
                        .style(ui::dim_text)
                        .wrapping(text::Wrapping::None),
                    text(code.to_string())
                        .size(13)
                        .font(iced::Font {
                            weight: iced::font::Weight::Bold,
                            ..iced::Font::MONOSPACE
                        })
                        .color(status_color(code))
                        .width(14)
                        .align_x(iced::alignment::Horizontal::Right),
                ]
                .spacing(6)
                .align_y(iced::Center);
                let b = button(container(body).clip(true).padding(iced::Padding {
                    left: indent + 16.0,
                    ..iced::Padding::ZERO
                }))
                .width(Length::Fill)
                .height(height)
                .padding([0, 4])
                .style(row_style(selected))
                .on_press((callbacks.select)(entry.clone()));
                tip(b.into(), row_tooltip(entry))
            }
        };
        list = list.push(content);
    }
    scrollable(list)
        .spacing(2)
        .id(scroll_id)
        .on_scroll(on_scroll)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
}

fn tip<'a, M: 'a>(content: Element<'a, M>, tip: String) -> Element<'a, M> {
    tooltip(
        content,
        container(text(tip).size(12)).padding([4, 8]).style(ui::tooltip_box),
        tooltip::Position::FollowCursor,
    )
    .delay(std::time::Duration::from_millis(700))
    .into()
}

fn row_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.extended_palette();
        let mut style = ui::flat_button(theme, status);
        style.border = iced::Border::default().rounded(4);
        if selected {
            style.background = Some(palette.primary.weak.color.scale_alpha(0.55).into());
            style.text_color = palette.background.base.text;
        }
        style
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meld_ailens::{GitFileStatus, build_file_tree};

    fn entry(path: &str) -> FileEntry {
        FileEntry::new(GitFileChange::new(GitFileStatus::Modified, path), Some(1), Some(2))
    }

    fn sample() -> Vec<FileRow> {
        build_file_tree(&[
            entry("README.md"),
            entry("src/a/b/one.rs"),
            entry("src/a/b/two.rs"),
            entry("src/main.rs"),
            entry("docs/guide.md"),
        ])
    }

    fn names(rows: &[ListRow]) -> Vec<String> {
        rows.iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    #[test]
    fn rows_follow_the_tree_folders_first() {
        let rows = visible_rows(&sample(), &HashSet::new());
        assert_eq!(
            names(&rows),
            ["docs", "  guide.md", "src", "  a/b", "    one.rs", "    two.rs", "  main.rs", "README.md"]
        );
    }

    #[test]
    fn collapsed_folders_hide_their_files() {
        let collapsed: HashSet<String> = ["src/a/b".to_string()].into();
        let rows = visible_rows(&sample(), &collapsed);
        assert_eq!(names(&rows), ["docs", "  guide.md", "src", "  a/b", "  main.rs", "README.md"]);
        assert!(matches!(&rows[3].kind, RowKind::Folder { expanded: false, .. }));
    }

    #[test]
    fn navigation_walks_tree_order_and_stops_at_the_ends() {
        let tree = sample();
        let first = relative_entry(&tree, None, 1).unwrap();
        assert_eq!(first.change.path, "docs/guide.md");
        let next = relative_entry(&tree, Some(&first), 1).unwrap();
        assert_eq!(next.change.path, "src/a/b/one.rs");
        let last = entry("README.md");
        assert_eq!(relative_entry(&tree, Some(&last), 1).unwrap(), last);
        let back = relative_entry(&tree, Some(&next), -1).unwrap();
        assert_eq!(back, first);
        assert_eq!(relative_entry(&tree, Some(&first), -1).unwrap(), first);
        assert!(relative_entry(&[], None, 1).is_none());
    }

    #[test]
    fn ancestors_name_every_enclosing_folder() {
        let tree = sample();
        assert_eq!(ancestor_keys(&tree, &entry("src/a/b/two.rs")), ["src", "src/a/b"]);
        assert!(ancestor_keys(&tree, &entry("README.md")).is_empty());
    }

    #[test]
    fn scrolling_reveals_rows_and_the_folders_above_the_first_file() {
        let rows = visible_rows(&sample(), &HashSet::new());
        // Rows are 20 px high; the list shows 100 px from y = 60
        assert_eq!(scroll_target(&rows, 4, 20.0, 60.0, 100.0), None);
        assert_eq!(scroll_target(&rows, 2, 20.0, 60.0, 100.0), Some(40.0));
        assert_eq!(scroll_target(&rows, 7, 20.0, 60.0, 100.0), Some(64.0));
        // guide.md follows only its folder: the top of the list
        assert_eq!(scroll_target(&rows, 1, 20.0, 30.0, 100.0), Some(0.0));
        assert_eq!(row_spacing(40), 16.0);
        assert_eq!(row_spacing(-3), 0.0);
    }

    #[test]
    fn status_and_stats() {
        let untracked = FileEntry::new(GitFileChange::untracked("new.txt"), Some(3), Some(0));
        assert_eq!(status_code(&untracked.change), 'U');
        assert_eq!(stats_text(&untracked), "+3 −0");
        let binary = FileEntry::new(GitFileChange::new(GitFileStatus::Added, "a.png"), None, None);
        assert_eq!(stats_text(&binary), "binary");
        let renamed = FileEntry::new(
            GitFileChange::renamed(GitFileStatus::Renamed, "b.rs", "a.rs", Some(90)),
            Some(0),
            Some(0),
        );
        assert_eq!(row_tooltip(&renamed), "b.rs · Renamed\nfrom a.rs");
    }
}
