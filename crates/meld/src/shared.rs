//! State shared by the window and every tab: settings, style schemes,
//! fonts and the recent comparisons list.

use std::path::Path;

use iced::Theme;

use crate::comparison::Comparison;

pub struct Shared {
    pub settings: Settings,
    pub theme: Theme,
    /// Font size change from zooming, in points.
    pub zoom: f32,
}

/// Placeholder until meld-config lands.
#[derive(Debug, Clone)]
pub struct Settings {
    pub width: i32,
    pub height: i32,
    pub is_maximized: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            width: 1000,
            height: 750,
            is_maximized: false,
        }
    }
}

impl Shared {
    pub fn load() -> Self {
        Shared {
            settings: Settings::default(),
            theme: Theme::Dark,
            zoom: 0.0,
        }
    }

    pub fn save_settings(&self) {}

    pub fn initial_window_size() -> iced::Size {
        iced::Size::new(1000.0, 750.0)
    }

    pub fn refresh_theme(&mut self) {}

    pub fn recent_comparisons(&self) -> Vec<(String, Comparison)> {
        Vec::new()
    }

    pub fn add_recent(&mut self, _comparison: &Comparison) {}
}

pub fn read_comparison_file(path: &Path) -> Result<Comparison, String> {
    Err(format!("cannot read {}", path.display()))
}
