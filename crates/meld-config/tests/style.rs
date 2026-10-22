//! Ports of test/test_style.py, test/test_style_schemes.py and the
//! meld.style parts of test/test_diffpalette.py.

use std::collections::BTreeSet;

use meld_config::{
    ChunkColors, DiffColorMode, DiffColors, DiffSide, FontSpec, FontZoom, PaletteTag, Rgba,
    SchemeManager, StyleAttr, StyleScheme, Theme, Underline, Variant, get_diff_palette,
};

const BASE_XML: &str = include_str!("../../../data/styles/meld-base.style-scheme.xml.in");

fn scheme_xml(id: &str) -> &'static str {
    match id {
        "meld-base" => BASE_XML,
        "meld-dark" => include_str!("../../../data/styles/meld-dark.style-scheme.xml.in"),
        "meld-lens-light" => include_str!("../../../data/styles/meld-lens-light.style-scheme.xml.in"),
        "meld-lens-dark" => include_str!("../../../data/styles/meld-lens-dark.style-scheme.xml.in"),
        "meld-monokai" => include_str!("../../../data/styles/meld-monokai.style-scheme.xml.in"),
        _ => unreachable!(),
    }
}

/// Bundled schemes, with their name, variant and the metadata key and
/// scheme ID of their other variant.
const BUNDLED_SCHEMES: [(&str, &str, &str, &str, &str); 3] = [
    (
        "meld-lens-light",
        "Meld Lens Light",
        "light",
        "dark-variant",
        "meld-lens-dark",
    ),
    (
        "meld-lens-dark",
        "Meld Lens Dark",
        "dark",
        "light-variant",
        "meld-lens-light",
    ),
    (
        "meld-monokai",
        "Monokai (PyCharm)",
        "dark",
        "light-variant",
        "meld-lens-light",
    ),
];
const PAIRED_SCHEMES: [&str; 2] = ["meld-lens-light", "meld-lens-dark"];

const SYNTAX_STYLES: [&str; 20] = [
    "text",
    "selection",
    "current-line",
    "line-numbers",
    "bracket-match",
    "search-match",
    "def:comment",
    "def:doc-comment",
    "def:string",
    "def:keyword",
    "def:type",
    "def:function",
    "def:number",
    "def:constant",
    "def:preprocessor",
    "def:special-char",
    "def:operator",
    "def:identifier",
    "def:error",
    "def:note",
];

/// Style names a scheme file defines itself.
fn style_names(xml: &str) -> BTreeSet<String> {
    xml.lines()
        .filter_map(|l| l.trim().strip_prefix("<style name=\""))
        .map(|rest| rest[..rest.find('"').unwrap()].to_string())
        .collect()
}

fn manager() -> SchemeManager {
    SchemeManager::builtin()
}

fn scheme(id: &str) -> StyleScheme {
    manager()
        .get(id)
        .unwrap_or_else(|| panic!("no scheme {id}"))
}

// test_style_schemes.py

#[test]
fn scheme_loads() {
    let manager = manager();
    for (id, name, variant, other_key, other_id) in BUNDLED_SCHEMES {
        let scheme = manager.get(id).unwrap();
        assert_eq!(scheme.name(), name);
        assert!(manager.ids().contains(&id.to_string()));

        // Variant metadata lets adapt() switch with dark mode
        assert_eq!(scheme.metadata("variant"), Some(variant));
        assert_eq!(scheme.metadata(other_key), Some(other_id));
        let other = manager.get(other_id).unwrap();
        if PAIRED_SCHEMES.contains(&id) {
            assert_eq!(other.metadata(&format!("{variant}-variant")), Some(id));
        }
    }
}

