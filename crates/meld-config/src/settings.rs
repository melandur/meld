//! Typed settings, ported from `data/org.gnome.Meld.gschema.xml` and
//! `meld/settings.py`.
//!
//! Every gschema key is a field of [`Settings`]; the TOML keys are the
//! gschema key names (kebab-case) and missing keys take the schema
//! default. The `org.gnome.meld.WindowState` keys live in a
//! `[window-state]` table.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::error::atomic_write;
use crate::palette::DiffColorMode;

/// `org.gnome.meld.stylevariant`: whether to force a light or dark UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StyleVariant {
    #[default]
    #[serde(alias = "ADW_COLOR_SCHEME_DEFAULT")]
    Default,
    #[serde(alias = "ADW_COLOR_SCHEME_FORCE_LIGHT")]
    ForceLight,
    #[serde(alias = "ADW_COLOR_SCHEME_FORCE_DARK")]
    ForceDark,
}

impl StyleVariant {
    pub const ALL: [StyleVariant; 3] = [Self::Default, Self::ForceLight, Self::ForceDark];

    /// The label the Preferences dialog shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Follow System",
            Self::ForceLight => "Light",
            Self::ForceDark => "Dark",
        }
    }

    /// Resolve against the desktop's preference.
    pub fn is_dark(self, system_prefers_dark: bool) -> bool {
        match self {
            Self::Default => system_prefers_dark,
            Self::ForceLight => false,
            Self::ForceDark => true,
        }
    }
}

/// `org.gnome.meld.wrapmode`: line wrapping in file comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WrapMode {
    #[default]
    #[serde(alias = "GTK_WRAP_NONE")]
    None,
    #[serde(alias = "GTK_WRAP_CHAR")]
    Char,
    #[serde(alias = "GTK_WRAP_WORD")]
    Word,
    WordChar,
}

impl WrapMode {
    /// The modes offered by the Preferences dialog, in its order.
    pub const PREFERENCES: [WrapMode; 3] = [Self::None, Self::Word, Self::Char];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Never",
            Self::Word => "At Spaces",
            Self::Char => "Anywhere",
            Self::WordChar => "At Spaces, Then Anywhere",
        }
    }
}

/// `org.gnome.meld.overviewmapstyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverviewMapStyle {
    #[default]
    Chunkmap,
    CompactSourcemap,
    FullSourcemap,
}

/// `org.gnome.meld.mergefileorder`: pane order for three-way VC merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeFileOrder {
    LocalMergeRemote,
    #[default]
    RemoteMergeLocal,
}

impl MergeFileOrder {
    pub const ALL: [MergeFileOrder; 2] = [Self::RemoteMergeLocal, Self::LocalMergeRemote];

    pub fn label(self) -> &'static str {
        match self {
            Self::RemoteMergeLocal => "Remote, merge, local",
            Self::LocalMergeRemote => "Local, merge, remote",
        }
    }
}

/// `org.gnome.meld.spacesflags`, GtkSourceView's space drawer flags.
///
/// No key of the current schema uses these flags (`enable-space-drawer`
/// is a boolean), but they are part of the schema and kept for the
/// space drawer widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SpacesFlags(pub u32);

impl SpacesFlags {
    pub const SPACE: Self = Self(1);
    pub const TAB: Self = Self(2);
    pub const NEWLINE: Self = Self(4);
    pub const NBSP: Self = Self(8);
    pub const LEADING: Self = Self(16);
    pub const TEXT: Self = Self(32);
    pub const TRAILING: Self = Self(64);
    pub const ALL: Self = Self(127);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for SpacesFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A `(name, enabled, pattern)` entry of `filename-filters` or
/// `text-filters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterSetting {
    pub name: String,
    pub active: bool,
    pub pattern: String,
}

impl FilterSetting {
    pub fn new(name: &str, active: bool, pattern: &str) -> Self {
        FilterSetting {
            name: name.to_string(),
            active,
            pattern: pattern.to_string(),
        }
    }
}

