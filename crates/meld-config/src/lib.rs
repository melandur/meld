//! meld-config: Meld's settings, recent comparisons, style schemes and
//! diff colours. Plain Rust, no GUI dependency; colours are [`Rgba`]
//! values with `f32` channels in `0..=1` for the app to convert.
//!
//! Replaces these Python modules:
//!
//! - `meld/settings.py` and `data/org.gnome.Meld.gschema.xml` →
//!   [`settings`]: [`Settings`] (one field per gschema key, TOML at
//!   `$XDG_CONFIG_HOME/meld/settings.toml`) and [`FontSpec`].
//! - `meld/recent.py` → [`recent`]: [`RecentComparisons`] (TOML at
//!   `$XDG_DATA_HOME/meld/recent.toml`) and the `.meldcmp` reader.
//! - GtkSourceView's `StyleSchemeManager`/`StyleScheme` → [`scheme`]:
//!   [`SchemeManager`], [`StyleScheme`], [`Style`].
//! - `meld/style.py` → [`style`]: [`Theme`], [`DiffColors`],
//!   [`StatusColors`], [`FontZoom`].
//! - `meld/diffpalette.py` → [`palette`]: [`DiffColorMode`],
//!   [`DiffSide`], [`get_diff_palette`], [`diff_sides`].
//!
//! # Public API
//!
//! Settings:
//! - `Settings::load() -> Settings`, `Settings::load_from(path) -> Settings`
//!   (missing or corrupt files give defaults; invalid keys take their
//!   default), `Settings::try_load_from(path) -> Result<Settings>`,
//!   `Settings::save(&self) -> Result<()>`, `Settings::save_to(&self, path)`
//!   (atomic), `Settings::from_toml_str`, `Settings::to_toml_string`,
//!   `Settings::changed_keys(&self, &Settings) -> Vec<String>`,
//!   `Settings::font(&self, system_monospace: Option<&str>) -> FontSpec`,
//!   `Settings::prefer_dark(&self, system_prefers_dark) -> bool`,
//!   [`settings_path`].
//! - Enum-typed keys: [`StyleVariant`], [`WrapMode`], [`OverviewMapStyle`],
//!   [`MergeFileOrder`], [`DiffColorMode`]; list keys: [`FilterSetting`],
//!   [`FolderColumn`]; [`WindowState`]; the unused [`SpacesFlags`];
//!   Preferences choices [`TIMESTAMP_RESOLUTIONS`] and `label()`s.
//! - `FontSpec::parse(&str) -> FontSpec { family, size_pt, bold, italic }`
//!   for Pango font strings.
//!
//! Recent comparisons:
//! - `RecentComparisons::load()`, `load_from(path)`, `save()`, `save_to(path)`,
//!   `add(kind, &[PathBuf], &[String]) -> bool`, `entries() ->
//!   &[RecentComparison]`, `remove`, `clear`, `prune_missing() -> usize`.
//! - [`RecentComparison`] `{ kind: ComparisonType, paths, flags, timestamp }`
//!   with `display_name()` and `description()`.
//! - [`read_meldcmp`], [`parse_meldcmp`], [`write_meldcmp`], [`recent_path`].
//!
//! Style schemes:
//! - `SchemeManager::new()` (bundled + installed + user schemes),
//!   `SchemeManager::builtin()`, `with_search_path(&[PathBuf])`,
//!   `add_dir`, `add_scheme_xml`, `ids()`, `list() -> Vec<SchemeInfo>`,
//!   `get(id) -> Option<StyleScheme>`, `adapt(scheme, want_dark)`.
//! - `StyleScheme::style(name) -> Option<Style>` (with parent and
//!   `use-style` resolution), `metadata`, `variant`, `is_dark`,
//!   `resolve_color`.
//!
//! Diff colours:
//! - `Theme::new(&SchemeManager, scheme_id, ui_dark)`,
//!   `Theme::from_settings`, `Theme::diff_colors(DiffColorMode,
//!   Option<DiffSide>) -> DiffColors`, `Theme::link_diff_colors(mode,
//!   left_side)`, `DiffColors::with_opacity(percent)`,
//!   `style::diff_color_alphas(percent)`, `Theme::pane_diff_colors(mode,
//!   num_panes, pane)`, `Theme::status_colors()`, `Theme::color(name,
//!   StyleAttr) -> Rgba`.
//! - `diff_sides(num_panes, Option<&[DiffSide]>) -> [Option<DiffSide>; 3]`,
//!   `get_diff_palette(mode, dark, side) -> Option<DiffPalette>`,
//!   `diff_tones(mode, dark) -> Option<DiffTones>`, `DiffColorMode::model()
//!   -> ColorModel` (by side or by change).

mod color;
mod error;
pub mod palette;
pub mod recent;
pub mod scheme;
pub mod settings;
pub mod style;

pub use color::Rgba;
pub use error::ConfigError;
pub use palette::{
    ColorModel, DiffColorMode, DiffPalette, DiffSide, DiffTone, DiffTones, PaletteTag, diff_sides,
    diff_tones, get_diff_palette,
};
pub use recent::{
    ComparisonType, MAX_RECENT, RecentComparison, RecentComparisons, parse_meldcmp, read_meldcmp,
    recent_path, write_meldcmp,
};
pub use scheme::{
    SchemeInfo, SchemeManager, SchemeSource, Style, StyleAttr, StyleScheme, Underline, Variant,
};
pub use settings::{
    ExplainLayout, FilterSetting, FolderColumn, FontSpec, MergeFileOrder, OverviewMapStyle,
    Settings, SpacesFlags, StyleVariant, TIMESTAMP_RESOLUTIONS, WindowState, WrapMode,
    settings_path,
};
pub use style::{ChunkColors, ChunkTag, DiffColors, FontZoom, StatusColors, Theme};
