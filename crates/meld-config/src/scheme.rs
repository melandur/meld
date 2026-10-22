//! GtkSourceView style scheme loading.
//!
//! Parses `*.style-scheme.xml` files: `<color>` palettes, `<metadata>`
//! properties, `<style>` elements with colour and font attributes,
//! `use-style` references and `parent-scheme` inheritance.
//!
//! Colour attributes follow GtkSourceView's rules: a value starting with
//! `#` is a literal colour (GtkSourceView first tries the value without
//! the `#`, which is how Meld's `#rgba(…)` values work), anything else is
//! a palette colour name looked up in the scheme and then its parents.
//! As an extension, an unknown name that is a CSS colour name is used as
//! that colour.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::{ConfigError, Rgba};

/// Meld's bundled schemes, from `data/styles`.
const BUNDLED: &[(&str, &str)] = &[
    (
        "meld-base",
        include_str!("../../../data/styles/meld-base.style-scheme.xml.in"),
    ),
    (
        "meld-dark",
        include_str!("../../../data/styles/meld-dark.style-scheme.xml.in"),
    ),
    (
        "meld-lens-light",
        include_str!("../../../data/styles/meld-lens-light.style-scheme.xml.in"),
    ),
    (
        "meld-lens-dark",
        include_str!("../../../data/styles/meld-lens-dark.style-scheme.xml.in"),
    ),
    (
        "meld-monokai",
        include_str!("../../../data/styles/meld-monokai.style-scheme.xml.in"),
    ),
];

/// Stand-ins for GtkSourceView's built-in schemes, which GtkSourceView 5
/// compiles into the library rather than installing as files. They are
/// used only when no installed scheme with the same id is found, so that
/// `classic` (the default `style-scheme`) and the parents of meld-base
/// and meld-dark always resolve.
const FALLBACK: &[(&str, &str)] = &[
    ("classic", include_str!("../schemes/classic.xml")),
    ("classic-dark", include_str!("../schemes/classic-dark.xml")),
    (
        "solarized-light",
        include_str!("../schemes/solarized-light.xml"),
    ),
    (
        "solarized-dark",
        include_str!("../schemes/solarized-dark.xml"),
    ),
];

/// Underline styles of a [`Style`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Underline {
    None,
    Single,
    Double,
    Low,
    Error,
}

impl Underline {
    fn parse(value: &str) -> Option<Underline> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "false" | "0" => Some(Self::None),
            "single" | "true" | "1" => Some(Self::Single),
            "double" => Some(Self::Double),
            "low" => Some(Self::Low),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// A resolved style: unset attributes are `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub foreground: Option<Rgba>,
    pub background: Option<Rgba>,
    pub line_background: Option<Rgba>,
    pub underline_color: Option<Rgba>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<Underline>,
    pub strikethrough: Option<bool>,
    /// Font scale factor, from a number or a Pango scale name.
    pub scale: Option<f32>,
}

impl Style {
    /// Whether the style is bold (bold set and true).
    pub fn is_bold(&self) -> bool {
        self.bold == Some(true)
    }
    pub fn is_italic(&self) -> bool {
        self.italic == Some(true)
    }
    pub fn is_strikethrough(&self) -> bool {
        self.strikethrough == Some(true)
    }

    /// A colour attribute of the style.
    pub fn color(&self, attribute: StyleAttr) -> Option<Rgba> {
        match attribute {
            StyleAttr::Foreground => self.foreground,
            StyleAttr::Background => self.background,
            StyleAttr::LineBackground => self.line_background,
            StyleAttr::UnderlineColor => self.underline_color,
        }
    }
}

/// The colour attributes of a style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StyleAttr {
    Foreground,
    Background,
    LineBackground,
    UnderlineColor,
}