/// A `(name, visible)` entry of `folder-columns`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderColumn {
    pub name: String,
    pub visible: bool,
}

impl FolderColumn {
    pub fn new(name: &str, visible: bool) -> Self {
        FolderColumn {
            name: name.to_string(),
            visible,
        }
    }
}

/// The `org.gnome.meld.WindowState` schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub is_maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        WindowState {
            width: -1,
            height: -1,
            is_maximized: false,
        }
    }
}

/// Choices the Preferences dialog offers for `folder-time-resolution`,
/// as `(nanoseconds, label)`.
pub const TIMESTAMP_RESOLUTIONS: [(i32, &str); 5] = [
    (1, "1ns (ext4)"),
    (100, "100ns (NTFS)"),
    (1_000_000_000, "1s (ext2/ext3)"),
    (2_000_000_000, "2s (VFAT)"),
    (-1, "Ignore timestamp"),
];

/// Range of `diff-color-opacity`, in percent.
pub const DIFF_COLOR_OPACITY_RANGE: (i32, i32) = (10, 100);
/// Range of `ailens-file-row-spacing`.
pub const AILENS_FILE_ROW_SPACING_RANGE: (i32, i32) = (0, 16);
/// Range of `ailens-file-list-width`.
pub const AILENS_FILE_LIST_WIDTH_RANGE: (i32, i32) = (120, 1200);

/// All of Meld's settings, one field per gschema key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Settings {
    pub style_variant: StyleVariant,
    pub detect_encodings: Vec<String>,
    pub indent_width: i32,
    pub insert_spaces_instead_of_tabs: bool,
    pub show_line_numbers: bool,
    pub highlight_syntax: bool,
    pub style_scheme: String,
    pub diff_colors: DiffColorMode,
    /// Opacity of difference colours in percent; see
    /// [`crate::style::diff_color_alphas`].
    pub diff_color_opacity: i32,
    pub enable_space_drawer: bool,
    pub wrap_mode: WrapMode,
    pub highlight_current_line: bool,
    pub use_system_font: bool,
    pub custom_font: String,
    pub show_overview_map: bool,
    pub overview_map_style: OverviewMapStyle,
    pub ignore_blank_lines: bool,
    pub use_system_editor: bool,
    pub custom_editor_command: String,
    /// Where file choosers start when nothing better is known; empty is
    /// the home folder.
    pub default_folder: String,
    pub folder_ignore_symlinks: bool,
    pub folder_shallow_comparison: bool,
    pub folder_time_resolution: i32,
    pub folder_filter_text: bool,
    pub folder_status_filters: Vec<String>,
    pub vc_console_visible: bool,
    pub vc_console_pane_position: i32,
    pub vc_left_is_local: bool,
    pub vc_merge_file_order: MergeFileOrder,
    pub vc_show_commit_margin: bool,
    pub vc_commit_margin: i32,
    pub vc_break_commit_message: bool,
    pub vc_status_filters: Vec<String>,
    pub ailens_claude_path: String,
    pub ailens_infer_enabled: bool,
    pub ailens_infer_model: String,
    pub ailens_build_model: String,
    pub ailens_build_folder: String,
    pub ailens_notes_ref: String,
    pub ailens_file_row_spacing: i32,
    pub ailens_file_list_width: i32,
    pub ailens_author_pattern: String,
    // Arrays of tables come last so the TOML output stays readable.
    pub folder_columns: Vec<FolderColumn>,
    pub filename_filters: Vec<FilterSetting>,
    pub text_filters: Vec<FilterSetting>,
    pub window_state: WindowState,
}

