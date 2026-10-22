//! Pane colours from the style scheme (`MeldSourceView`'s colour lookups
//! and `get_common_theme()`), including syntax styles.

use iced::Color;
use meld_config::{DiffColors, DiffSide, StyleAttr, Underline};
use meld_widgets::{ChunkColor, ChunkColors, PaneStyle, TextStyle};

use crate::shared::{Shared, color};

fn chunk(c: meld_config::ChunkColors) -> ChunkColor {
    ChunkColor {
        fill: color(c.fill),
        line: color(c.line),
    }
}

/// The style for a pane on `side` of the comparison (`None` for
/// three-way panes and the link maps).
pub fn pane_style(shared: &Shared, side: Option<DiffSide>) -> PaneStyle {
    let theme = &shared.style;
    let mut style = if theme.is_dark() {
        PaneStyle::dark()
    } else {
        PaneStyle::light()
    };
    let look = |name: &str, attr| shared.scheme_color(name, attr);
    let colors: DiffColors = theme.diff_colors(shared.settings.diff_colors, side);

    if let Some(c) = look("text", StyleAttr::Foreground) {
        style.text = c;
    }
    if let Some(c) = look("text", StyleAttr::Background) {
        style.background = c;
        style.map_base = c;
    }
    if let Some(c) = look("line-numbers", StyleAttr::Foreground) {
        style.line_number = c;
    }
    if let Some(c) = look("line-numbers", StyleAttr::Background) {
        style.gutter_background = c;
    }
    style.line_number_current = look("current-line-number", StyleAttr::Foreground).unwrap_or(style.line_number);
    if let Some(c) = look("selection", StyleAttr::Background) {
        style.selection = c;
        style.selection_unfocused =
            look("selection-unfocused", StyleAttr::Background).unwrap_or(Color { a: c.a * 0.6, ..c });
    }
    if let Some(c) = look("cursor", StyleAttr::Foreground) {
        style.cursor = c;
    } else {
        style.cursor = style.text;
    }
    if let Some(c) = look("draw-spaces", StyleAttr::Foreground) {
        style.whitespace = c;
    }

    style.current_line = color(colors.current_line_highlight);
    style.sync_point = color(colors.syncpoint_outline);
    style.overscroll = color(colors.overscroll);
    style.inline = color(colors.inline);
    style.chunks = ChunkColors {
        insert: chunk(colors.insert),
        delete: chunk(colors.delete),
        replace: chunk(colors.replace),
        conflict: chunk(colors.conflict),
        error: chunk(colors.error),
        current_chunk: color(colors.current_chunk_highlight),
    };

    let palette = shared.theme.extended_palette();
    style.widget_background = palette.background.weak.color;
    style.border = palette.background.strong.color;
    style.map_text = palette.background.base.text;
    style.button_icon = palette.background.base.text;

    if shared.settings.highlight_syntax {
        style.syntax = syntax_styles(shared);
    }
    style
}

/// Syntax styles (`def:*` and language styles) from the scheme.
fn syntax_styles(shared: &Shared) -> std::collections::HashMap<String, TextStyle> {
    let scheme = shared.style.scheme();
    let mut out = std::collections::HashMap::new();
    for name in scheme.style_names() {
        if !name.contains(':') || name.starts_with("meld:") {
            continue;
        }
        let Some(s) = scheme.style(&name) else {
            continue;
        };
        let ts = TextStyle {
            fg: s.foreground.map(color),
            bg: s.background.map(color),
            bold: s.is_bold(),
            italic: s.is_italic(),
            underline: s.underline.is_some_and(|u| u != Underline::None),
            strikethrough: s.is_strikethrough(),
        };
        out.insert(name, ts);
    }
    out
}