#[derive(Debug, Clone, Default)]
struct RawStyle {
    foreground: Option<String>,
    background: Option<String>,
    line_background: Option<String>,
    underline_color: Option<String>,
    bold: Option<String>,
    italic: Option<String>,
    underline: Option<String>,
    strikethrough: Option<String>,
    scale: Option<String>,
    use_style: Option<String>,
}

/// Where a scheme was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemeSource {
    /// One of Meld's bundled schemes, embedded in the binary.
    Bundled,
    /// A stand-in for a GtkSourceView built-in scheme.
    Fallback,
    /// A scheme file.
    File(PathBuf),
    /// Added with [`SchemeManager::add_scheme_xml`].
    Memory,
}

#[derive(Debug)]
struct RawScheme {
    id: String,
    name: String,
    description: String,
    authors: Vec<String>,
    parent_id: Option<String>,
    metadata: BTreeMap<String, String>,
    colors: HashMap<String, String>,
    styles: HashMap<String, RawStyle>,
    source: SchemeSource,
}

/// Light or dark, from a scheme's `variant` metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    Light,
    Dark,
}

/// Summary of an available scheme.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemeInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Whether the scheme is dark, see [`StyleScheme::is_dark`].
    pub is_dark: bool,
    pub source: SchemeSource,
}

/// A style scheme with its parent chain resolved.
#[derive(Debug, Clone)]
pub struct StyleScheme {
    raw: Arc<RawScheme>,
    parent: Option<Box<StyleScheme>>,
}

const MAX_DEPTH: usize = 32;

impl StyleScheme {
    /// Parse a standalone scheme (without parent resolution).
    pub fn from_xml(xml: &str) -> Result<StyleScheme, ConfigError> {
        Ok(StyleScheme {
            raw: Arc::new(parse_scheme(xml, SchemeSource::Memory)?),
            parent: None,
        })
    }

    pub fn id(&self) -> &str {
        &self.raw.id
    }
    pub fn name(&self) -> &str {
        &self.raw.name
    }
    pub fn description(&self) -> &str {
        &self.raw.description
    }
    pub fn authors(&self) -> &[String] {
        &self.raw.authors
    }
    pub fn source(&self) -> &SchemeSource {
        &self.raw.source
    }
    /// The `parent-scheme` id, whether or not it was found.
    pub fn parent_id(&self) -> Option<&str> {
        self.raw.parent_id.as_deref()
    }
    /// The resolved parent scheme, if it was found.
    pub fn parent(&self) -> Option<&StyleScheme> {
        self.parent.as_deref()
    }

    /// A `<metadata>` property, e.g. "variant" or "dark-variant".
    pub fn metadata(&self, name: &str) -> Option<&str> {
        self.raw.metadata.get(name).map(String::as_str)
    }

    /// The `variant` metadata.
    pub fn variant(&self) -> Option<Variant> {
        match self.metadata("variant")? {
            "dark" => Some(Variant::Dark),
            "light" => Some(Variant::Light),
            _ => None,
        }
    }