#[test]
fn scheme_defines_meld_styles() {
    let base = scheme("meld-base");
    let base_styles = style_names(BASE_XML);
    let meld_styles: Vec<&String> = base_styles
        .iter()
        .filter(|n| n.starts_with("meld:"))
        .collect();
    assert!(!meld_styles.is_empty());

    for (id, ..) in BUNDLED_SCHEMES {
        let scheme = scheme(id);
        let scheme_styles = style_names(scheme_xml(id));
        for name in &meld_styles {
            assert!(scheme_styles.contains(*name), "{id} lacks {name}");
            assert!(scheme.defines_style(name));
        }
        assert!(scheme_styles.contains("map-overlay"));

        // Every colour attribute that meld-base sets must resolve to a
        // colour, since the theme looks these up directly.
        for name in &meld_styles {
            let base_style = base.style(name).unwrap();
            let style = scheme.style(name).unwrap_or_else(|| panic!("{id} {name}"));
            for attr in [
                StyleAttr::Foreground,
                StyleAttr::Background,
                StyleAttr::LineBackground,
            ] {
                if base_style.color(attr).is_none() {
                    continue;
                }
                assert!(
                    style.color(attr).is_some(),
                    "{id} {name} is missing {attr:?}"
                );
            }
        }
    }
}

/// The base schemes must resolve every colour attribute they write,
/// including the "#rgba(…)" ones.
#[test]
fn base_schemes_resolve_all_their_colours() {
    for id in ["meld-base", "meld-dark"] {
        let xml = scheme_xml(id);
        let scheme = scheme(id);
        for name in style_names(xml) {
            let line = xml
                .lines()
                .find(|l| l.contains(&format!("name=\"{name}\"")))
                .unwrap();
            let style = scheme.style(&name).unwrap();
            for (attr, key) in [
                (StyleAttr::Foreground, " foreground="),
                (StyleAttr::Background, " background="),
                (StyleAttr::LineBackground, " line-background="),
            ] {
                if line.contains(key) {
                    assert!(style.color(attr).is_some(), "{id} {name} {key}");
                }
            }
        }
    }
    let highlight = scheme("meld-base")
        .style("meld:current-line-highlight")
        .unwrap();
    assert_eq!(highlight.background.unwrap().to_rgba8(), [255, 255, 0, 64]);
    let overlay = scheme("meld-lens-dark").style("map-overlay").unwrap();
    assert_eq!(overlay.background.unwrap().to_rgba8(), [200, 200, 200, 64]);
}

#[test]
fn scheme_defines_syntax_styles() {
    for (id, ..) in BUNDLED_SCHEMES {
        let scheme = scheme(id);
        for name in SYNTAX_STYLES {
            assert!(scheme.style(name).is_some(), "{id} {name}");
        }
    }
}

#[test]
fn scheme_diff_colours() {
    for (id, ..) in BUNDLED_SCHEMES {
        let scheme = scheme(id);
        // GitHub-like: inserts are green and deletes are red
        let insert = scheme
            .style("meld:insert")
            .unwrap()
            .line_background
            .unwrap();
        assert!(insert.g > insert.r && insert.g > insert.b, "{id}");
        let delete = scheme
            .style("meld:delete")
            .unwrap()
            .line_background
            .unwrap();
        assert!(delete.r > delete.g && delete.r > delete.b, "{id}");
    }
}

#[test]
fn scheme_dark_detection() {
    for (id, _, variant, ..) in BUNDLED_SCHEMES {
        assert_eq!(scheme(id).is_dark(), variant == "dark", "{id}");
        let expected = if variant == "dark" {
            Variant::Dark
        } else {
            Variant::Light
        };
        assert_eq!(scheme(id).variant(), Some(expected));
    }
    assert!(!scheme("meld-base").is_dark());
    assert!(scheme("meld-dark").is_dark());
}

