//! State shared by the window and every tab: settings, style schemes,
//! fonts and the recent comparisons list.

use std::path::{Path, PathBuf};

use iced::{Color, Font, Theme};
use meld_config::{
    ComparisonType, RecentComparison, RecentComparisons, Rgba, SchemeManager, Settings, StyleAttr,
};

use crate::comparison::Comparison;

pub struct Shared {
    pub settings: Settings,
    pub schemes: SchemeManager,
    /// The resolved style scheme, for diff and syntax colors.
    pub style: meld_config::Theme,
    /// The iced theme derived from the style scheme.
    pub theme: Theme,
    pub recent: RecentComparisons,
    /// Font size change from zooming, in points.
    pub zoom: f32,
    pub system_dark: bool,
    /// The desktop's monospace font, as a Pango font description.
    system_monospace: Option<String>,
    mono_font: Font,
}

impl Shared {
    pub fn load() -> Self {
        let settings = Settings::load();
        let schemes = SchemeManager::new();
        let system_dark = detect_system_dark();
        let style = meld_config::Theme::from_settings(&schemes, &settings, system_dark);
        let mut shared = Shared {
            theme: Theme::Dark,
            recent: RecentComparisons::load(),
            zoom: 0.0,
            system_dark,
            system_monospace: system_monospace_font(),
            mono_font: Font::MONOSPACE,
            settings,
            schemes,
            style,
        };
        shared.refresh_theme();
        shared
    }

    pub fn initial_window_size() -> iced::Size {
        let state = Settings::load().window_state;
        let size = |value: i32, default: f32, min: f32| {
            if value > 0 {
                (value as f32).max(min)
            } else {
                default
            }
        };
        iced::Size::new(
            size(state.width, 1000.0, 500.0),
            size(state.height, 750.0, 350.0),
        )
    }

    pub fn save_settings(&self) {
        if let Err(err) = self.settings.save() {
            log::warn!("could not save settings: {err}");
        }
    }

    /// Re-resolves the style scheme, iced theme and fonts after a settings
    /// or system theme change.
    pub fn refresh_theme(&mut self) {
        self.style =
            meld_config::Theme::from_settings(&self.schemes, &self.settings, self.system_dark);
        self.theme = iced_theme(&self.style);
        let spec = self.settings.font(self.system_monospace.as_deref());
        self.mono_font = match spec.family.to_ascii_lowercase().as_str() {
            "monospace" | "mono" => Font::MONOSPACE,
            _ => Font::with_name(intern(&spec.family)),
        };
        if spec.bold {
            self.mono_font.weight = iced::font::Weight::Bold;
        }
        if spec.italic {
            self.mono_font.style = iced::font::Style::Italic;
        }
    }

    /// The font for comparison text.
    pub fn mono_font(&self) -> Font {
        self.mono_font
    }

    /// The comparison text size in logical pixels, including zoom.
    pub fn mono_size(&self) -> f32 {
        let pt = self.settings.font(self.system_monospace.as_deref()).size_pt + self.zoom;
        (pt.max(4.0) * 96.0 / 72.0).round()
    }

    /// A color from the current style scheme, by GtkSourceView style name.
    pub fn scheme_color(&self, name: &str, attribute: StyleAttr) -> Option<Color> {
        self.style.lookup(name, attribute).map(color)
    }

    pub fn add_recent(&mut self, comparison: &Comparison) {
        let Some((kind, paths, flags)) = recent_entry(comparison) else {
            return;
        };
        if self.recent.add(kind, &paths, &flags)
            && let Err(err) = self.recent.save()
        {
            log::warn!("could not save recent comparisons: {err}");
        }
    }

    pub fn recent_comparisons(&self) -> Vec<(String, Comparison)> {
        self.recent
            .entries()
            .iter()
            .filter_map(|entry| Some((entry.display_name(), comparison_from_recent(entry)?)))
            .collect()
    }
}

/// Converts a config color to an iced color.
pub fn color(c: Rgba) -> Color {
    Color::from_rgba(c.r, c.g, c.b, c.a)
}

fn iced_theme(style: &meld_config::Theme) -> Theme {
    let dark = style.is_dark();
    let base = if dark {
        iced::theme::Palette::DARK
    } else {
        iced::theme::Palette::LIGHT
    };
    let background = style
        .lookup("text", StyleAttr::Background)
        .map(color)
        .unwrap_or(base.background);
    let text = style
        .lookup("text", StyleAttr::Foreground)
        .map(color)
        .unwrap_or(base.text);
    let primary = style
        .lookup("selection", StyleAttr::Background)
        .map(color)
        .filter(|c| c.a > 0.5)
        .unwrap_or(base.primary);
    let palette = iced::theme::Palette {
        background,
        text,
        primary,
        ..base
    };
    let name = if dark { "Meld Dark" } else { "Meld Light" };
    Theme::custom(name, palette)
}