/// Default `filename-filters`, exactly as in the gschema.
pub fn default_filename_filters() -> Vec<FilterSetting> {
    vec![
        FilterSetting::new("Backups", true, "#*# .#* ~* *~ *.{orig,bak,swp}"),
        FilterSetting::new(
            "OS-Specific Metadata",
            true,
            ".DS_Store ._* .Spotlight-V100 .Trashes Thumbs.db Desktop.ini",
        ),
        FilterSetting::new(
            "Version Control",
            true,
            "_MTN .bzr .svn .svn .hg .fslckout _FOSSIL_ .fos CVS _darcs .git .svn .osc",
        ),
        FilterSetting::new("Binaries", true, "*.{pyc,a,obj,o,so,la,lib,dll,exe}"),
        FilterSetting::new(
            "Media",
            false,
            "*.{jxl,jpg,jpeg,gif,png,avif,webp,heif,heic,bmp,tif,tiff,raw,dng,cr2,wav,wave,mp3,ogg,oga,vorbis,spx,opus,flac,ac3,aac,aif,aiff,aifc,alac,m4a,3gp,wma,aup,aup3,avi,mov,mpg,mpeg,mp4,m4v,webm,ogv,flv,xcf,xpm,ora,kra,psd,psp}",
        ),
    ]
}

/// Default `text-filters`, exactly as in the gschema (with GVariant
/// string escapes resolved).
pub fn default_text_filters() -> Vec<FilterSetting> {
    vec![
        FilterSetting::new("CVS/SVN Keywords", false, r"\$\w+(:[^\n$]+)?\$"),
        FilterSetting::new("C++ Comment", false, "//.*"),
        FilterSetting::new("C Comment", false, r"/\*.*?\*/"),
        FilterSetting::new("All Whitespace", false, r"[ \t\r\f\v]*"),
        FilterSetting::new("Leading Whitespace", false, r"^[ \t\r\f\v]*"),
        FilterSetting::new("Trailing Whitespace", false, r"[ \t\r\f\v]*$"),
        FilterSetting::new("Script Comment", false, "#.*"),
    ]
}

/// Default `folder-columns`.
pub fn default_folder_columns() -> Vec<FolderColumn> {
    vec![
        FolderColumn::new("size", true),
        FolderColumn::new("modification time", true),
        FolderColumn::new("permissions", false),
    ]
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            style_variant: StyleVariant::Default,
            detect_encodings: Vec::new(),
            indent_width: 8,
            insert_spaces_instead_of_tabs: false,
            show_line_numbers: true,
            highlight_syntax: true,
            style_scheme: "meld-monokai".to_string(),
            diff_colors: DiffColorMode::SchemeChanges,
            diff_color_opacity: 100,
            enable_space_drawer: false,
            wrap_mode: WrapMode::Word,
            highlight_current_line: false,
            use_system_font: true,
            custom_font: "monospace, 14".to_string(),
            show_overview_map: true,
            overview_map_style: OverviewMapStyle::Chunkmap,
            ignore_blank_lines: false,
            use_system_editor: true,
            custom_editor_command: String::new(),
            default_folder: String::new(),
            folder_columns: default_folder_columns(),
            folder_ignore_symlinks: false,
            folder_shallow_comparison: false,
            folder_time_resolution: 100,
            folder_filter_text: true,
            folder_status_filters: strings(&["normal", "modified", "new"]),
            vc_console_visible: false,
            vc_console_pane_position: 300,
            vc_left_is_local: false,
            vc_merge_file_order: MergeFileOrder::RemoteMergeLocal,
            vc_show_commit_margin: true,
            vc_commit_margin: 72,
            vc_break_commit_message: false,
            vc_status_filters: strings(&["flatten", "modified"]),
            filename_filters: default_filename_filters(),
            text_filters: default_text_filters(),
            ailens_claude_path: "claude".to_string(),
            ailens_infer_enabled: true,
            ailens_infer_model: "claude-sonnet-5-5".to_string(),
            ailens_build_model: "claude-opus-5-5".to_string(),
            ailens_build_folder: "agent".to_string(),
            ailens_notes_ref: "ai-lens".to_string(),
            ailens_file_row_spacing: 4,
            ailens_file_list_width: 300,
            ailens_author_pattern: String::new(),
            window_state: WindowState::default(),
        }
    }
}

/// Path of the settings file, `$XDG_CONFIG_HOME/meld/settings.toml`.
pub fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("meld").join("settings.toml"))
}

