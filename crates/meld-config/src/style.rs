//! Chunk and highlight colours derived from a style scheme, ported from
//! `meld/style.py`.
//!
//! A [`Theme`] is the pair of schemes `meld.style` keeps globally: the
//! user's (variant-adapted) scheme and Meld's base scheme (`meld-base` or
//! `meld-dark`, depending on whether the user's scheme is dark), which
//! supplies any `meld:*` colour the user's scheme lacks.

use crate::palette::{DiffColorMode, DiffSide, PaletteTag, get_diff_palette};
use crate::scheme::{SchemeManager, StyleAttr, StyleScheme};
use crate::{Rgba, Settings};

/// Meld's base schemes (`MeldStyleScheme`).
pub const BASE_SCHEME: &str = "meld-base";
pub const DARK_BASE_SCHEME: &str = "meld-dark";

/// The kinds of chunk Meld colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkTag {
    Insert,
    Delete,
    Replace,
    Conflict,
    Error,
}

/// Fill (chunk background) and line (chunk outline, and the colour of
/// zero-height chunks) colours of a chunk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkColors {
    pub fill: Rgba,
    pub line: Rgba,
}

/// All colours a file comparison pane or diff widget needs, resolved for
/// one scheme, `diff-colors` setting and pane side.
///
/// The chunk colours are `get_common_theme()`'s fill and line maps; the
/// rest are the individual `colour_lookup_with_fallback()` calls of
/// `sourceview.py`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffColors {
    pub insert: ChunkColors,
    /// Note that, as in Meld, deletes use the `meld:insert` colours unless
    /// a diff palette is active: a delete in one pane is an insert as seen
    /// from the other.
    pub delete: ChunkColors,
    pub replace: ChunkColors,
    pub conflict: ChunkColors,
    pub error: ChunkColors,
    /// Changed-word highlight inside replace chunks (`meld:inline`).
    pub inline: Rgba,
    /// `focus-highlight`: `meld:current-line-highlight` foreground.
    pub focus_highlight: Rgba,
    /// `meld:current-chunk-highlight` background.
    pub current_chunk_highlight: Rgba,
    /// `meld:overscroll` background.
    pub overscroll: Rgba,
    /// Current line highlight: `meld:current-line-highlight` background.
    pub current_line_highlight: Rgba,
    /// `meld:syncpoint-outline` foreground.
    pub syncpoint_outline: Rgba,
    /// Foreground of dimmed text (`meld:dimmed`).
    pub dimmed: Rgba,
}

impl DiffColors {
    pub fn chunk(&self, tag: ChunkTag) -> ChunkColors {
        match tag {
            ChunkTag::Insert => self.insert,
            ChunkTag::Delete => self.delete,
            ChunkTag::Replace => self.replace,
            ChunkTag::Conflict => self.conflict,
            ChunkTag::Error => self.error,
        }
    }

    /// The colours with the `diff-color-opacity` setting (in percent)
    /// applied; see [`diff_color_alphas`]. 100% leaves them unchanged.
    pub fn with_opacity(mut self, percent: i32) -> DiffColors {
        let (fill, line) = diff_color_alphas(percent);
        for chunk in [
            &mut self.insert,
            &mut self.delete,
            &mut self.replace,
            &mut self.conflict,
            &mut self.error,
        ] {
            chunk.fill.a *= fill;
            chunk.line.a *= line;
        }
        self.inline.a *= fill;
        self
    }

    fn chunk_mut(&mut self, tag: PaletteTag) -> &mut ChunkColors {
        match tag {
            PaletteTag::Insert => &mut self.insert,
            PaletteTag::Delete => &mut self.delete,
            PaletteTag::Replace => &mut self.replace,
        }
    }
}

/// Alpha factors for chunk fills (and inline highlights) and for chunk
/// outlines at a `diff-color-opacity` of `percent`, which is clamped to
/// [`crate::settings::DIFF_COLOR_OPACITY_RANGE`].
///
/// Fills scale with the opacity. Outlines fade only half as much, so
/// that chunk edges, zero-height chunks and the link map's curves stay
/// visible on faint fills.
pub fn diff_color_alphas(percent: i32) -> (f32, f32) {
    let (lo, hi) = crate::settings::DIFF_COLOR_OPACITY_RANGE;
    let opacity = percent.clamp(lo, hi) as f32 / 100.0;
    (opacity, 0.5 + 0.5 * opacity)
}

