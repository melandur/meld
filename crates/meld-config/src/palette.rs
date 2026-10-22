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
///
/// A two-way comparison is coloured in one of two ways:
///
/// - [`ColorModel::BySide`] (`red-green` and `colorblind`, as in GitHub's
///   split view): every change in the old pane is coloured as removed
///   text and every change in the new pane as added text, so a replaced
///   chunk is, e.g., red on the left and green on the right.
/// - [`ColorModel::ByChange`] (all other palettes): a chunk has one colour
///   wherever it is drawn (both panes, the link map, the action gutters
///   and the overview maps). Lines only in the old file are removed,
///   lines only in the new file are added and replaced lines are changed.
///
/// `scheme`, Meld's default, uses the scheme's `meld:insert` colours for
/// both inserted and deleted lines, so it looks the same in both panes
/// too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffColorMode {
    /// Use the style scheme's own `meld:*` colours.
    #[default]
    Scheme,
    /// The style scheme's colours, with lines only in the old file in its
    /// `meld:delete` colours instead of `meld:insert`.
    SchemeChanges,
    /// Red for removed and green for added text, like GitHub or GitLens.
    RedGreen,
    /// Orange for removed and blue for added text.
    Colorblind,
    /// Red removed, green added and blue changed lines.
    RedGreenBlue,
    /// Orange removed, blue added and teal changed lines (Okabe-Ito).
    BlueOrange,
    /// Purple removed, green added and slate changed lines.
    PurpleGreen,
    /// Saturated fills and strong outlines.
    HighContrast,
}

/// How a palette colours the two panes of a comparison; see
/// [`DiffColorMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorModel {
    /// Old pane all removed, new pane all added.
    BySide,
    /// Removed, added or changed, the same in every pane and widget.
    ByChange,
}

impl DiffColorMode {
    /// Every mode, in the order the Preferences dialog lists them.
    pub const ALL: [DiffColorMode; 8] = [
        Self::Scheme,
        Self::SchemeChanges,
        Self::RedGreenBlue,
        Self::BlueOrange,
        Self::PurpleGreen,
        Self::HighContrast,
        Self::RedGreen,
        Self::Colorblind,
    ];

    /// The setting's string value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheme => "scheme",
            Self::SchemeChanges => "scheme-changes",
            Self::RedGreen => "red-green",
            Self::Colorblind => "colorblind",
            Self::RedGreenBlue => "red-green-blue",
            Self::BlueOrange => "blue-orange",
            Self::PurpleGreen => "purple-green",
            Self::HighContrast => "high-contrast",
        }
    }

    /// How the mode colours the panes of a two-way comparison.
    pub fn model(self) -> ColorModel {
        match self {
            Self::RedGreen | Self::Colorblind => ColorModel::BySide,
            _ => ColorModel::ByChange,
        }
    }

    /// A one-line explanation for the Preferences dialog.
    pub fn description(self) -> &'static str {
        match self {
            Self::Scheme => "The color scheme's insert and change colors",
            Self::SchemeChanges => "The color scheme's colors; removed lines in its delete color",
            Self::RedGreen => "Old pane red, new pane green",
            Self::Colorblind => "Old pane orange, new pane blue",
            Self::RedGreenBlue => "Removed red, added green, changed blue",
            Self::BlueOrange => "Removed orange, added blue, changed teal",
            Self::PurpleGreen => "Removed purple, added green, changed slate",
            Self::HighContrast => "Strong fills and outlines",
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
            Self::SchemeChanges => "Color Scheme, Removed and Added",
            Self::RedGreen => "Red and Green by Side",
            Self::Colorblind => "Blue and Orange by Side (Colorblind Safe)",
            Self::RedGreenBlue => "Red, Green and Blue",
            Self::BlueOrange => "Orange, Blue and Teal (Colorblind Safe)",
            Self::PurpleGreen => "Purple, Green and Slate (Red-Green Safe)",
            Self::HighContrast => "High Contrast",
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

// By-change palettes. Their changed tone has to stand apart from both
// removed and added text, also for the colour vision deficiencies a
// palette claims to support.
const BLUE_ORANGE_LIGHT: ToneSet = ToneSet {
    removed: COLORBLIND_LIGHT.removed,
    added: COLORBLIND_LIGHT.added,
    // Okabe-Ito bluish green
    neutral: tone("#e0f5ef", "#009e73", "#a8e3d2"),
};
const BLUE_ORANGE_DARK: ToneSet = ToneSet {
    removed: COLORBLIND_DARK.removed,
    added: COLORBLIND_DARK.added,
    neutral: tone("#0f3a30", "#009e73", "#155c4c"),
};
// Purple keeps its blue component, so it stays apart from green for
// red-green deficiencies; changed lines are a low-saturation slate.
const PURPLE_GREEN_LIGHT: ToneSet = ToneSet {
    removed: tone("#f8efff", "#8250df", "#e2c6ff"),
    added: tone("#e6ffec", "#1a7f37", "#abf2bc"),
    neutral: tone("#eef1f4", "#6e7781", "#d0d7de"),
};
const PURPLE_GREEN_DARK: ToneSet = ToneSet {
    removed: tone("#3b2152", "#a371f7", "#5e3a8a"),
    added: tone("#173d22", "#3fb950", "#1b5e2e"),
    neutral: tone("#2d333b", "#768390", "#4a5560"),
};
// Saturated fills that keep at least 7:1 contrast with black (light) or
// white (dark) text, with strong outlines.
const HIGH_CONTRAST_LIGHT: ToneSet = ToneSet {
    removed: tone("#ffd7d7", "#c00000", "#ff9e9e"),
    added: tone("#c8f7c8", "#006400", "#7ee07e"),
    neutral: tone("#cfe6ff", "#0040c0", "#8cc2ff"),
};
const HIGH_CONTRAST_DARK: ToneSet = ToneSet {
    removed: tone("#4d0000", "#ff6b6b", "#8a1c1c"),
    added: tone("#003300", "#4cd64c", "#0f660f"),
    neutral: tone("#002147", "#4da3ff", "#0b4f99"),
};

/// The removed, added and changed tones of a palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiffTones {
    pub removed: DiffTone,
    pub added: DiffTone,
    pub changed: DiffTone,
}

