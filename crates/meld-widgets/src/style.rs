//! Colours and text styles shared by all widgets in this crate.
//!
//! [`PaneStyle`] plays the role of the GtkSourceView style scheme plus
//! Meld's `get_common_theme()`: the application fills it from the active
//! style scheme (see `meld-config`) or uses one of the built-in presets.

use std::collections::HashMap;

use iced::Color;

use crate::chunk::ChunkKind;
use crate::highlight::style_fallback;

/// Text attributes for a syntax style (a GtkSourceView `<style>`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextStyle {
    /// Foreground colour; `None` keeps the default text colour.
    pub fg: Option<Color>,
    /// Background colour drawn behind the characters.
    pub bg: Option<Color>,
    /// Bold weight.
    pub bold: bool,
    /// Italic slant.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Strike-through.
    pub strikethrough: bool,
}

impl TextStyle {
    /// A style with only a foreground colour.
    pub fn fg(color: Color) -> Self {
        Self {
            fg: Some(color),
            ..Self::default()
        }
    }

    /// Sets bold.
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Sets italic.
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// Sets underline.
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Sets strike-through.
    pub fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }
}

/// Fill and outline colour of one chunk kind (`background` and
/// `line-background` of the `meld:<kind>` style).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkColor {
    /// Chunk fill.
    pub fill: Color,
    /// Chunk outline.
    pub line: Color,
}

/// Chunk colours for all kinds, as returned by Meld's `get_common_theme()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkColors {
    /// `meld:insert`.
    pub insert: ChunkColor,
    /// `meld:delete` (Meld uses the insert colours by default).
    pub delete: ChunkColor,
    /// `meld:replace`.
    pub replace: ChunkColor,
    /// `meld:conflict`.
    pub conflict: ChunkColor,
    /// `meld:error`.
    pub error: ChunkColor,
    /// `meld:current-chunk-highlight` overlay.
    pub current_chunk: Color,
}

impl ChunkColors {
    /// Colours for a chunk kind.
    pub fn get(&self, kind: ChunkKind) -> ChunkColor {
        match kind {
            ChunkKind::Insert => self.insert,
            ChunkKind::Delete => self.delete,
            ChunkKind::Replace => self.replace,
            ChunkKind::Conflict => self.conflict,
            ChunkKind::Error => self.error,
        }
    }

    /// Returns a copy with insert and delete swapped (for panes on the
    /// "new" side, as Meld does in RTL layouts / diff palettes).
    pub fn swapped(mut self) -> Self {
        std::mem::swap(&mut self.insert, &mut self.delete);
        self
    }
}

/// All colours used by the text pane, link map, chunk map and gutter.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneStyle {
    /// Default text colour (`text` foreground).
    pub text: Color,
    /// Text background (`text` background).
    pub background: Color,
    /// Line number colour (`line-numbers` foreground).
    pub line_number: Color,
    /// Line number of the cursor line (`current-line-number`).
    pub line_number_current: Color,
    /// Gutter background (`line-numbers` background / theme bg).
    pub gutter_background: Color,
    /// Current line highlight (`meld:current-line-highlight` background).
    pub current_line: Color,
    /// Selection background when focused (`selection`).
    pub selection: Color,
    /// Selection background when unfocused (`selection-unfocused`).
    pub selection_unfocused: Color,
    /// Cursor colour (`cursor`).
    pub cursor: Color,
    /// Whitespace markers (`draw-spaces`).
    pub whitespace: Color,
    /// Sync point outline (`meld:syncpoint-outline`).
    pub sync_point: Color,
    /// Area below the last line (`meld:overscroll`).
    pub overscroll: Color,
    /// Intraline change background (`meld:inline`).
    pub inline: Color,
    /// Chunk colours.
    pub chunks: ChunkColors,
    /// Widget background used by link map and action gutter
    /// (`@theme_bg_color`).
    pub widget_background: Color,
    /// Border colour (`@borders`), used for the chunk map outline.
    pub border: Color,
    /// Chunk map base (`@theme_base_color`).
    pub map_base: Color,
    /// Chunk map text colour (`@theme_text_color`), used for the
    /// viewport handle at 0.2 (fill) and 0.4 (outline) alpha.
    pub map_text: Color,
    /// Action button hover/pressed background.
    pub button_hover: Color,
    /// Action button icon colour.
    pub button_icon: Color,
    /// Horizontal scrollbar thumb.
    pub scrollbar: Color,
    /// Syntax styles keyed by GtkSourceView style name
    /// (`"def:keyword"`, `"python:module-handler"`, ...).
    pub syntax: HashMap<String, TextStyle>,
}

impl PaneStyle {
    /// Resolves a syntax style by name, following GtkSourceView's
    /// `def:` fallback chain (e.g. `def:keyword` → `def:statement`).
    pub fn syntax_style(&self, name: &str) -> Option<TextStyle> {
        let mut name = name;
        loop {
            if let Some(style) = self.syntax.get(name) {
                return Some(*style);
            }
            name = style_fallback(name)?;
        }
    }