    /// Names of the styles this scheme and its parents define.
    pub fn style_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut scheme = Some(self);
        while let Some(s) = scheme {
            for name in s.raw.styles.keys() {
                if seen.insert(name.clone()) {
                    names.push(name.clone());
                }
            }
            scheme = s.parent.as_deref();
        }
        names.sort();
        names
    }

    /// Whether this scheme itself (not a parent) defines a style.
    pub fn defines_style(&self, name: &str) -> bool {
        self.raw.styles.contains_key(name)
    }

    /// Look up a style, falling back on parent schemes.
    pub fn style(&self, name: &str) -> Option<Style> {
        self.style_depth(name, 0)
    }

    fn style_depth(&self, name: &str, depth: usize) -> Option<Style> {
        if depth > MAX_DEPTH {
            log::warn!("Style reference loop at '{name}' in scheme '{}'", self.id());
            return None;
        }
        match self.raw.styles.get(name) {
            Some(raw) => {
                if let Some(other) = &raw.use_style {
                    return self.style_depth(other, depth + 1);
                }
                Some(self.resolve(raw))
            }
            None => self.parent.as_ref()?.style_depth(name, depth + 1),
        }
    }

    fn resolve(&self, raw: &RawStyle) -> Style {
        let color = |v: &Option<String>| v.as_deref().and_then(|v| self.resolve_color(v));
        Style {
            foreground: color(&raw.foreground),
            background: color(&raw.background),
            line_background: color(&raw.line_background),
            underline_color: color(&raw.underline_color),
            bold: raw.bold.as_deref().and_then(parse_bool),
            italic: raw.italic.as_deref().and_then(parse_bool),
            underline: raw.underline.as_deref().and_then(Underline::parse),
            strikethrough: raw.strikethrough.as_deref().and_then(parse_bool),
            scale: raw.scale.as_deref().and_then(parse_scale),
        }
    }

    /// Resolve a colour attribute value in the context of this scheme.
    pub fn resolve_color(&self, value: &str) -> Option<Rgba> {
        let value = value.trim();
        if value.starts_with('#') {
            let colour = parse_literal_color(value);
            if colour.is_none() {
                log::warn!("Could not parse color '{value}' in scheme '{}'", self.id());
            }
            return colour;
        }
        if let Some(palette_value) = self.palette_color(value) {
            return parse_literal_color(palette_value).or_else(|| Rgba::parse(palette_value));
        }
        let colour = Rgba::parse(value);
        if colour.is_none() {
            log::warn!("No color named '{value}' in scheme '{}'", self.id());
        }
        colour
    }

    fn palette_color(&self, name: &str) -> Option<&str> {
        let mut scheme = Some(self);
        while let Some(s) = scheme {
            if let Some(v) = s.raw.colors.get(name) {
                return Some(v);
            }
            scheme = s.parent.as_deref();
        }
        None
    }

    /// Whether the scheme is dark (`meld.style.should_use_dark`).
    ///
    /// Variant metadata is trusted when present. Otherwise the luminance
    /// of the `text` style's background decides; schemes without one
    /// (like "classic") are treated as light, standing in for the GTK
    /// theme background.
    pub fn is_dark(&self) -> bool {
        self.is_dark_with_theme_background(Rgba::WHITE)
    }

    /// Like [`StyleScheme::is_dark`], with the theme background to use
    /// when the scheme sets no text background.
    pub fn is_dark_with_theme_background(&self, theme_background: Rgba) -> bool {
        if let Some(variant) = self.variant() {
            return variant == Variant::Dark;
        }
        self.style("text")
            .and_then(|s| s.background)
            .unwrap_or(theme_background)
            .is_dark()
    }

    pub fn info(&self) -> SchemeInfo {
        SchemeInfo {
            id: self.raw.id.clone(),
            name: self.raw.name.clone(),
            description: self.raw.description.clone(),
            is_dark: self.is_dark(),
            source: self.raw.source.clone(),
        }
    }
}