impl DiffTones {
    /// Chunk colours for a pane on `side`, following `model`.
    ///
    /// Meld's insert and delete tags are relative to the pane a chunk is
    /// drawn in: a delete is lines only this pane has, an insert is lines
    /// only the other pane has. So by change, a pane on the old side
    /// shows deletes as removed and inserts as added text, and a pane on
    /// the new side the other way round. Side-less panes and widgets use
    /// the old side's orientation, as Meld's neutral palette does.
    pub fn palette(&self, model: ColorModel, side: Option<DiffSide>) -> DiffPalette {
        let all = |tone: DiffTone| DiffPalette {
            insert: tone,
            delete: tone,
            replace: tone,
        };
        match (model, side) {
            (ColorModel::BySide, Some(DiffSide::Old)) => all(self.removed),
            (ColorModel::BySide, Some(DiffSide::New)) => all(self.added),
            (ColorModel::ByChange, Some(DiffSide::New)) => DiffPalette {
                insert: self.removed,
                delete: self.added,
                replace: self.changed,
            },
            _ => DiffPalette {
                insert: self.added,
                delete: self.removed,
                replace: self.changed,
            },
        }
    }
}

/// The tones of a palette, or `None` for the scheme modes.
pub fn diff_tones(mode: DiffColorMode, dark: bool) -> Option<DiffTones> {
    let tones = match (mode, dark) {
        (DiffColorMode::Scheme | DiffColorMode::SchemeChanges, _) => return None,
        (DiffColorMode::RedGreen | DiffColorMode::RedGreenBlue, false) => &RED_GREEN_LIGHT,
        (DiffColorMode::RedGreen | DiffColorMode::RedGreenBlue, true) => &RED_GREEN_DARK,
        (DiffColorMode::Colorblind, false) => &COLORBLIND_LIGHT,
        (DiffColorMode::Colorblind, true) => &COLORBLIND_DARK,
        (DiffColorMode::BlueOrange, false) => &BLUE_ORANGE_LIGHT,
        (DiffColorMode::BlueOrange, true) => &BLUE_ORANGE_DARK,
        (DiffColorMode::PurpleGreen, false) => &PURPLE_GREEN_LIGHT,
        (DiffColorMode::PurpleGreen, true) => &PURPLE_GREEN_DARK,
        (DiffColorMode::HighContrast, false) => &HIGH_CONTRAST_LIGHT,
        (DiffColorMode::HighContrast, true) => &HIGH_CONTRAST_DARK,
    };
    Some(DiffTones {
        removed: tones.removed,
        added: tones.added,
        changed: tones.neutral,
    })
}