    /// Light preset (Meld Lens Light scheme, Adwaita widget colours).
    pub fn light() -> Self {
        let p = LensPalette {
            comment: hex(0x008000),
            doc_comment_element: hex(0x3f8a3f),
            keyword: hex(0x0000ff),
            control: hex(0xaf00db),
            string: hex(0xa31515),
            number: hex(0x098658),
            ty: hex(0x267f99),
            function: hex(0x795e26),
            variable: hex(0x001080),
            constant: hex(0x0070c1),
            escape: hex(0xee0000),
            error: hex(0xe51400),
            warning: hex(0xbf8803),
            note: hex(0x1a85ff),
            foreground: hex(0x1f1f1f),
            gutter_fg: hex(0x6e7781),
            heading: hex(0x800000),
            list_marker: hex(0x0070c1),
            added_fg: hex(0x1a7f37),
            removed_fg: hex(0xcf222e),
        };
        Self {
            text: hex(0x1f1f1f),
            background: hex(0xffffff),
            line_number: hex(0x6e7781),
            line_number_current: hex(0x0b216f),
            gutter_background: hex(0xf6f5f4),
            current_line: rgba(255, 213, 0, 0.18),
            selection: hex(0xadd6ff),
            selection_unfocused: hex(0xe5ebf1),
            cursor: hex(0x000000),
            whitespace: hex(0xc8ccd0),
            sync_point: hex(0x555555),
            overscroll: rgba(50, 50, 50, 0.1),
            inline: hex(0xb6e3ff),
            chunks: ChunkColors {
                insert: ChunkColor {
                    fill: hex(0xe6ffec),
                    line: hex(0x4ac26b),
                },
                delete: ChunkColor {
                    fill: hex(0xffebe9),
                    line: hex(0xff8182),
                },
                replace: ChunkColor {
                    fill: hex(0xddf4ff),
                    line: hex(0x54aeff),
                },
                conflict: ChunkColor {
                    fill: hex(0xffcecb),
                    line: hex(0xe5534b),
                },
                error: ChunkColor {
                    fill: hex(0xfff8c5),
                    line: hex(0xd4a72c),
                },
                current_chunk: rgba(255, 255, 255, 0.5),
            },
            widget_background: hex(0xf6f5f4),
            border: hex(0xcdc7c2),
            map_base: hex(0xffffff),
            map_text: hex(0x000000),
            button_hover: rgba(0, 0, 0, 0.1),
            button_icon: hex(0x2e3436),
            scrollbar: rgba(0, 0, 0, 0.3),
            syntax: lens_syntax(&p),
        }
    }

    /// Dark preset (Meld Lens Dark scheme, Adwaita dark widget colours).
    pub fn dark() -> Self {
        let p = LensPalette {
            comment: hex(0x6a9955),
            doc_comment_element: hex(0x8fbf7a),
            keyword: hex(0x569cd6),
            control: hex(0xc586c0),
            string: hex(0xce9178),
            number: hex(0xb5cea8),
            ty: hex(0x4ec9b0),
            function: hex(0xdcdcaa),
            variable: hex(0x9cdcfe),
            constant: hex(0x4fc1ff),
            escape: hex(0xd7ba7d),
            error: hex(0xf44747),
            warning: hex(0xcca700),
            note: hex(0x3794ff),
            foreground: hex(0xd4d4d4),
            gutter_fg: hex(0x858585),
            heading: hex(0x569cd6),
            list_marker: hex(0x569cd6),
            added_fg: hex(0x7ee787),
            removed_fg: hex(0xffa198),
        };
        Self {
            text: hex(0xd4d4d4),
            background: hex(0x1e1e1e),
            line_number: hex(0x858585),
            line_number_current: hex(0xc6c6c6),
            gutter_background: hex(0x242424),
            current_line: rgba(255, 255, 255, 0.06),
            selection: hex(0x264f78),
            selection_unfocused: hex(0x3a3d41),
            cursor: hex(0xaeafad),
            whitespace: hex(0x404040),
            sync_point: hex(0xbbbbbb),
            overscroll: rgba(255, 255, 255, 0.05),
            inline: hex(0x1f4f80),
            chunks: ChunkColors {
                insert: ChunkColor {
                    fill: hex(0x173d22),
                    line: hex(0x3fb950),
                },
                delete: ChunkColor {
                    fill: hex(0x4a1d1f),
                    line: hex(0xf85149),
                },
                replace: ChunkColor {
                    fill: hex(0x12304f),
                    line: hex(0x388bfd),
                },
                conflict: ChunkColor {
                    fill: hex(0x67060c),
                    line: hex(0xda3633),
                },
                error: ChunkColor {
                    fill: hex(0x4b3a05),
                    line: hex(0x9e6a03),
                },
                current_chunk: rgba(255, 255, 255, 0.06),
            },
            widget_background: hex(0x242424),
            border: hex(0x1b1b1b),
            map_base: hex(0x1e1e1e),
            map_text: hex(0xffffff),
            button_hover: rgba(255, 255, 255, 0.12),
            button_icon: hex(0xeeeeec),
            scrollbar: rgba(255, 255, 255, 0.3),
            syntax: lens_syntax(&p),
        }
    }