/// A `#`-prefixed colour: GtkSourceView tries the value without the `#`
/// first (so `#rgba(…)` and `#red` work), then with it.
fn parse_literal_color(value: &str) -> Option<Rgba> {
    match value.strip_prefix('#') {
        Some(rest) => Rgba::parse(rest)
            .filter(|_| !rest.bytes().all(|b| b.is_ascii_hexdigit()))
            .or_else(|| Rgba::parse(value)),
        None => None,
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn parse_scale(value: &str) -> Option<f32> {
    let named = match value.trim().to_ascii_lowercase().as_str() {
        "xx-small" => 0.578_703_7,
        "x-small" => 0.694_444_4,
        "small" => 0.833_333_3,
        "medium" => 1.0,
        "large" => 1.2,
        "x-large" => 1.44,
        "xx-large" => 1.728,
        other => return other.parse().ok(),
    };
    Some(named)
}

fn attr_value(e: &BytesStart, name: &[u8]) -> Result<Option<String>, ConfigError> {
    for attr in e.attributes() {
        let attr = attr.map_err(|err| ConfigError::InvalidScheme(err.to_string()))?;
        if attr.key.as_ref() == name {
            let value = attr
                .unescape_value()
                .map_err(|err| ConfigError::InvalidScheme(err.to_string()))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

/// Either `name` or the translatable `_name` attribute.
fn attr_or_translatable(e: &BytesStart, name: &str) -> Result<Option<String>, ConfigError> {
    if let Some(v) = attr_value(e, name.as_bytes())? {
        return Ok(Some(v));
    }
    attr_value(e, format!("_{name}").as_bytes())
}

#[derive(PartialEq)]
enum TextTarget {
    None,
    Author,
    Description,
    Property(String),
}

fn parse_scheme(xml: &str, source: SchemeSource) -> Result<RawScheme, ConfigError> {
    let mut reader = Reader::from_str(xml);
    let mut scheme: Option<RawScheme> = None;
    let mut target = TextTarget::None;
    let mut text = String::new();
    let invalid = |e: quick_xml::Error| ConfigError::InvalidScheme(e.to_string());

    loop {
        let event = reader.read_event().map_err(invalid)?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(event, Event::Empty(_));
                match e.name().as_ref() {
                    b"style-scheme" => {
                        let id = attr_value(e, b"id")?
                            .ok_or_else(|| ConfigError::InvalidScheme("scheme has no id".into()))?;
                        let name = attr_or_translatable(e, "name")?.unwrap_or_else(|| id.clone());
                        scheme = Some(RawScheme {
                            id,
                            name,
                            description: String::new(),
                            authors: Vec::new(),
                            parent_id: attr_value(e, b"parent-scheme")?,
                            metadata: BTreeMap::new(),
                            colors: HashMap::new(),
                            styles: HashMap::new(),
                            source: source.clone(),
                        });
                    }
                    _ if scheme.is_none() => {
                        return Err(ConfigError::InvalidScheme(
                            "root element is not <style-scheme>".into(),
                        ));
                    }
                    b"author" if !is_empty => target = TextTarget::Author,
                    b"description" | b"_description" if !is_empty => {
                        target = TextTarget::Description
                    }
                    b"property" if !is_empty => {
                        if let Some(name) = attr_value(e, b"name")? {
                            target = TextTarget::Property(name);
                        }
                    }
                    b"color" => {
                        if let (Some(name), Some(value)) =
                            (attr_value(e, b"name")?, attr_value(e, b"value")?)
                            && let Some(s) = scheme.as_mut()
                        {
                            s.colors.insert(name, value);
                        }
                    }
                    b"style" => {
                        let Some(name) = attr_value(e, b"name")? else {
                            continue;
                        };
                        let style = RawStyle {
                            foreground: attr_value(e, b"foreground")?,
                            background: attr_value(e, b"background")?,
                            line_background: attr_value(e, b"line-background")?,
                            underline_color: attr_value(e, b"underline-color")?,
                            bold: attr_value(e, b"bold")?,
                            italic: attr_value(e, b"italic")?,
                            underline: attr_value(e, b"underline")?,
                            strikethrough: attr_value(e, b"strikethrough")?,
                            scale: attr_value(e, b"scale")?,
                            use_style: attr_value(e, b"use-style")?,
                        };
                        if let Some(s) = scheme.as_mut() {
                            s.styles.insert(name, style);
                        }
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Text(t) => {
                if target != TextTarget::None {
                    text.push_str(
                        &t.decode()
                            .map_err(|e| ConfigError::InvalidScheme(e.to_string()))?,
                    );
                }
            }
            Event::CData(t) => {
                if target != TextTarget::None {
                    text.push_str(
                        &t.decode()
                            .map_err(|e| ConfigError::InvalidScheme(e.to_string()))?,
                    );
                }
            }
            Event::GeneralRef(r) => {
                if target != TextTarget::None {
                    let name = r
                        .decode()
                        .map_err(|e| ConfigError::InvalidScheme(e.to_string()))?;
                    let entity = format!("&{name};");
                    match quick_xml::escape::unescape(&entity) {
                        Ok(s) => text.push_str(&s),
                        Err(_) => text.push_str(&entity),
                    }
                }
            }
            Event::End(_) => {
                if let Some(s) = scheme.as_mut() {
                    let value = text.trim().to_string();
                    match std::mem::replace(&mut target, TextTarget::None) {
                        TextTarget::Author => s.authors.push(value),
                        TextTarget::Description => s.description = value,
                        TextTarget::Property(name) => {
                            s.metadata.insert(name, value);
                        }
                        TextTarget::None => {}
                    }
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    scheme.ok_or_else(|| ConfigError::InvalidScheme("no <style-scheme> element".into()))
}

/// Finds, loads and resolves style schemes, like
/// `GtkSource.StyleSchemeManager`.
#[derive(Debug, Clone, Default)]
pub struct SchemeManager {
    schemes: HashMap<String, Arc<RawScheme>>,
}

impl SchemeManager {
    /// A manager with Meld's bundled schemes, the installed and user
    /// GtkSourceView schemes, `~/.local/share/meld/styles`, and stand-ins
    /// for GtkSourceView's built-in schemes.
    pub fn new() -> SchemeManager {
        Self::with_search_path(&Self::default_search_path())
    }

    /// Only the embedded schemes (bundled and stand-ins).
    pub fn builtin() -> SchemeManager {
        Self::with_search_path(&[])
    }

    /// The embedded schemes plus the schemes in `dirs`.
    ///
    /// When several sources define the same id, the first wins in this
    /// order: `~/.local/share/meld/styles` (if in `dirs`), Meld's bundled
    /// schemes, the other `dirs` in order, and the stand-ins.
    pub fn with_search_path(dirs: &[PathBuf]) -> SchemeManager {
        let mut manager = SchemeManager::default();
        let meld_user = meld_user_styles_dir();
        let (meld_dirs, other_dirs): (Vec<&PathBuf>, Vec<&PathBuf>) =
            dirs.iter().partition(|d| Some(*d) == meld_user.as_ref());

        for dir in meld_dirs {
            manager.load_dir(dir, false);
        }
        for (id, xml) in BUNDLED {
            manager.insert_xml(xml, SchemeSource::Bundled, false, id);
        }
        for dir in other_dirs {
            manager.load_dir(dir, false);
        }
        for (id, xml) in FALLBACK {
            manager.insert_xml(xml, SchemeSource::Fallback, false, id);
        }
        manager
    }

    /// Directories scanned by [`SchemeManager::new`], highest priority
    /// first: Meld's user styles, user GtkSourceView styles, then the
    /// system ones (`$XDG_DATA_DIRS`), newest GtkSourceView first.
    pub fn default_search_path() -> Vec<PathBuf> {
        const VERSIONS: [&str; 3] = ["gtksourceview-5", "gtksourceview-4", "gtksourceview-3.0"];
        let mut dirs = Vec::new();
        dirs.extend(meld_user_styles_dir());
        if let Some(data) = dirs::data_dir() {
            for v in VERSIONS {
                dirs.push(data.join(v).join("styles"));
            }
        }
        let system: Vec<PathBuf> = match std::env::var_os("XDG_DATA_DIRS") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v).collect(),
            _ => vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ],
        };
        for v in VERSIONS {
            for d in &system {
                dirs.push(d.join(v).join("styles"));
            }
        }
        dirs
    }

    fn insert_xml(
        &mut self,
        xml: &str,
        source: SchemeSource,
        replace: bool,
        what: &str,
    ) -> Option<String> {
        match parse_scheme(xml, source) {
            Ok(raw) => {
                let id = raw.id.clone();
                if replace || !self.schemes.contains_key(&id) {
                    self.schemes.insert(id.clone(), Arc::new(raw));
                }
                Some(id)
            }
            Err(e) => {
                log::warn!("Could not load style scheme {what}: {e}");
                None
            }
        }
    }

    fn load_dir(&mut self, dir: &Path, replace: bool) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "xml"))
            .collect();
        paths.sort();
        for path in paths {
            match std::fs::read_to_string(&path) {
                Ok(xml) => {
                    self.insert_xml(
                        &xml,
                        SchemeSource::File(path.clone()),
                        replace,
                        &path.display().to_string(),
                    );
                }
                Err(e) => log::warn!("Could not read style scheme {}: {e}", path.display()),
            }
        }
    }

    /// Load every scheme in a directory, replacing schemes with the same
    /// ids (like `prepend_search_path`).
    pub fn add_dir(&mut self, dir: impl AsRef<Path>) {
        self.load_dir(dir.as_ref(), true);
    }

    /// Add a scheme from XML text, replacing any scheme with its id.
    /// Returns the scheme's id.
    pub fn add_scheme_xml(&mut self, xml: &str) -> Result<String, ConfigError> {
        let raw = parse_scheme(xml, SchemeSource::Memory)?;
        let id = raw.id.clone();
        self.schemes.insert(id.clone(), Arc::new(raw));
        Ok(id)
    }

    /// Ids of all schemes, sorted.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.schemes.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// All schemes, sorted by name.
    pub fn list(&self) -> Vec<SchemeInfo> {
        let mut infos: Vec<SchemeInfo> = self
            .schemes
            .keys()
            .filter_map(|id| self.get(id))
            .map(|s| s.info())
            .collect();
        infos.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        infos
    }

    /// Get a scheme with its parent chain resolved. Missing parents are
    /// logged and left out.
    pub fn get(&self, id: &str) -> Option<StyleScheme> {
        self.build(id, &mut Vec::new())
    }

    fn build(&self, id: &str, chain: &mut Vec<String>) -> Option<StyleScheme> {
        let raw = self.schemes.get(id)?.clone();
        chain.push(id.to_string());
        let parent = match &raw.parent_id {
            Some(pid) if chain.iter().any(|c| c == pid) => {
                log::warn!("Style scheme parent loop: {} -> {pid}", chain.join(" -> "));
                None
            }
            Some(pid) => {
                let parent = self.build(pid, chain);
                if parent.is_none() {
                    log::warn!("Parent scheme '{pid}' of '{id}' not found");
                }
                parent.map(Box::new)
            }
            None => None,
        };
        chain.pop();
        Some(StyleScheme { raw, parent })
    }

    /// Pick the scheme variant matching the UI (`adapt_style_scheme`).
    ///
    /// Schemes with `variant` metadata that doesn't match `want_dark`
    /// are swapped for their `dark-variant`/`light-variant`, if that
    /// scheme exists.
    pub fn adapt(&self, scheme: StyleScheme, want_dark: bool) -> StyleScheme {
        let desired = if want_dark { "dark" } else { "light" };
        let variant = scheme.metadata("variant");
        let other = scheme.metadata(&format!("{desired}-variant"));
        match (variant, other) {
            (Some(v), Some(other)) if v != desired => self.get(other).unwrap_or(scheme),
            _ => scheme,
        }
    }
}

fn meld_user_styles_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("meld").join("styles"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_colours() {
        assert_eq!(parse_literal_color("#ffffff"), Some(Rgba::WHITE));
        assert_eq!(parse_literal_color("#red").unwrap().to_hex(), "#ff0000");
        // "#add" must stay a hex colour, not be read as a name
        assert_eq!(parse_literal_color("#add").unwrap().to_hex(), "#aadddd");
        let c = parse_literal_color("#rgba(200, 200, 200, 0.25)").unwrap();
        assert_eq!(c.to_rgba8(), [200, 200, 200, 64]);
        assert_eq!(parse_literal_color("#nonsense"), None);
    }

    #[test]
    fn parse_and_inherit() {
        let mut manager = SchemeManager::default();
        manager
            .add_scheme_xml(
                r##"<style-scheme id="parent" _name="Parent &amp; co" version="1.0">
                  <_description>The parent</_description>
                  <color name="blue" value="#0000ff"/>
                  <style name="text" foreground="blue" background="#000"/>
                  <style name="def:comment" foreground="blue" italic="true"/>
                </style-scheme>"##,
            )
            .unwrap();
        manager
            .add_scheme_xml(
                r##"<?xml version="1.0"?>
                <style-scheme id="child" name="Child" parent-scheme="parent">
                  <author>A</author><author>B</author>
                  <metadata><property name="variant">light</property></metadata>
                  <color name="green" value="#00ff00"/>
                  <style name="def:keyword" foreground="green" bold="true" underline="error"
                         underline-color="blue" strikethrough="false" scale="large"/>
                  <style name="def:string" foreground="#rgba(1,2,3,0.5)" line-background="lime"/>
                  <style name="def:alias" use-style="def:comment"/>
                  <style name="def:loop" use-style="def:loop"/>
                  <style name="def:bad" foreground="nonexistent" background="#zz"/>
                </style-scheme>"##,
            )
            .unwrap();

        let child = manager.get("child").unwrap();
        assert_eq!(child.name(), "Child");
        assert_eq!(child.authors(), ["A", "B"]);
        assert_eq!(child.parent().unwrap().name(), "Parent & co");
        assert_eq!(child.parent().unwrap().description(), "The parent");
        assert_eq!(child.variant(), Some(Variant::Light));
        assert!(!child.is_dark());

        let kw = child.style("def:keyword").unwrap();
        assert_eq!(kw.foreground.unwrap().to_hex(), "#00ff00");
        assert!(kw.is_bold());
        assert_eq!(kw.italic, None);
        assert_eq!(kw.underline, Some(Underline::Error));
        // Palette names resolve through parents
        assert_eq!(kw.underline_color.unwrap().to_hex(), "#0000ff");
        assert_eq!(kw.strikethrough, Some(false));
        assert_eq!(kw.scale, Some(1.2));

        let s = child.style("def:string").unwrap();
        assert_eq!(s.foreground.unwrap().to_rgba8(), [1, 2, 3, 128]);
        assert_eq!(s.line_background.unwrap().to_hex(), "#00ff00");

        // Inherited from the parent
        let comment = child.style("def:comment").unwrap();
        assert!(comment.is_italic());
        assert_eq!(child.style("text").unwrap().background, Some(Rgba::BLACK));
        assert_eq!(child.style("def:alias"), Some(comment));
        assert_eq!(child.style("def:loop"), None);
        let bad = child.style("def:bad").unwrap();
        assert_eq!((bad.foreground, bad.background), (None, None));
        assert_eq!(child.style("missing"), None);
        assert!(child.style_names().contains(&"text".to_string()));

        // Without variant metadata, the text background decides
        assert!(manager.get("parent").unwrap().is_dark());
    }

    #[test]
    fn parent_loops_and_missing_parents() {
        let mut manager = SchemeManager::default();
        manager
            .add_scheme_xml(r#"<style-scheme id="a" parent-scheme="b"/>"#)
            .unwrap();
        manager
            .add_scheme_xml(r#"<style-scheme id="b" parent-scheme="a"/>"#)
            .unwrap();
        manager
            .add_scheme_xml(r#"<style-scheme id="c" parent-scheme="nope"/>"#)
            .unwrap();
        let a = manager.get("a").unwrap();
        assert_eq!(a.parent().unwrap().id(), "b");
        assert!(a.parent().unwrap().parent().is_none());
        assert!(a.style("anything").is_none());
        let c = manager.get("c").unwrap();
        assert_eq!(c.parent_id(), Some("nope"));
        assert!(c.parent().is_none());
    }

    #[test]
    fn invalid_xml() {
        assert!(StyleScheme::from_xml("<not-a-scheme/>").is_err());
        assert!(StyleScheme::from_xml("<style-scheme name='x'/>").is_err());
        assert!(StyleScheme::from_xml("<style-scheme id='x'><style").is_err());
        assert!(StyleScheme::from_xml("").is_err());
    }

    #[test]
    fn directories_and_priority() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mine.xml"),
            r##"<style-scheme id="mine" name="Mine"><style name="text" background="#111111"/></style-scheme>"##,
        )
        .unwrap();
        // A system copy of a bundled scheme doesn't replace Meld's own
        std::fs::write(
            dir.path().join("meld-base.xml"),
            r#"<style-scheme id="meld-base" name="Impostor"/>"#,
        )
        .unwrap();
        // An installed classic replaces the stand-in
        std::fs::write(
            dir.path().join("classic.xml"),
            r#"<style-scheme id="classic" name="Installed Classic"/>"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.xml"), "<style-scheme").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "").unwrap();

        let manager = SchemeManager::with_search_path(&[dir.path().to_path_buf()]);
        let mine = manager.get("mine").unwrap();
        assert!(mine.is_dark());
        assert_eq!(
            mine.source(),
            &SchemeSource::File(dir.path().join("mine.xml"))
        );
        assert_eq!(manager.get("meld-base").unwrap().name(), "Classic (Meld)");
        assert_eq!(manager.get("classic").unwrap().name(), "Installed Classic");

        let info = manager.list().into_iter().find(|i| i.id == "mine").unwrap();
        assert_eq!(info.name, "Mine");
        assert!(info.is_dark);

        let mut manager = SchemeManager::builtin();
        manager.add_dir(dir.path());
        assert_eq!(manager.get("meld-base").unwrap().name(), "Impostor");
    }

    #[test]
    fn builtin_schemes() {
        let manager = SchemeManager::builtin();
        let ids = manager.ids();
        for id in [
            "meld-base",
            "meld-dark",
            "meld-lens-light",
            "meld-lens-dark",
            "meld-monokai",
            "classic",
            "classic-dark",
            "solarized-light",
            "solarized-dark",
        ] {
            assert!(ids.contains(&id.to_string()), "{id}");
        }
        let base = manager.get("meld-base").unwrap();
        assert_eq!(base.parent().unwrap().id(), "classic");
        assert!(!base.is_dark());
        let dark = manager.get("meld-dark").unwrap();
        assert_eq!(dark.parent().unwrap().id(), "solarized-dark");
        assert!(dark.is_dark());
        let list = manager.list();
        assert_eq!(list.len(), ids.len());
        assert!(list.iter().any(|i| i.id == "classic-dark" && i.is_dark));
        assert!(list.iter().any(|i| i.id == "classic" && !i.is_dark));
    }

    #[test]
    fn adapt_variants() {
        let manager = SchemeManager::builtin();
        let get = |id| manager.get(id).unwrap();
        assert_eq!(
            manager.adapt(get("meld-lens-light"), true).id(),
            "meld-lens-dark"
        );
        assert_eq!(
            manager.adapt(get("meld-lens-light"), false).id(),
            "meld-lens-light"
        );
        assert_eq!(
            manager.adapt(get("meld-lens-dark"), false).id(),
            "meld-lens-light"
        );
        assert_eq!(
            manager.adapt(get("meld-monokai"), false).id(),
            "meld-lens-light"
        );
        assert_eq!(manager.adapt(get("classic"), true).id(), "classic-dark");
        // No variant metadata: kept
        assert_eq!(manager.adapt(get("meld-base"), true).id(), "meld-base");
    }
}