/// Get the chunk colours for a palette, or `None` for scheme colours.
///
/// `side` is the [`DiffSide`] of the pane being coloured, or `None` for
/// panes and widgets that don't belong to one side of a two-way
/// comparison (three-way comparisons, link maps).
///
/// With the by-side palettes (`red-green`, `colorblind`), panes on the old
/// side colour every change as removed text, and panes on the new side as
/// added text. Side-less widgets get a neutral variant, where inserts are
/// added text, deletes are removed text and replacements get a neutral
/// colour. Note that Meld's insert and delete tags are relative to the
/// pane a chunk is viewed from, so callers drawing chunks from the new
/// side's perspective need to swap insert and delete.
///
/// The by-change palettes swap insert and delete themselves for panes on
/// the new side, so that a chunk has the same colour in both panes; see
/// [`DiffTones::palette`].
pub fn get_diff_palette(
    mode: DiffColorMode,
    dark: bool,
    side: Option<DiffSide>,
) -> Option<DiffPalette> {
    diff_tones(mode, dark).map(|tones| tones.palette(mode.model(), side))
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

    const BY_CHANGE: [DiffColorMode; 4] = [
        DiffColorMode::RedGreenBlue,
        DiffColorMode::BlueOrange,
        DiffColorMode::PurpleGreen,
        DiffColorMode::HighContrast,
    ];

    #[test]
    fn models() {
        for mode in DiffColorMode::ALL {
            let expected = match mode {
                DiffColorMode::RedGreen | DiffColorMode::Colorblind => ColorModel::BySide,
                _ => ColorModel::ByChange,
            };
            assert_eq!(mode.model(), expected, "{mode:?}");
            assert_eq!(DiffColorMode::parse(mode.as_str()), mode);
            assert!(!mode.label().is_empty() && !mode.description().is_empty());
        }
        for dark in [false, true] {
            for mode in [DiffColorMode::Scheme, DiffColorMode::SchemeChanges] {
                assert_eq!(diff_tones(mode, dark), None);
            }
        }
    }

    /// A chunk drawn in the left (old) pane, the right (new) pane and
    /// the link map between them gets the same colours with every
    /// by-change palette. Lines only in the left file are a delete in the
    /// left pane and the link map but an insert in the right pane, and
    /// the other way round.
    #[test]
    fn by_change_palettes_match_across_panes() {
        for mode in BY_CHANGE {
            for dark in [false, true] {
                let tones = diff_tones(mode, dark).unwrap();
                let left = get_diff_palette(mode, dark, Some(DiffSide::Old)).unwrap();
                let right = get_diff_palette(mode, dark, Some(DiffSide::New)).unwrap();
                let link = get_diff_palette(mode, dark, None).unwrap();

                // Removed: only in the left file
                assert_eq!(left.delete, tones.removed);
                assert_eq!(right.insert, tones.removed);
                assert_eq!(link.delete, tones.removed);
                // Added: only in the right file
                assert_eq!(left.insert, tones.added);
                assert_eq!(right.delete, tones.added);
                assert_eq!(link.insert, tones.added);
                // Changed
                for p in [left, right, link] {
                    assert_eq!(p.replace, tones.changed);
                    assert_eq!(p.inline(), tones.changed.inline);
                }
            }
        }
    }

    /// Meld's by-side palettes colour each pane in one tone, so the same
    /// replaced chunk differs between panes (as in meld/diffpalette.py).
    #[test]
    fn by_side_palettes_colour_whole_panes() {
        for mode in [DiffColorMode::RedGreen, DiffColorMode::Colorblind] {
            for dark in [false, true] {
                let tones = diff_tones(mode, dark).unwrap();
                let left = get_diff_palette(mode, dark, Some(DiffSide::Old)).unwrap();
                let right = get_diff_palette(mode, dark, Some(DiffSide::New)).unwrap();
                assert_ne!(left.replace, right.replace);
                for tag in PaletteTag::ALL {
                    assert_eq!(left.tone(tag), tones.removed);
                    assert_eq!(right.tone(tag), tones.added);
                }
            }
        }
    }

    /// WCAG relative luminance and contrast ratio.
    fn wcag_luminance(c: Rgba) -> f32 {
        let lin = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (la, lb) = (wcag_luminance(a), wcag_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// Text stays readable on every fill and inline highlight: at least
    /// 4.5:1 (WCAG AA) against typical scheme text, with fills at 7:1
    /// (AAA) for the high-contrast palette. (Meld's own dark red-green
    /// inline green is just under AA, at 4.47:1.)
    #[test]
    fn fills_keep_text_readable() {
        for mode in DiffColorMode::ALL {
            for dark in [false, true] {
                let Some(tones) = diff_tones(mode, dark) else {
                    continue;
                };
                let text = if dark {
                    rgba("#d4d4d4")
                } else {
                    rgba("#1f1f1f")
                };
                let fill_min = if mode == DiffColorMode::HighContrast {
                    7.0
                } else {
                    4.5
                };
                let inline_min = match mode {
                    DiffColorMode::RedGreen | DiffColorMode::RedGreenBlue => 4.4,
                    _ => 4.5,
                };
                for tone in [tones.removed, tones.added, tones.changed] {
                    for (spec, min) in [(tone.fill, fill_min), (tone.inline, inline_min)] {
                        let ratio = contrast(text, rgba(spec));
                        assert!(ratio >= min, "{mode:?} dark={dark} {spec}: {ratio}");
                    }
                    // Outlines stand out from the fill
                    let ratio = contrast(rgba(tone.line), rgba(tone.fill));
                    assert!(ratio >= 1.8, "{mode:?} dark={dark} {}: {ratio}", tone.line);
                }
            }
        }
    }

    /// Simulated colour vision deficiency (Viénot, Brettel and Mollon
    /// 1999 for protanopia and deuteranopia, in linear RGB).
    fn simulate(c: Rgba, m: [[f32; 3]; 3]) -> [f32; 3] {
        let lin = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let gamma = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            if v <= 0.0031308 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            }
        };
        let rgb = [lin(c.r), lin(c.g), lin(c.b)];
        let row = |r: [f32; 3]| gamma(r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2]);
        [row(m[0]), row(m[1]), row(m[2])]
    }

    const PROTANOPIA: [[f32; 3]; 3] = [
        [0.11238, 0.88762, 0.0],
        [0.11238, 0.88762, 0.0],
        [0.00401, -0.00401, 1.0],
    ];
    const DEUTERANOPIA: [[f32; 3]; 3] = [
        [0.29275, 0.70725, 0.0],
        [0.29275, 0.70725, 0.0],
        [-0.02234, 0.02234, 1.0],
    ];

    fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y) * (x - y))
            .sum::<f32>()
            .sqrt()
    }

    /// Palettes that claim to suit colour vision deficiencies keep their
    /// removed, added and changed outlines and inline highlights apart
    /// under simulated protanopia and deuteranopia. (Meld's by-side
    /// `colorblind` palette only shows its grey changed tone in link maps;
    /// its two panes are orange and blue.)
    #[test]
    fn colorblind_safe_palettes_stay_distinct() {
        for mode in [
            DiffColorMode::Colorblind,
            DiffColorMode::BlueOrange,
            DiffColorMode::PurpleGreen,
        ] {
            for dark in [false, true] {
                let t = diff_tones(mode, dark).unwrap();
                let tones: &[DiffTone] = match mode.model() {
                    ColorModel::BySide => &[t.removed, t.added],
                    ColorModel::ByChange => &[t.removed, t.added, t.changed],
                };
                for cvd in [PROTANOPIA, DEUTERANOPIA] {
                    for (i, a) in tones.iter().enumerate() {
                        for b in &tones[i + 1..] {
                            let line =
                                distance(simulate(rgba(a.line), cvd), simulate(rgba(b.line), cvd));
                            assert!(line > 0.2, "{mode:?} {dark} {} {}: {line}", a.line, b.line);
                            let fill = distance(
                                simulate(rgba(a.inline), cvd),
                                simulate(rgba(b.inline), cvd),
                            );
                            assert!(
                                fill > 0.04,
                                "{mode:?} {dark} {} {}: {fill}",
                                a.inline,
                                b.inline
                            );
                        }
                    }
                }
            }
        }
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
