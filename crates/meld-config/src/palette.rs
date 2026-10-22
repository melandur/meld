//! Diff colour palettes that override the style scheme's chunk colours,
//! ported from `meld/diffpalette.py`.
//!
//! Meld's chunk colours normally come from the `meld:*` styles of the
//! current style scheme. The palettes here replace the insert, delete and
//! replace colours (plus the inline highlight) with fixed sets so that
//! comparisons can look like other diff tools.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Rgba;

/// Values of the `diff-colors` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffColorMode {
    /// Use the style scheme's own `meld:*` colours.
    #[default]
    Scheme,
    /// Red for removed and green for added text, like GitHub or GitLens.
    RedGreen,
    /// Orange for removed and blue for added text.
    Colorblind,
}

impl DiffColorMode {
    pub const ALL: [DiffColorMode; 3] = [Self::Scheme, Self::RedGreen, Self::Colorblind];

    /// The setting's string value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheme => "scheme",
            Self::RedGreen => "red-green",
            Self::Colorblind => "colorblind",
        }
    }

    /// Convert a setting value to a palette, defaulting to the scheme
    /// (`parse_diff_colours`).
    pub fn parse(value: &str) -> DiffColorMode {
        Self::ALL
            .into_iter()
            .find(|m| m.as_str() == value)
            .unwrap_or(Self::Scheme)
    }

    /// The label the Preferences dialog shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Scheme => "From Color Scheme",
            Self::RedGreen => "Red and Green",
            Self::Colorblind => "Blue and Orange (Colorblind Safe)",
        }
    }
}

impl FromStr for DiffColorMode {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

/// Which side of a two-way comparison a pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffSide {
    Old,
    New,
}

impl DiffSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }

    /// Convert a side name to a side, with anything unknown as no side
    /// (`parse_diff_side`).
    pub fn parse(value: &str) -> Option<DiffSide> {
        match value {
            "old" => Some(Self::Old),
            "new" => Some(Self::New),
            _ => None,
        }
    }
}

/// Diff sides of the (up to three) panes of a file comparison
/// (`FileDiff._update_diff_sides`).
///
/// Two-way comparisons have the left pane old and the right pane new,
/// unless `meta_sides` (a comparison's "diff-sides" meta entry) is a
/// permutation of old and new. One- and three-way comparisons have no
/// sides.
pub fn diff_sides(num_panes: usize, meta_sides: Option<&[DiffSide]>) -> [Option<DiffSide>; 3] {
    if num_panes != 2 {
        return [None, None, None];
    }
    if let Some(&[a, b]) = meta_sides
        && a != b
    {
        return [Some(a), Some(b), None];
    }
    [Some(DiffSide::Old), Some(DiffSide::New), None]
}

/// Colours for one kind of change, as `#rrggbb` strings.
///
/// The fill is the chunk background, the line is the chunk outline (and
/// the colour used for zero-height chunks) and the inline colour
/// highlights changed words within a replaced chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiffTone {
    pub fill: &'static str,
    pub line: &'static str,
    pub inline: &'static str,
}

impl DiffTone {
    pub fn fill_rgba(&self) -> Rgba {
        Rgba::parse(self.fill).unwrap_or(Rgba::TRANSPARENT)
    }
    pub fn line_rgba(&self) -> Rgba {
        Rgba::parse(self.line).unwrap_or(Rgba::TRANSPARENT)
    }
    pub fn inline_rgba(&self) -> Rgba {
        Rgba::parse(self.inline).unwrap_or(Rgba::TRANSPARENT)
    }
}

/// Chunk tags whose colours a palette overrides; conflict and error
/// colours always come from the style scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteTag {
    Insert,
    Delete,
    Replace,
}

impl PaletteTag {
    pub const ALL: [PaletteTag; 3] = [Self::Insert, Self::Delete, Self::Replace];
}

/// Chunk colours for a single pane or side-less widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiffPalette {
    pub insert: DiffTone,
    pub delete: DiffTone,
    pub replace: DiffTone,
}

impl DiffPalette {
    /// Inline highlighting only ever happens within replace chunks.
    pub fn inline(&self) -> &'static str {
        self.replace.inline
    }

    pub fn tone(&self, tag: PaletteTag) -> DiffTone {
        match tag {
            PaletteTag::Insert => self.insert,
            PaletteTag::Delete => self.delete,
            PaletteTag::Replace => self.replace,
        }
    }
}

struct ToneSet {
    removed: DiffTone,
    added: DiffTone,
    neutral: DiffTone,
}

const fn tone(fill: &'static str, line: &'static str, inline: &'static str) -> DiffTone {
    DiffTone { fill, line, inline }
}