    /// Meld's classic chunk colours (`meld-base` scheme) on the light
    /// preset; delete uses the insert colours as in `get_common_theme()`.
    pub fn meld_classic() -> Self {
        let mut style = Self::light();
        let insert = ChunkColor {
            fill: hex(0xd0ffa3),
            line: hex(0xa5ff4c),
        };
        style.chunks = ChunkColors {
            insert,
            delete: insert,
            replace: ChunkColor {
                fill: hex(0xbdddff),
                line: hex(0x65b2ff),
            },
            conflict: ChunkColor {
                fill: hex(0xffa5a3),
                line: hex(0xff4f4c),
            },
            error: ChunkColor {
                fill: hex(0xfce94f),
                line: hex(0xfade0a),
            },
            current_chunk: rgba(255, 255, 255, 0.5),
        };
        style.inline = hex(0x8ac2ff);
        style.current_line = rgba(255, 255, 0, 0.25);
        style
    }
}

impl PaneStyle {
    /// A copy for a pane on the "new" side of a two-way comparison:
    /// insert and delete colours swap, so lines only present in this pane
    /// use the insert ("added") colours. Meld does the same through
    /// `get_common_theme(side)`.
    pub fn for_new_side(&self) -> Self {
        Self {
            chunks: self.chunks.swapped(),
            ..self.clone()
        }
    }
}

impl Default for PaneStyle {
    fn default() -> Self {
        Self::light()
    }
}

struct LensPalette {
    comment: Color,
    doc_comment_element: Color,
    keyword: Color,
    control: Color,
    string: Color,
    number: Color,
    ty: Color,
    function: Color,
    variable: Color,
    constant: Color,
    escape: Color,
    error: Color,
    warning: Color,
    note: Color,
    foreground: Color,
    gutter_fg: Color,
    heading: Color,
    list_marker: Color,
    added_fg: Color,
    removed_fg: Color,
}

fn lens_syntax(p: &LensPalette) -> HashMap<String, TextStyle> {
    let fg = TextStyle::fg;
    let entries = [
        ("def:comment", fg(p.comment)),
        ("def:shebang", fg(p.comment).bold()),
        ("def:doc-comment", fg(p.comment)),
        ("def:doc-comment-element", fg(p.doc_comment_element).bold()),
        ("def:constant", fg(p.constant)),
        ("def:special-constant", fg(p.keyword)),
        ("def:boolean", fg(p.keyword)),
        ("def:string", fg(p.string)),
        ("def:character", fg(p.string)),
        ("def:special-char", fg(p.escape)),
        ("def:number", fg(p.number)),
        ("def:floating-point", fg(p.number)),
        ("def:decimal", fg(p.number)),
        ("def:base-n-integer", fg(p.number)),
        ("def:identifier", fg(p.variable)),
        ("def:variable", fg(p.variable)),
        ("def:function", fg(p.function)),
        ("def:builtin", fg(p.ty)),
        ("def:type", fg(p.ty)),
        ("def:statement", fg(p.control)),
        ("def:keyword", fg(p.keyword)),
        ("def:reserved", fg(p.keyword)),
        ("def:operator", fg(p.foreground)),
        ("def:preprocessor", fg(p.control)),
        ("def:error", fg(p.error).underline()),
        ("def:warning", fg(p.warning)),
        ("def:note", fg(p.note).bold()),
        ("def:net-address", fg(p.constant).underline()),
        ("def:underlined", TextStyle::default().underline()),
        ("def:emphasis", TextStyle::default().italic()),
        ("def:strong-emphasis", TextStyle::default().bold()),
        ("def:heading", fg(p.heading).bold()),
        ("def:inline-code", fg(p.string)),
        ("def:preformatted-section", fg(p.string)),
        ("def:link-text", fg(p.constant)),
        ("def:link-symbol", fg(p.gutter_fg)),
        ("def:link-destination", fg(p.constant).underline()),
        ("def:list-marker", fg(p.list_marker).bold()),
        ("def:blockquote-marker", fg(p.comment)),
        ("def:thematic-break", fg(p.gutter_fg)),
        ("def:insertion", fg(p.added_fg)),
        ("def:deletion", fg(p.removed_fg).strikethrough()),
    ];
    entries
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
}

/// Builds an opaque colour from `0xRRGGBB`.
pub fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba8(r, g, b, a)
}

/// Meld's `alpha_tint`: composites `overlay` at `alpha` onto `base`.
pub fn alpha_tint(base: Color, overlay: Color, alpha: f32) -> Color {
    let mix = |b: f32, o: f32| b + (o - b) * alpha;
    Color {
        r: mix(base.r, overlay.r),
        g: mix(base.g, overlay.g),
        b: mix(base.b, overlay.b),
        a: base.a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_lookup() {
        let mut style = PaneStyle::light();
        style.syntax.clear();
        style
            .syntax
            .insert("def:statement".into(), TextStyle::fg(Color::BLACK));
        assert_eq!(
            style.syntax_style("def:keyword"),
            Some(TextStyle::fg(Color::BLACK))
        );
        assert_eq!(style.syntax_style("def:type"), None);
    }
}