/// Colours from the PyCharm-monokai theme, as (foreground, bold, italic).
const MONOKAI_STYLES: [(&str, &str, bool, bool); 19] = [
    ("text", "#ffffff", false, false),
    ("def:keyword", "#66d9ef", true, false),
    ("def:builtin", "#66d9ef", false, false),
    ("def:comment", "#75715e", false, false),
    ("def:doc-comment", "#e6db74", false, true),
    ("def:doc-comment-element", "#e6db74", true, true),
    ("def:string", "#e6db74", false, false),
    ("def:special-char", "#ae81ff", false, false),
    ("def:number", "#ae81ff", false, false),
    ("def:operator", "#f92672", false, false),
    ("def:function", "#a6e22e", false, false),
    ("python:keyword", "#66d9ef", true, false),
    ("python:builtin-function", "#66d9ef", false, false),
    ("python:function-name", "#a6e22e", true, false),
    ("python:class-name", "#ffffff", true, false),
    ("python:decorator", "#a6e22e", false, false),
    ("python:special-variable", "#fd971f", false, false),
    ("python:multiline-string", "#e6db74", false, true),
    ("python:escaped-char", "#ae81ff", false, false),
];

#[test]
fn monokai_style_colours() {
    let scheme = scheme("meld-monokai");
    for (id, foreground, bold, italic) in MONOKAI_STYLES {
        let style = scheme.style(id).unwrap();
        assert_eq!(style.foreground.unwrap().to_hex(), foreground, "{id}");
        assert_eq!(style.is_bold(), bold, "{id}");
        assert_eq!(style.is_italic(), italic, "{id}");
    }
}

#[test]
fn monokai_editor_colours() {
    let scheme = scheme("meld-monokai");
    for (id, attr, colour) in [
        ("text", StyleAttr::Background, "#2b2b2b"),
        ("selection", StyleAttr::Background, "#214283"),
        ("current-line", StyleAttr::Background, "#323232"),
        ("line-numbers", StyleAttr::Foreground, "#606366"),
        ("line-numbers", StyleAttr::Background, "#313335"),
        ("bracket-match", StyleAttr::Background, "#344134"),
        ("search-match", StyleAttr::Background, "#40332b"),
        ("def:error", StyleAttr::UnderlineColor, "#ff0000"),
    ] {
        let style = scheme.style(id).unwrap();
        assert_eq!(style.color(attr).unwrap().to_hex(), colour, "{id} {attr:?}");
    }
    assert_eq!(
        scheme.style("def:error").unwrap().underline,
        Some(Underline::Error)
    );
    assert!(scheme.style("def:deletion").unwrap().is_strikethrough());
}

// test_style.py

const BASE_FONT_SIZE: i32 = 11;

fn base_font() -> FontSpec {
    FontSpec::parse(&format!("monospace {BASE_FONT_SIZE}"))
}

#[test]
fn default_size_is_the_configured_font_size() {
    let zoom = FontZoom::default();
    assert_eq!(zoom.scaled_size(base_font().size_points()), BASE_FONT_SIZE);
}

#[test]
fn zoom_steps_by_a_point() {
    let size = base_font().size_points();
    let mut zoom = FontZoom::default();
    let mut sizes = Vec::new();
    for _ in 0..3 {
        zoom.update(Some(1), size);
        sizes.push(zoom.scaled_size(size));
    }
    assert_eq!(
        sizes,
        [BASE_FONT_SIZE + 1, BASE_FONT_SIZE + 2, BASE_FONT_SIZE + 3]
    );
    for _ in 0..3 {
        zoom.update(Some(-1), size);
    }
    assert_eq!(zoom.scaled_size(size), BASE_FONT_SIZE);
}

#[test]
fn zoom_reset() {
    let size = base_font().size_points();
    let mut zoom = FontZoom::default();
    zoom.update(Some(1), size);
    assert_eq!(zoom.scaled_size(size), BASE_FONT_SIZE + 1);
    zoom.update(None, size);
    assert_eq!(zoom.scaled_size(size), BASE_FONT_SIZE);
    zoom.update(Some(2), size);
    assert!(zoom.reset());
    assert!(!zoom.reset());
}