// Light colours follow GitHub's diff view; dark colours are opaque
// equivalents that keep enough contrast against dark editor backgrounds,
// since chunk fills are drawn underneath the text.
const RED_GREEN_LIGHT: ToneSet = ToneSet {
    removed: tone("#ffebe9", "#ff8182", "#ffc0bd"),
    added: tone("#e6ffec", "#4ac26b", "#abf2bc"),
    neutral: tone("#ddf4ff", "#54aeff", "#b6e3ff"),
};
const RED_GREEN_DARK: ToneSet = ToneSet {
    removed: tone("#4a1d1f", "#f85149", "#7d2a2c"),
    added: tone("#173d22", "#3fb950", "#1f6a33"),
    neutral: tone("#12304f", "#388bfd", "#1f4f80"),
};
// Colours from the Okabe-Ito palette, which stays distinguishable for the
// common forms of colour vision deficiency. The neutral tone is a
// low-saturation grey so that it can't be confused with either side.
const COLORBLIND_LIGHT: ToneSet = ToneSet {
    removed: tone("#fff1e0", "#e69f00", "#ffd29e"),
    added: tone("#e3f0fb", "#0072b2", "#b3d7f2"),
    neutral: tone("#ebe8f2", "#8e86a8", "#d3cde3"),
};
const COLORBLIND_DARK: ToneSet = ToneSet {
    removed: tone("#4a3110", "#e69f00", "#7a4f12"),
    added: tone("#0f3550", "#56b4e9", "#1a5a85"),
    neutral: tone("#34313d", "#8e86a8", "#4d4860"),
};

/// Get the chunk colours for a palette, or `None` for scheme colours.
///
/// `side` is the [`DiffSide`] of the pane being coloured, or `None` for
/// panes and widgets that don't belong to one side of a two-way
/// comparison (three-way comparisons, link maps).
///
/// Panes on the old side colour every change as removed text, and panes
/// on the new side as added text. Side-less widgets get a neutral
/// variant, where inserts are added text, deletes are removed text and
/// replacements get a neutral colour. Note that Meld's insert and delete
/// tags are relative to the pane a chunk is viewed from, so callers
/// drawing chunks from the new side's perspective need to swap insert and
/// delete.
pub fn get_diff_palette(
    mode: DiffColorMode,
    dark: bool,
    side: Option<DiffSide>,
) -> Option<DiffPalette> {
    let tones = match (mode, dark) {
        (DiffColorMode::Scheme, _) => return None,
        (DiffColorMode::RedGreen, false) => &RED_GREEN_LIGHT,
        (DiffColorMode::RedGreen, true) => &RED_GREEN_DARK,
        (DiffColorMode::Colorblind, false) => &COLORBLIND_LIGHT,
        (DiffColorMode::Colorblind, true) => &COLORBLIND_DARK,
    };
    let all = |tone: DiffTone| DiffPalette {
        insert: tone,
        delete: tone,
        replace: tone,
    };
    Some(match side {
        Some(DiffSide::Old) => all(tones.removed),
        Some(DiffSide::New) => all(tones.added),
        None => DiffPalette {
            insert: tones.added,
            delete: tones.removed,
            replace: tones.neutral,
        },
    })
}

#[cfg(test)]
mod tests {
    //! Port of test/test_diffpalette.py's palette tests.
    use super::*;

    const PALETTES: [DiffColorMode; 2] = [DiffColorMode::RedGreen, DiffColorMode::Colorblind];
    const SIDES: [Option<DiffSide>; 3] = [None, Some(DiffSide::Old), Some(DiffSide::New)];

    fn rgba(spec: &str) -> Rgba {
        Rgba::parse(spec).unwrap_or_else(|| panic!("Invalid colour {spec}"))
    }

    fn luminance(spec: &str) -> f32 {
        rgba(spec).luminance()
    }