impl Settings {
    /// Load from [`settings_path`]; see [`Settings::load_from`].
    pub fn load() -> Settings {
        match settings_path() {
            Some(path) => Self::load_from(path),
            None => {
                log::warn!("No configuration directory; using default settings");
                Settings::default()
            }
        }
    }

    /// Load settings from a file.
    ///
    /// A missing file gives the defaults. An unreadable or unparseable
    /// file is logged and gives the defaults. Keys with a value of the
    /// wrong type are logged and take their default, without affecting
    /// the other keys.
    pub fn load_from(path: impl AsRef<Path>) -> Settings {
        let path = path.as_ref();
        match Self::try_load_from(path) {
            Ok(settings) => settings,
            Err(ConfigError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Settings::default()
            }
            Err(e) => {
                log::warn!("{e}; using default settings");
                Settings::default()
            }
        }
    }

    /// Like [`Settings::load_from`], but reports I/O and syntax errors.
    pub fn try_load_from(path: impl AsRef<Path>) -> Result<Settings, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::io(path, e))?;
        Self::from_toml_str(&text).map_err(|source| ConfigError::TomlParse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Parse settings from TOML text.
    ///
    /// Only TOML syntax errors fail; keys with invalid values are logged
    /// and take their defaults.
    pub fn from_toml_str(text: &str) -> Result<Settings, toml::de::Error> {
        let user: toml::Table = toml::from_str(text)?;
        if let Ok(settings) = toml::Table::try_into::<Settings>(user.clone()) {
            return Ok(settings.sanitized());
        }

        // Fall back to applying keys one at a time, so that one bad
        // value doesn't reset everything else.
        let mut merged = Settings::default().to_table();
        for (key, value) in user {
            let previous = merged.insert(key.clone(), value);
            if toml::Table::try_into::<Settings>(merged.clone()).is_err() {
                log::warn!("Ignoring invalid value for setting '{key}'");
                match previous {
                    Some(previous) => merged.insert(key, previous),
                    None => merged.remove(&key),
                };
            }
        }
        Ok(toml::Table::try_into::<Settings>(merged)
            .unwrap_or_default()
            .sanitized())
    }

    /// Serialize to TOML text.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    fn to_table(&self) -> toml::Table {
        toml::Table::try_from(self).unwrap_or_default()
    }

    /// Save to [`settings_path`].
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = settings_path().ok_or(ConfigError::NoConfigDir)?;
        self.save_to(path)
    }

    /// Atomically write the settings to a file, creating its directory.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let text = self.to_toml_string()?;
        atomic_write(path.as_ref(), text.as_bytes())
    }

    /// Clamp ranged keys into their gschema ranges.
    pub fn sanitized(mut self) -> Settings {
        let (lo, hi) = DIFF_COLOR_OPACITY_RANGE;
        self.diff_color_opacity = self.diff_color_opacity.clamp(lo, hi);
        let (lo, hi) = AILENS_FILE_ROW_SPACING_RANGE;
        self.ailens_file_row_spacing = self.ailens_file_row_spacing.clamp(lo, hi);
        let (lo, hi) = AILENS_FILE_LIST_WIDTH_RANGE;
        self.ailens_file_list_width = self.ailens_file_list_width.clamp(lo, hi);
        self
    }

    /// The gschema key names whose values differ between two settings,
    /// e.g. to notify views like `MeldSettings`'s "changed" signals.
    /// Window state keys are reported as `window-state`.
    pub fn changed_keys(&self, other: &Settings) -> Vec<String> {
        let a = self.to_table();
        let b = other.to_table();
        let mut keys: Vec<String> = a
            .iter()
            .filter(|(k, v)| b.get(*k) != Some(*v))
            .map(|(k, _)| k.clone())
            .chain(b.keys().filter(|k| !a.contains_key(*k)).cloned())
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// The font file comparisons use, as `MeldSettings.font` resolves it.
    ///
    /// With `use-system-font`, `system_monospace` is the desktop's
    /// monospace font name (GNOME's `monospace-font-name`), if known; on
    /// Windows it is always "Consolas 11". Otherwise `custom-font` is
    /// used. An empty font string falls back to "monospace 11".
    pub fn font(&self, system_monospace: Option<&str>) -> FontSpec {
        let font_string = if self.use_system_font {
            if cfg!(windows) {
                "Consolas 11".to_string()
            } else {
                system_monospace.unwrap_or_default().to_string()
            }
        } else {
            self.custom_font.clone()
        };
        if font_string.trim().is_empty() {
            FontSpec::parse(FontSpec::FALLBACK)
        } else {
            FontSpec::parse(&font_string)
        }
    }

    /// Whether the dark variant should be used, following `style-variant`.
    pub fn prefer_dark(&self, system_prefers_dark: bool) -> bool {
        self.style_variant.is_dark(system_prefers_dark)
    }
}