#[test]
fn zoom_reports_changes_correctly() {
    let size = base_font().size_points();
    let mut zoom = FontZoom::default();
    // Zooming is a change
    assert!(zoom.update(Some(1), size));
    // Resetting is a change here, but resetting again is not
    assert!(zoom.update(None, size));
    assert!(!zoom.update(None, size));
    // Neither is zooming out once we've hit the minimum font size
    zoom.update(Some(-BASE_FONT_SIZE), size);
    assert_eq!(zoom.scaled_size(size), 1);
    assert!(!zoom.update(Some(-1), size));
    assert_eq!(zoom.offset(), 1 - BASE_FONT_SIZE);
}

// test_diffpalette.py: meld.style integration

const PALETTES: [DiffColorMode; 2] = [DiffColorMode::RedGreen, DiffColorMode::Colorblind];
const SIDES: [Option<DiffSide>; 3] = [None, Some(DiffSide::Old), Some(DiffSide::New)];

fn theme(id: &str) -> Theme {
    Theme::new(&manager(), id, false)
}

/// `get_common_theme()` as it was before diff palettes.
fn old_common_theme(theme: &Theme) -> DiffColors {
    let c = |name: &str, attr| theme.color(name, attr);
    let chunk = |name: &str| ChunkColors {
        fill: c(name, StyleAttr::Background),
        line: c(name, StyleAttr::LineBackground),
    };
    DiffColors {
        insert: chunk("meld:insert"),
        delete: chunk("meld:insert"),
        replace: chunk("meld:replace"),
        conflict: chunk("meld:conflict"),
        error: chunk("meld:error"),
        inline: c("meld:inline", StyleAttr::Background),
        focus_highlight: c("meld:current-line-highlight", StyleAttr::Foreground),
        current_chunk_highlight: c("meld:current-chunk-highlight", StyleAttr::Background),
        overscroll: c("meld:overscroll", StyleAttr::Background),
        current_line_highlight: c("meld:current-line-highlight", StyleAttr::Background),
        syncpoint_outline: c("meld:syncpoint-outline", StyleAttr::Foreground),
        dimmed: c("meld:dimmed", StyleAttr::Foreground),
        map_overlay: c("map-overlay", StyleAttr::Background),
    }
}

#[test]
fn scheme_setting_keeps_scheme_colours() {
    for id in [
        "meld-base",
        "meld-dark",
        "meld-lens-light",
        "meld-lens-dark",
    ] {
        // Each scheme as is, without adapting to the UI variant
        let theme = Theme::from_scheme(&manager(), scheme(id));
        for side in SIDES {
            let colors = theme.diff_colors(DiffColorMode::Scheme, side);
            assert_eq!(colors, old_common_theme(&theme), "{id} {side:?}");
            assert_eq!(colors, theme.scheme_diff_colors());
        }
    }
}

#[test]
fn palette_setting_overrides_chunk_colours() {
    for palette in PALETTES {
        for (id, dark) in [
            ("meld-base", false),
            ("meld-dark", true),
            ("meld-lens-dark", true),
        ] {
            let theme = Theme::from_scheme(&manager(), scheme(id));
            assert_eq!(theme.is_dark(), dark, "{id}");
            for side in SIDES {
                let expected = get_diff_palette(palette, dark, side).unwrap();
                let colors = theme.diff_colors(palette, side);
                let old = old_common_theme(&theme);

                for tag in PaletteTag::ALL {
                    let tone = expected.tone(tag);
                    let chunk = match tag {
                        PaletteTag::Insert => colors.insert,
                        PaletteTag::Delete => colors.delete,
                        PaletteTag::Replace => colors.replace,
                    };
                    assert!(chunk.fill.approx_eq(Rgba::parse(tone.fill).unwrap()));
                    assert!(chunk.line.approx_eq(Rgba::parse(tone.line).unwrap()));
                }

                // Everything else still comes from the scheme
                assert_eq!(colors.conflict, old.conflict);
                assert_eq!(colors.error, old.error);
                assert_eq!(colors.focus_highlight, old.focus_highlight);
                assert_eq!(colors.current_chunk_highlight, old.current_chunk_highlight);
                assert_eq!(colors.overscroll, old.overscroll);
                assert_eq!(colors.dimmed, old.dimmed);

                assert!(
                    colors
                        .inline
                        .approx_eq(Rgba::parse(expected.inline()).unwrap())
                );
            }
        }
    }
}