/// Fonts must be `'static` in iced; font family names are few, so keep
/// each one alive for the rest of the run.
fn intern(name: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut names = NAMES.lock().expect("font name lock");
    let names = names.get_or_insert_with(HashSet::new);
    if let Some(existing) = names.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(value.trim_matches('\'').to_string())
}

fn system_monospace_font() -> Option<String> {
    gsettings_get("org.gnome.desktop.interface", "monospace-font-name").filter(|f| !f.is_empty())
}

fn detect_system_dark() -> bool {
    match gsettings_get("org.gnome.desktop.interface", "color-scheme") {
        Some(scheme) if scheme.contains("dark") => true,
        Some(_) => gsettings_get("org.gnome.desktop.interface", "gtk-theme")
            .is_some_and(|t| t.to_ascii_lowercase().contains("dark")),
        None => false,
    }
}

fn recent_entry(comparison: &Comparison) -> Option<(ComparisonType, Vec<PathBuf>, Vec<String>)> {
    Some(match comparison {
        Comparison::Files {
            paths, auto_merge, ..
        } => {
            // Blank panes can't be reopened.
            let paths: Option<Vec<PathBuf>> = paths.iter().cloned().collect();
            let kind = if *auto_merge {
                ComparisonType::Merge
            } else {
                ComparisonType::File
            };
            (kind, paths?, Vec::new())
        }
        Comparison::Folders { paths, .. } => {
            if paths.iter().any(|p| p.as_os_str().is_empty()) {
                return None;
            }
            (ComparisonType::Folder, paths.clone(), Vec::new())
        }
        Comparison::VersionControl { location, .. } => (
            ComparisonType::VersionControl,
            vec![location.clone()],
            Vec::new(),
        ),
        Comparison::VersionControlFile(path) => (
            ComparisonType::VersionControl,
            vec![path.clone()],
            Vec::new(),
        ),
        Comparison::Git {
            location,
            base,
            target,
            pr,
        } => {
            let mut flags = Vec::new();
            if let Some(base) = base {
                flags.push(format!("base={base}"));
            }
            if let Some(target) = target {
                flags.push(format!("target={target}"));
            }
            if let Some(pr) = pr {
                flags.push(format!("pr={}", pr.number));
                if let Some(remote) = &pr.remote {
                    flags.push(format!("remote={remote}"));
                }
            }
            (ComparisonType::GitCompare, vec![location.clone()], flags)
        }
        Comparison::Images { paths } => (ComparisonType::File, paths.clone(), Vec::new()),
        Comparison::SavedFile(_) => return None,
    })
}

fn comparison_from_recent(entry: &RecentComparison) -> Option<Comparison> {
    let flag = |name: &str| {
        entry
            .flags
            .iter()
            .find_map(|f| f.strip_prefix(name)?.strip_prefix('=').map(str::to_string))
    };
    Some(match entry.kind {
        ComparisonType::File => {
            Comparison::from_paths(entry.paths.iter().cloned().map(Some).collect(), false).ok()?
        }
        ComparisonType::Merge => {
            let mut c = Comparison::files(entry.paths.iter().cloned().map(Some).collect());
            c.set_auto_merge();
            c
        }
        ComparisonType::Folder => Comparison::Folders {
            paths: entry.paths.clone(),
            labels: Vec::new(),
            auto_compare: false,
        },
        ComparisonType::VersionControl => {
            Comparison::from_paths(vec![entry.paths.first().cloned()], false).ok()?
        }
        ComparisonType::GitCompare => Comparison::Git {
            location: entry.paths.first()?.clone(),
            base: flag("base"),
            target: flag("target"),
            pr: flag("pr")
                .and_then(|n| n.parse().ok())
                .map(|number| crate::comparison::PrSpec {
                    number,
                    remote: flag("remote"),
                }),
        },
    })
}

pub fn read_comparison_file(path: &Path) -> Result<Comparison, String> {
    let entry = meld_config::read_meldcmp(path)
        .map_err(|e| format!("{}: {e}", tr!("Error reading saved comparison file")))?;
    comparison_from_recent(&entry).ok_or_else(|| tr!("Error reading saved comparison file").into())
}