/// Foreground colours for file states in folder and version control
/// trees (`tree.py`'s `_setup_default_styles`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatusColors {
    /// Ignored, unversioned, empty and non-existent entries.
    pub unknown: Rgba,
    pub new: Rgba,
    /// Modified and renamed entries.
    pub modified: Rgba,
    /// Removed and missing entries.
    pub removed: Rgba,
    pub error: Rgba,
    pub conflict: Rgba,
}

/// The current scheme plus Meld's matching base scheme.
#[derive(Debug, Clone)]
pub struct Theme {
    scheme: StyleScheme,
    base: StyleScheme,
    dark: bool,
}

impl Theme {
    /// Resolve the theme for a `style-scheme` id, as
    /// `MeldSettings._style_scheme_from_gsettings` does: look the scheme
    /// up (falling back to "classic", then `meld-base`), switch to its
    /// light or dark variant to match the UI, then pick the base scheme.
    pub fn new(manager: &SchemeManager, scheme_id: &str, ui_dark: bool) -> Theme {
        let scheme = manager
            .get(scheme_id)
            .or_else(|| {
                log::warn!("Style scheme '{scheme_id}' not found; using classic");
                manager.get("classic")
            })
            .or_else(|| manager.get(BASE_SCHEME))
            .unwrap_or_else(|| bundled(BASE_SCHEME));
        let scheme = manager.adapt(scheme, ui_dark);
        Self::from_scheme(manager, scheme)
    }

    /// The theme for the scheme and UI variant in `settings`.
    pub fn from_settings(
        manager: &SchemeManager,
        settings: &Settings,
        system_prefers_dark: bool,
    ) -> Theme {
        Self::new(
            manager,
            &settings.style_scheme,
            settings.prefer_dark(system_prefers_dark),
        )
    }

    /// Use a scheme as is (`set_base_style_scheme`).
    ///
    /// If the scheme is one of Meld's base schemes, the base scheme
    /// matching its darkness replaces it.
    pub fn from_scheme(manager: &SchemeManager, scheme: StyleScheme) -> Theme {
        let dark = scheme.is_dark();
        let base_id = if dark { DARK_BASE_SCHEME } else { BASE_SCHEME };
        let base = manager.get(base_id).unwrap_or_else(|| bundled(base_id));
        let scheme = if [BASE_SCHEME, DARK_BASE_SCHEME].contains(&scheme.id()) {
            base.clone()
        } else {
            scheme
        };
        Theme { scheme, base, dark }
    }

    /// The scheme to highlight text with.
    pub fn scheme(&self) -> &StyleScheme {
        &self.scheme
    }

    /// Whether the scheme is dark (`_use_dark`).
    pub fn is_dark(&self) -> bool {
        self.dark
    }

    /// Look a colour up in the scheme, then in the base scheme.
    pub fn lookup(&self, name: &str, attribute: StyleAttr) -> Option<Rgba> {
        self.scheme
            .style(name)
            .and_then(|s| s.color(attribute))
            .or_else(|| self.base.style(name).and_then(|s| s.color(attribute)))
    }

    /// `colour_lookup_with_fallback`. Where Meld exits on a colour that
    /// even the base scheme lacks ("this is a bad install"), this logs an
    /// error and returns transparent.
    pub fn color(&self, name: &str, attribute: StyleAttr) -> Rgba {
        self.lookup(name, attribute).unwrap_or_else(|| {
            log::error!(
                "Couldn't find color scheme details for {name}-{attribute:?}; this is a bad install"
            );
            Rgba::TRANSPARENT
        })
    }

    /// Colours from the scheme alone, ignoring any diff palette.
    pub fn scheme_diff_colors(&self) -> DiffColors {
        use StyleAttr::{Background as Bg, Foreground as Fg, LineBackground as Line};
        let c = |name: &str, attr| self.color(name, attr);
        let chunk = |name: &str| ChunkColors {
            fill: c(name, Bg),
            line: c(name, Line),
        };
        DiffColors {
            insert: chunk("meld:insert"),
            delete: chunk("meld:insert"),
            replace: chunk("meld:replace"),
            conflict: chunk("meld:conflict"),
            error: chunk("meld:error"),
            inline: c("meld:inline", Bg),
            focus_highlight: c("meld:current-line-highlight", Fg),
            current_chunk_highlight: c("meld:current-chunk-highlight", Bg),
            overscroll: c("meld:overscroll", Bg),
            current_line_highlight: c("meld:current-line-highlight", Bg),
            syncpoint_outline: c("meld:syncpoint-outline", Fg),
            dimmed: c("meld:dimmed", Fg),
        }
    }

