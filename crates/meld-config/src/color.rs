//! RGBA colours and the colour-string syntax used by style schemes.
//!
//! This mirrors what `gdk_rgba_parse()` accepts in the places Meld relies
//! on it: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, the 12 and 16 digit hex
//! forms, `rgb()`/`rgba()` with integer or percentage channels, and CSS
//! colour names.

use std::fmt;
use std::str::FromStr;

use crate::ConfigError;

/// A colour with floating point channels in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba::new(0.0, 0.0, 0.0, 0.0);
    #[cfg(test)]
    pub const BLACK: Rgba = Rgba::new(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);

    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Rgba { r, g, b, a }
    }

    /// An opaque colour from 8-bit channels.
    pub fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        Rgba::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
    }

    /// Parse a colour string, see the module documentation for the syntax.
    pub fn parse(spec: &str) -> Option<Rgba> {
        let spec = spec.trim();
        if let Some(hex) = spec.strip_prefix('#') {
            return parse_hex(hex);
        }
        let lower = spec.to_ascii_lowercase();
        if let Some(args) = lower
            .strip_prefix("rgba")
            .or_else(|| lower.strip_prefix("rgb"))
        {
            return parse_rgb_function(args);
        }
        named_colour(&lower)
    }

    /// Same colour with a different alpha.
    pub fn with_alpha(self, a: f32) -> Self {
        Rgba { a, ..self }
    }

    /// The channels as 8-bit values, rounded.
    pub fn to_rgba8(self) -> [u8; 4] {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [c(self.r), c(self.g), c(self.b), c(self.a)]
    }

    /// `#rrggbb` for opaque colours, `#rrggbbaa` otherwise.
    pub fn to_hex(self) -> String {
        let [r, g, b, a] = self.to_rgba8();
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }

    /// Relative luminance (Rec. 709 weights) of the colour channels.
    #[cfg(test)]
    pub fn luminance(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    /// Whether this colour counts as a dark background.
    ///
    /// This is the heuristic from `meld.style.should_use_dark`, as
    /// recommended by Microsoft: `2r + 5g + b < 4`.
    pub fn is_dark(self) -> bool {
        (2.0 * self.r + 5.0 * self.g + self.b) < 4.0
    }

    /// Approximate equality, for colours that went through `f32` maths.
    pub fn approx_eq(self, other: Rgba) -> bool {
        self.to_rgba8() == other.to_rgba8()
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for Rgba {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Rgba::parse(s).ok_or_else(|| ConfigError::InvalidColor(s.to_string()))
    }
}

fn parse_hex(hex: &str) -> Option<Rgba> {
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |s: &str| -> Option<f32> {
        let max = (1u32 << (4 * s.len())) - 1;
        u32::from_str_radix(s, 16)
            .ok()
            .map(|v| v as f32 / max as f32)
    };
    let (digits, has_alpha) = match hex.len() {
        3 => (1, false),
        4 => (1, true),
        6 => (2, false),
        8 => (2, true),
        9 => (3, false),
        12 => (4, false),
        16 => (4, true),
        _ => return None,
    };
    let part = |i: usize| channel(&hex[i * digits..(i + 1) * digits]);
    Some(Rgba::new(
        part(0)?,
        part(1)?,
        part(2)?,
        if has_alpha { part(3)? } else { 1.0 },
    ))
}

fn parse_rgb_function(args: &str) -> Option<Rgba> {
    let inner = args.trim().strip_prefix('(')?.strip_suffix(')')?;
    let parts: Vec<&str> = inner
        .split([',', ' ', '/'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let colour_channel = |p: &str| -> Option<f32> {
        if let Some(pct) = p.strip_suffix('%') {
            pct.trim().parse::<f32>().ok().map(|v| v / 100.0)
        } else {
            p.parse::<f32>().ok().map(|v| v / 255.0)
        }
    };
    let alpha_channel = |p: &str| -> Option<f32> {
        if let Some(pct) = p.strip_suffix('%') {
            pct.trim().parse::<f32>().ok().map(|v| v / 100.0)
        } else {
            p.parse::<f32>().ok()
        }
    };
    let clamp = |v: f32| v.clamp(0.0, 1.0);
    Some(Rgba::new(
        clamp(colour_channel(parts[0])?),
        clamp(colour_channel(parts[1])?),
        clamp(colour_channel(parts[2])?),
        clamp(match parts.get(3) {
            Some(a) => alpha_channel(a)?,
            None => 1.0,
        }),
    ))
}

/// CSS colour keywords.
fn named_colour(name: &str) -> Option<Rgba> {
    if name == "transparent" {
        return Some(Rgba::TRANSPARENT);
    }
    let name = name.replace([' ', '-', '_'], "");
    let hex = NAMED_COLOURS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, hex)| *hex)?;
    let [_, r, g, b] = hex.to_be_bytes();
    Some(Rgba::from_rgb8(r, g, b))
}

#[rustfmt::skip]
const NAMED_COLOURS: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff), ("antiquewhite", 0xfaebd7), ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4), ("azure", 0xf0ffff), ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4), ("black", 0x000000), ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff), ("blueviolet", 0x8a2be2), ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887), ("cadetblue", 0x5f9ea0), ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e), ("coral", 0xff7f50), ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc), ("crimson", 0xdc143c), ("cyan", 0x00ffff),
    ("darkblue", 0x00008b), ("darkcyan", 0x008b8b), ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9), ("darkgreen", 0x006400), ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b), ("darkmagenta", 0x8b008b), ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00), ("darkorchid", 0x9932cc), ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a), ("darkseagreen", 0x8fbc8f), ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f), ("darkslategrey", 0x2f4f4f), ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3), ("deeppink", 0xff1493), ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969), ("dimgrey", 0x696969), ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222), ("floralwhite", 0xfffaf0), ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff), ("gainsboro", 0xdcdcdc), ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700), ("goldenrod", 0xdaa520), ("gray", 0x808080),
    ("green", 0x008000), ("greenyellow", 0xadff2f), ("grey", 0x808080),
    ("honeydew", 0xf0fff0), ("hotpink", 0xff69b4), ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082), ("ivory", 0xfffff0), ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa), ("lavenderblush", 0xfff0f5), ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd), ("lightblue", 0xadd8e6), ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff), ("lightgoldenrodyellow", 0xfafad2), ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90), ("lightgrey", 0xd3d3d3), ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a), ("lightseagreen", 0x20b2aa), ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899), ("lightslategrey", 0x778899), ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0), ("lime", 0x00ff00), ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6), ("magenta", 0xff00ff), ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa), ("mediumblue", 0x0000cd), ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db), ("mediumseagreen", 0x3cb371), ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a), ("mediumturquoise", 0x48d1cc), ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970), ("mintcream", 0xf5fffa), ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5), ("navajowhite", 0xffdead), ("navy", 0x000080),
    ("oldlace", 0xfdf5e6), ("olive", 0x808000), ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500), ("orangered", 0xff4500), ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa), ("palegreen", 0x98fb98), ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093), ("papayawhip", 0xffefd5), ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f), ("pink", 0xffc0cb), ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6), ("purple", 0x800080), ("rebeccapurple", 0x663399),
    ("red", 0xff0000), ("rosybrown", 0xbc8f8f), ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513), ("salmon", 0xfa8072), ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57), ("seashell", 0xfff5ee), ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0), ("skyblue", 0x87ceeb), ("slateblue", 0x6a5acd),
    ("slategray", 0x708090), ("slategrey", 0x708090), ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f), ("steelblue", 0x4682b4), ("tan", 0xd2b48c),
    ("teal", 0x008080), ("thistle", 0xd8bfd8), ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0), ("violet", 0xee82ee), ("wheat", 0xf5deb3),
    ("white", 0xffffff), ("whitesmoke", 0xf5f5f5), ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_forms() {
        assert_eq!(Rgba::parse("#fff"), Some(Rgba::WHITE));
        assert_eq!(Rgba::parse("#000000"), Some(Rgba::BLACK));
        assert_eq!(Rgba::parse("#ff000080").unwrap().to_hex(), "#ff000080");
        assert_eq!(Rgba::parse("#f008").unwrap().to_rgba8(), [255, 0, 0, 136]);
        assert_eq!(Rgba::parse("#ffff00000000").unwrap().to_hex(), "#ff0000");
        assert_eq!(Rgba::parse("#d0ffa3").unwrap().to_hex(), "#d0ffa3");
        assert_eq!(Rgba::parse("#12345"), None);
        assert_eq!(Rgba::parse("#ggg"), None);
        assert_eq!(Rgba::parse("#"), None);
    }

    #[test]
    fn rgb_functions() {
        let c = Rgba::parse("rgba(255, 255, 0, 0.25)").unwrap();
        assert_eq!(c.to_rgba8(), [255, 255, 0, 64]);
        let c = Rgba::parse("rgb(0, 128, 255)").unwrap();
        assert_eq!(c.to_rgba8(), [0, 128, 255, 255]);
        let c = Rgba::parse("rgb(100%, 50%, 0%)").unwrap();
        assert_eq!(c.to_rgba8(), [255, 128, 0, 255]);
        let c = Rgba::parse("RGBA(1,2,3,50%)").unwrap();
        assert_eq!(c.to_rgba8(), [1, 2, 3, 128]);
        assert_eq!(Rgba::parse("rgba(1,2)"), None);
        assert_eq!(Rgba::parse("rgb(1,2,3"), None);
    }

    #[test]
    fn named() {
        assert_eq!(Rgba::parse("red").unwrap().to_hex(), "#ff0000");
        assert_eq!(Rgba::parse("Dark Gray").unwrap().to_hex(), "#a9a9a9");
        assert_eq!(Rgba::parse("transparent"), Some(Rgba::TRANSPARENT));
        assert_eq!(Rgba::parse("no-such-colour"), None);
        assert!("bogus".parse::<Rgba>().is_err());
    }

    #[test]
    fn darkness() {
        assert!(Rgba::BLACK.is_dark());
        assert!(!Rgba::WHITE.is_dark());
        assert!(Rgba::parse("#1e1e1e").unwrap().is_dark());
        assert!(!Rgba::parse("#fdf6e3").unwrap().is_dark());
    }
}