    #[test]
    fn palette_selection() {
        for palette in PALETTES {
            for dark in [false, true] {
                for side in SIDES {
                    let result = get_diff_palette(palette, dark, side).unwrap();
                    let removed = get_diff_palette(palette, dark, Some(DiffSide::Old))
                        .unwrap()
                        .insert;
                    let added = get_diff_palette(palette, dark, Some(DiffSide::New))
                        .unwrap()
                        .insert;

                    match side {
                        Some(DiffSide::Old) => {
                            assert!(
                                result.insert == removed
                                    && result.delete == removed
                                    && result.replace == removed
                            )
                        }
                        Some(DiffSide::New) => {
                            assert!(
                                result.insert == added
                                    && result.delete == added
                                    && result.replace == added
                            )
                        }
                        None => {
                            assert_eq!(result.insert, added);
                            assert_eq!(result.delete, removed);
                            assert!(result.replace != added && result.replace != removed);
                        }
                    }
                    assert_ne!(removed, added);
                    assert_eq!(result.inline(), result.replace.inline);

                    for tag in PaletteTag::ALL {
                        let tone = result.tone(tag);
                        for spec in [tone.fill, tone.line, tone.inline] {
                            rgba(spec);
                        }
                        // Inline highlights need to stand out from their chunk's fill
                        assert_ne!(tone.inline, tone.fill);
                        if dark {
                            assert!(luminance(tone.fill) < 0.3);
                            assert!(luminance(tone.inline) > luminance(tone.fill));
                        } else {
                            assert!(luminance(tone.fill) > 0.8);
                            assert!(luminance(tone.inline) < luminance(tone.fill));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn palette_hues() {
        for dark in [false, true] {
            let red_green = get_diff_palette(DiffColorMode::RedGreen, dark, None).unwrap();
            let c = rgba(red_green.delete.line);
            assert!(c.r > c.g && c.r > c.b);
            let c = rgba(red_green.insert.line);
            assert!(c.g > c.r && c.g > c.b);

            let colorblind = get_diff_palette(DiffColorMode::Colorblind, dark, None).unwrap();
            let c = rgba(colorblind.delete.line);
            assert!(c.r > c.b, "Removed text should be orange");
            let c = rgba(colorblind.insert.line);
            assert!(c.b > c.r, "Added text should be blue");
        }
    }

    #[test]
    fn known_palette_colours() {
        let light_old = get_diff_palette(
            DiffColorMode::parse("red-green"),
            false,
            DiffSide::parse("old"),
        )
        .unwrap();
        assert_eq!(light_old.insert.fill, "#ffebe9");
        let light_new = get_diff_palette(
            DiffColorMode::parse("red-green"),
            false,
            DiffSide::parse("new"),
        )
        .unwrap();
        assert_eq!(light_new.insert.fill, "#e6ffec");
    }

    #[test]
    fn scheme_and_unknown_palettes_use_scheme() {
        for value in ["scheme", "unknown", ""] {
            for dark in [false, true] {
                for side in SIDES {
                    assert_eq!(
                        get_diff_palette(DiffColorMode::parse(value), dark, side),
                        None
                    );
                }
            }
        }
    }

    #[test]
    fn palette_values_match_settings_schema() {
        let text = include_str!("../../../data/org.gnome.Meld.gschema.xml");
        for value in DiffColorMode::ALL {
            assert!(text.contains(&format!("nick=\"{}\"", value.as_str())));
        }
    }

    #[test]
    fn parse_diff_colours() {
        assert_eq!(DiffColorMode::parse("scheme"), DiffColorMode::Scheme);
        assert_eq!(DiffColorMode::parse("red-green"), DiffColorMode::RedGreen);
        assert_eq!(
            DiffColorMode::parse("colorblind"),
            DiffColorMode::Colorblind
        );
        assert_eq!(DiffColorMode::parse(""), DiffColorMode::Scheme);
        assert_eq!(DiffColorMode::parse("['red-green']"), DiffColorMode::Scheme);
        assert_eq!(
            "colorblind".parse::<DiffColorMode>(),
            Ok(DiffColorMode::Colorblind)
        );
    }

    #[test]
    fn parse_diff_side() {
        assert_eq!(DiffSide::parse("old"), Some(DiffSide::Old));
        assert_eq!(DiffSide::parse("new"), Some(DiffSide::New));
        assert_eq!(DiffSide::parse(""), None);
        assert_eq!(DiffSide::parse("left"), None);
    }

    /// Port of test_filediff_diff_sides.
    #[test]
    fn filediff_diff_sides() {
        use DiffSide::{New, Old};
        let parse = |names: &[&str]| -> Vec<DiffSide> {
            names.iter().filter_map(|n| DiffSide::parse(n)).collect()
        };
        assert_eq!(diff_sides(1, None), [None, None, None]);
        assert_eq!(diff_sides(2, None), [Some(Old), Some(New), None]);
        assert_eq!(diff_sides(3, None), [None, None, None]);
        assert_eq!(
            diff_sides(2, Some(&parse(&["new", "old"]))),
            [Some(New), Some(Old), None]
        );
        // Invalid overrides are ignored
        assert_eq!(
            diff_sides(2, Some(&parse(&["new", "new"]))),
            [Some(Old), Some(New), None]
        );
        assert_eq!(
            diff_sides(2, Some(&parse(&["left"]))),
            [Some(Old), Some(New), None]
        );
        // Sides make no sense for three-way comparisons
        assert_eq!(
            diff_sides(3, Some(&parse(&["new", "old"]))),
            [None, None, None]
        );
    }
}