    /// Colours for a pane or widget (`get_common_theme` and
    /// `get_inline_colour`).
    ///
    /// With a `diff-colors` palette, insert, delete and replace colours
    /// and the inline colour come from the palette for `side`; see
    /// [`get_diff_palette`]. Everything else comes from the scheme.
    pub fn diff_colors(&self, mode: DiffColorMode, side: Option<DiffSide>) -> DiffColors {
        let mut colors = self.scheme_diff_colors();
        if mode == DiffColorMode::SchemeChanges {
            // By change, as the palettes do: lines only in the old file
            // are removed, lines only in the new file added.
            let (removed, added) = (self.removed_colors(), colors.insert);
            (colors.insert, colors.delete) = match side {
                Some(DiffSide::New) => (removed, added),
                _ => (added, removed),
            };
        }
        if let Some(palette) = get_diff_palette(mode, self.dark, side) {
            for tag in PaletteTag::ALL {
                let tone = palette.tone(tag);
                *colors.chunk_mut(tag) = ChunkColors {
                    fill: tone.fill_rgba(),
                    line: tone.line_rgba(),
                };
            }
            colors.inline = Rgba::parse(palette.inline()).unwrap_or(colors.inline);
        }
        colors
    }

    /// Colours of removed lines for [`DiffColorMode::SchemeChanges`].
    ///
    /// Meld's base schemes only use `meld:delete` as a text colour (its
    /// background is white), so the scheme's own `meld:delete` chunk
    /// colours are used only when the scheme itself defines both its
    /// background and line background. Otherwise the fill is the
    /// `meld:delete` foreground blended into the text background, with
    /// the foreground as the outline.
    fn removed_colors(&self) -> ChunkColors {
        use StyleAttr::{Background as Bg, Foreground as Fg, LineBackground as Line};
        let is_base = [BASE_SCHEME, DARK_BASE_SCHEME].contains(&self.scheme.id());
        let own = self.scheme.style("meld:delete").filter(|_| !is_base);
        if let Some(style) = own
            && let (Some(fill), Some(line)) = (style.color(Bg), style.color(Line))
        {
            return ChunkColors { fill, line };
        }
        let line = self.color("meld:delete", Fg);
        let base = self
            .scheme
            .style("text")
            .and_then(|s| s.color(Bg))
            .unwrap_or(if self.dark {
                Rgba::from_rgb8(0x1e, 0x1e, 0x1e)
            } else {
                Rgba::WHITE
            });
        let amount = if self.dark { 0.3 } else { 0.18 };
        let mix = |b: f32, o: f32| b + (o - b) * amount;
        let fill = Rgba::new(
            mix(base.r, line.r),
            mix(base.g, line.g),
            mix(base.b, line.b),
            1.0,
        );
        ChunkColors { fill, line }
    }

    /// Colours for a link map between `left_side` and the pane to its
    /// right (`LinkMap.update_diff_colors`).
    ///
    /// Link maps use the side-less colours, which assume the left pane is
    /// the old side; if it's actually the new side (e.g., a version
    /// control comparison with the working copy on the left), insert and
    /// delete colours swap to keep removed and added text coloured
    /// correctly.
    pub fn link_diff_colors(&self, mode: DiffColorMode, left_side: Option<DiffSide>) -> DiffColors {
        let mut colors = self.diff_colors(mode, None);
        if left_side == Some(DiffSide::New) {
            std::mem::swap(&mut colors.insert, &mut colors.delete);
        }
        colors
    }

    /// Folder and version control tree state colours.
    pub fn status_colors(&self) -> StatusColors {
        let fg = |name: &str| self.color(name, StyleAttr::Foreground);
        StatusColors {
            unknown: fg("meld:unknown-text"),
            new: fg("meld:insert"),
            modified: fg("meld:replace"),
            removed: fg("meld:delete"),
            error: fg("meld:error"),
            conflict: fg("meld:conflict"),
        }
    }
}

fn bundled(id: &str) -> StyleScheme {
    SchemeManager::builtin()
        .get(id)
        .unwrap_or_else(|| unreachable!("bundled scheme {id} is embedded"))
}