/// A font parsed from a Pango font description string such as
/// "Monospace 11", "DejaVu Sans Mono Bold 10.5" or "monospace, 14".
#[derive(Debug, Clone, PartialEq)]
pub struct FontSpec {
    /// The first family of the description's family list.
    pub family: String,
    /// Size in points; descriptions without a size get 11pt.
    pub size_pt: f32,
    pub bold: bool,
    pub italic: bool,
}

impl FontSpec {
    /// Font used when nothing else is configured.
    pub const FALLBACK: &'static str = "monospace 11";
    const DEFAULT_SIZE: f32 = 11.0;

    /// Parse a Pango font description string.
    ///
    /// The syntax is `[FAMILY-LIST] [STYLE-OPTIONS] [SIZE]`, where the
    /// family list is comma separated, style options are words like
    /// "Bold" or "Italic" and the size is in points, or in pixels with a
    /// "px" suffix (converted at 96 dpi).
    pub fn parse(description: &str) -> FontSpec {
        let mut words: Vec<&str> = description.split_whitespace().collect();
        let mut size_pt = None;
        let mut bold = false;
        let mut italic = false;

        if let Some(last) = words.last() {
            if let Some(px) = last.strip_suffix("px").and_then(|v| v.parse::<f32>().ok()) {
                size_pt = Some(px * 0.75);
                words.pop();
            } else if let Ok(pt) = last.parse::<f32>() {
                size_pt = Some(pt);
                words.pop();
            }
        }

        // Style words are only recognised after the family list, i.e.
        // when not followed by a comma.
        while let Some(word) = words.last() {
            if word.ends_with(',') || words.len() == 1 {
                break;
            }
            match word.to_ascii_lowercase().as_str() {
                "bold" | "semi-bold" | "semibold" | "demi-bold" | "demibold" | "ultra-bold"
                | "ultrabold" | "extra-bold" | "extrabold" | "heavy" | "black" | "ultra-heavy"
                | "ultraheavy" => bold = true,
                "italic" | "oblique" => italic = true,
                "normal" | "roman" | "regular" | "book" | "medium" | "light" | "thin"
                | "ultra-light" | "ultralight" | "extra-light" | "extralight" | "semi-light"
                | "semilight" | "demi-light" | "demilight" | "small-caps" | "condensed"
                | "semi-condensed" | "semicondensed" | "expanded" | "semi-expanded"
                | "semiexpanded" | "ultra-condensed" | "ultracondensed" | "extra-condensed"
                | "extracondensed" | "extra-expanded" | "extraexpanded" | "ultra-expanded"
                | "ultraexpanded" | "not-rotated" | "south" | "upside-down" | "north"
                | "rotated-left" | "east" | "rotated-right" | "west" => {}
                _ => break,
            }
            words.pop();
        }

        let families = words.join(" ");
        let family = families
            .split(',')
            .map(str::trim)
            .find(|f| !f.is_empty())
            .unwrap_or("monospace")
            .to_string();

        FontSpec {
            family,
            size_pt: size_pt.filter(|s| *s > 0.0).unwrap_or(Self::DEFAULT_SIZE),
            bold,
            italic,
        }
    }