#[test]
fn pane_diff_colors_use_default_sides() {
    let theme = theme("meld-base");
    let mode = DiffColorMode::RedGreen;
    assert_eq!(
        theme.pane_diff_colors(mode, 2, 0),
        theme.diff_colors(mode, Some(DiffSide::Old))
    );
    assert_eq!(
        theme.pane_diff_colors(mode, 2, 1),
        theme.diff_colors(mode, Some(DiffSide::New))
    );
    assert_eq!(
        theme.pane_diff_colors(mode, 3, 1),
        theme.diff_colors(mode, None)
    );
    assert_eq!(
        theme.pane_diff_colors(mode, 2, 7),
        theme.diff_colors(mode, None)
    );
}

// set_base_style_scheme / adapt_style_scheme behaviour

#[test]
fn base_scheme_follows_darkness() {
    let manager = manager();
    let light = Theme::new(&manager, "meld-lens-light", false);
    assert_eq!(light.scheme().id(), "meld-lens-light");
    assert_eq!(light.base_scheme().id(), "meld-base");
    assert!(!light.is_dark());

    // Dark UI switches to the dark variant, with the dark base
    let dark = Theme::new(&manager, "meld-lens-light", true);
    assert_eq!(dark.scheme().id(), "meld-lens-dark");
    assert_eq!(dark.base_scheme().id(), "meld-dark");
    assert!(dark.is_dark());

    // The base schemes stand in for each other by darkness
    let base = Theme::new(&manager, "meld-dark", false);
    assert_eq!(base.scheme().id(), "meld-dark");
    assert_eq!(base.base_scheme().id(), "meld-dark");

    // The default "classic" setting resolves
    let classic = Theme::new(&manager, "classic", false);
    assert_eq!(classic.scheme().id(), "classic");
    assert_eq!(classic.base_scheme().id(), "meld-base");
    let classic_dark = Theme::new(&manager, "classic", true);
    assert_eq!(classic_dark.scheme().id(), "classic-dark");
    assert_eq!(classic_dark.base_scheme().id(), "meld-dark");

    // Unknown schemes fall back to classic
    assert_eq!(
        Theme::new(&manager, "no-such-scheme", false).scheme().id(),
        "classic"
    );
}

#[test]
fn fallback_to_base_scheme_colours() {
    // A scheme without meld styles takes them from the base scheme
    let manager = manager();
    let theme = Theme::new(&manager, "classic", false);
    let base = scheme("meld-base");
    let expected = base.style("meld:insert").unwrap().background.unwrap();
    assert_eq!(theme.color("meld:insert", StyleAttr::Background), expected);
    assert_eq!(
        theme.lookup("meld:nonexistent", StyleAttr::Background),
        None
    );
    assert_eq!(
        theme.color("meld:nonexistent", StyleAttr::Background),
        Rgba::TRANSPARENT
    );

    let status = theme.status_colors();
    assert_eq!(status.new.to_hex(), "#008800");
    assert_eq!(status.removed.to_hex(), "#880000");
    assert_eq!(status.unknown.to_hex(), "#888888");
}

#[test]
fn theme_from_settings() {
    let mut settings = meld_config::Settings {
        style_scheme: "meld-lens-dark".into(),
        ..Default::default()
    };
    let manager = manager();
    assert_eq!(
        Theme::from_settings(&manager, &settings, false)
            .scheme()
            .id(),
        "meld-lens-light"
    );
    assert_eq!(
        Theme::from_settings(&manager, &settings, true)
            .scheme()
            .id(),
        "meld-lens-dark"
    );
    settings.style_variant = meld_config::StyleVariant::ForceDark;
    assert_eq!(
        Theme::from_settings(&manager, &settings, false)
            .scheme()
            .id(),
        "meld-lens-dark"
    );
}