    /// Size in whole points, like `Pango.FontDescription.get_size() //
    /// Pango.SCALE`.
    pub fn size_points(&self) -> i32 {
        self.size_pt.floor() as i32
    }
}

impl std::fmt::Display for FontSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.family)?;
        if self.bold {
            write!(f, " Bold")?;
        }
        if self.italic {
            write!(f, " Italic")?;
        }
        write!(f, " {}", self.size_pt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_schema() {
        let s = Settings::default();
        assert_eq!(s.style_variant, StyleVariant::Default);
        assert!(s.detect_encodings.is_empty());
        assert_eq!(s.indent_width, 8);
        assert!(!s.insert_spaces_instead_of_tabs);
        assert!(s.show_line_numbers);
        assert!(s.highlight_syntax);
        assert_eq!(s.style_scheme, "meld-monokai");
        assert_eq!(s.diff_colors, DiffColorMode::SchemeChanges);
        assert!(!s.enable_space_drawer);
        assert_eq!(s.wrap_mode, WrapMode::Word);
        assert!(!s.highlight_current_line);
        assert!(s.use_system_font);
        assert_eq!(s.custom_font, "monospace, 14");
        assert!(s.show_overview_map);
        assert_eq!(s.overview_map_style, OverviewMapStyle::Chunkmap);
        assert!(!s.ignore_blank_lines);
        assert!(s.use_system_editor);
        assert_eq!(s.custom_editor_command, "");
        assert_eq!(
            s.folder_columns,
            vec![
                FolderColumn::new("size", true),
                FolderColumn::new("modification time", true),
                FolderColumn::new("permissions", false),
            ]
        );
        assert!(!s.folder_ignore_symlinks);
        assert!(!s.folder_shallow_comparison);
        assert_eq!(s.folder_time_resolution, 100);
        assert!(s.folder_filter_text);
        assert_eq!(s.folder_status_filters, ["normal", "modified", "new"]);
        assert!(!s.vc_console_visible);
        assert_eq!(s.vc_console_pane_position, 300);
        assert!(!s.vc_left_is_local);
        assert_eq!(s.vc_merge_file_order, MergeFileOrder::RemoteMergeLocal);
        assert!(s.vc_show_commit_margin);
        assert_eq!(s.vc_commit_margin, 72);
        assert!(!s.vc_break_commit_message);
        assert_eq!(s.vc_status_filters, ["flatten", "modified"]);
        assert_eq!(s.filename_filters.len(), 5);
        assert_eq!(s.filename_filters[4].name, "Media");
        assert!(!s.filename_filters[4].active);
        assert!(s.filename_filters[..4].iter().all(|f| f.active));
        assert_eq!(s.text_filters.len(), 7);
        assert_eq!(s.text_filters[0].pattern, "\\$\\w+(:[^\\n$]+)?\\$");
        assert_eq!(s.text_filters[2].pattern, "/\\*.*?\\*/");
        assert!(s.text_filters.iter().all(|f| !f.active));
        assert_eq!(s.ailens_claude_path, "claude");
        assert!(s.ailens_infer_enabled);
        assert_eq!(s.ailens_infer_model, "claude-sonnet-5-5");
        assert_eq!(s.ailens_build_model, "claude-opus-5-5");
        assert_eq!(s.ailens_build_folder, "agent");
        assert_eq!(s.ailens_notes_ref, "ai-lens");
        assert_eq!(s.ailens_file_row_spacing, 4);
        assert_eq!(s.ailens_file_list_width, 300);
        assert_eq!(s.ailens_author_pattern, "");
        assert_eq!(
            s.window_state,
            WindowState {
                width: -1,
                height: -1,
                is_maximized: false
            }
        );
    }

    /// Every gschema key appears in the serialized settings, with the
    /// schema's default for the simple types.
    #[test]
    fn every_schema_key_is_a_setting() {
        let schema = include_str!("../../../data/org.gnome.Meld.gschema.xml");
        let table = Settings::default().to_table();
        let window: &toml::Table = table["window-state"].as_table().unwrap();
        let mut count = 0;
        for line in schema.lines() {
            let Some(rest) = line.trim().strip_prefix("<key name=\"") else {
                continue;
            };
            let key = &rest[..rest.find('"').unwrap()];
            count += 1;
            assert!(
                table.contains_key(key) || window.contains_key(key),
                "missing setting {key}"
            );
        }
        assert_eq!(count, 48);
        assert_eq!(table.len() - 1 + window.len(), 48);
    }

    #[test]
    fn enum_values_match_schema_nicks() {
        let schema = include_str!("../../../data/org.gnome.Meld.gschema.xml");
        let check = |value: toml::Value| {
            let nick = value.as_str().unwrap().to_string();
            assert!(schema.contains(&format!("nick=\"{nick}\"")), "{nick}");
        };
        for v in StyleVariant::ALL {
            check(toml::Value::try_from(v).unwrap());
        }
        for v in [
            WrapMode::None,
            WrapMode::Char,
            WrapMode::Word,
            WrapMode::WordChar,
        ] {
            check(toml::Value::try_from(v).unwrap());
        }
        for v in [
            OverviewMapStyle::Chunkmap,
            OverviewMapStyle::CompactSourcemap,
            OverviewMapStyle::FullSourcemap,
        ] {
            check(toml::Value::try_from(v).unwrap());
        }
        for v in MergeFileOrder::ALL {
            check(toml::Value::try_from(v).unwrap());
        }
        for v in DiffColorMode::ALL {
            check(toml::Value::try_from(v).unwrap());
        }
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Settings::from_toml_str("").unwrap(), Settings::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let s = Settings::from_toml_str(
            "indent-width = 4\nwrap-mode = 'GTK_WRAP_WORD'\n\
             style-variant = 'force-dark'\n[window-state]\nwidth = 800\n",
        )
        .unwrap();
        assert_eq!(s.indent_width, 4);
        assert_eq!(s.wrap_mode, WrapMode::Word);
        assert_eq!(s.style_variant, StyleVariant::ForceDark);
        assert_eq!(s.window_state.width, 800);
        assert_eq!(s.window_state.height, -1);
        assert_eq!(s.text_filters, default_text_filters());
    }

    #[test]
    fn invalid_values_only_reset_their_key() {
        let s = Settings::from_toml_str(
            "indent-width = 'wide'\nwrap-mode = 'sideways'\nshow-line-numbers = true\n\
             unknown-key = 3\nailens-file-list-width = 5\n",
        )
        .unwrap();
        assert_eq!(s.indent_width, 8);
        assert_eq!(s.wrap_mode, WrapMode::Word);
        assert!(s.show_line_numbers);
        // Clamped into the schema range
        assert_eq!(s.ailens_file_list_width, 120);
    }

    #[test]
    fn diff_color_settings() {
        let s = Settings::default();
        assert_eq!(s.diff_colors, DiffColorMode::SchemeChanges);
        assert_eq!(s.diff_color_opacity, 100);
        // Every palette is a valid value, including Meld's original ones
        for mode in DiffColorMode::ALL {
            let s = Settings::from_toml_str(&format!("diff-colors = '{}'", mode.as_str())).unwrap();
            assert_eq!(s.diff_colors, mode);
        }
        let s = Settings::from_toml_str("diff-colors = 'rainbow'\nindent-width = 3").unwrap();
        assert_eq!(s.diff_colors, DiffColorMode::SchemeChanges);
        assert_eq!(s.indent_width, 3);
        // Opacity is clamped into the schema range
        for (value, expected) in [(5, 10), (10, 10), (55, 55), (100, 100), (150, 100)] {
            let s = Settings::from_toml_str(&format!("diff-color-opacity = {value}")).unwrap();
            assert_eq!(s.diff_color_opacity, expected);
        }
        let s = Settings::from_toml_str("diff-color-opacity = 'faint'").unwrap();
        assert_eq!(s.diff_color_opacity, 100);
    }

    #[test]
    fn syntax_error_is_reported() {
        assert!(Settings::from_toml_str("indent-width = = 3").is_err());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.toml");
        let mut s = Settings {
            indent_width: 2,
            diff_colors: DiffColorMode::Colorblind,
            ..Default::default()
        };
        s.text_filters[1].active = true;
        s.folder_columns.reverse();
        s.window_state.is_maximized = true;
        s.save_to(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("indent-width = 2"));
        assert!(text.contains("diff-colors = \"colorblind\""));
        assert!(text.contains("[[text-filters]]"));
        assert_eq!(Settings::load_from(&path), s);
        assert_eq!(Settings::try_load_from(&path).unwrap(), s);
        // No temporary files left behind
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn corrupt_or_missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert_eq!(Settings::load_from(&path), Settings::default());
        std::fs::write(&path, "this is [not toml").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        assert!(Settings::try_load_from(&path).is_err());
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
    }

    #[test]
    fn changed_keys() {
        let a = Settings::default();
        let mut b = a.clone();
        assert!(a.changed_keys(&b).is_empty());
        b.custom_font = "Sans 9".into();
        b.text_filters.pop();
        b.window_state.width = 3;
        assert_eq!(
            a.changed_keys(&b),
            ["custom-font", "text-filters", "window-state"]
        );
    }

    #[test]
    fn font_resolution() {
        let mut s = Settings::default();
        assert_eq!(s.font(Some("Source Code Pro 10")).family, "Source Code Pro");
        assert_eq!(s.font(Some("Source Code Pro 10")).size_pt, 10.0);
        let fallback = s.font(None);
        assert_eq!(
            (fallback.family.as_str(), fallback.size_pt),
            ("monospace", 11.0)
        );
        s.use_system_font = false;
        let custom = s.font(Some("ignored 3"));
        assert_eq!(
            (custom.family.as_str(), custom.size_pt),
            ("monospace", 14.0)
        );
        s.custom_font = String::new();
        assert_eq!(s.font(None), FontSpec::parse("monospace 11"));
    }

    #[test]
    fn font_parsing() {
        let f = FontSpec::parse("Monospace 11");
        assert_eq!(
            (f.family.as_str(), f.size_pt, f.bold, f.italic),
            ("Monospace", 11.0, false, false)
        );
        let f = FontSpec::parse("DejaVu Sans Mono Bold Italic 10.5");
        assert_eq!(
            (f.family.as_str(), f.size_pt, f.bold, f.italic),
            ("DejaVu Sans Mono", 10.5, true, true)
        );
        let f = FontSpec::parse("Fira Code, monospace 12");
        assert_eq!((f.family.as_str(), f.size_pt), ("Fira Code", 12.0));
        let f = FontSpec::parse("Sans 16px");
        assert_eq!((f.family.as_str(), f.size_pt), ("Sans", 12.0));
        let f = FontSpec::parse("Bold");
        assert_eq!(
            (f.family.as_str(), f.size_pt, f.bold),
            ("Bold", 11.0, false)
        );
        let f = FontSpec::parse("");
        assert_eq!((f.family.as_str(), f.size_pt), ("monospace", 11.0));
        assert_eq!(FontSpec::parse("Mono 11.7").size_points(), 11);
    }

    #[test]
    fn style_variant_resolution() {
        let mut s = Settings::default();
        assert!(s.prefer_dark(true));
        assert!(!s.prefer_dark(false));
        s.style_variant = StyleVariant::ForceLight;
        assert!(!s.prefer_dark(true));
        s.style_variant = StyleVariant::ForceDark;
        assert!(s.prefer_dark(false));
    }

    #[test]
    fn spaces_flags() {
        let f = SpacesFlags::SPACE | SpacesFlags::TAB;
        assert!(f.contains(SpacesFlags::TAB));
        assert!(!f.contains(SpacesFlags::NBSP));
        assert!(SpacesFlags::ALL.contains(f));
    }
}
