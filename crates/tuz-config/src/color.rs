//! Color parsing and colorspace conversion.
//!
//! Colors are authored in config as sRGB hex strings and stored as 8-bit sRGB.
//! The GPU needs linear-space floats, so conversion happens at the boundary in
//! [`Rgba::to_linear`] rather than being baked into the stored value.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// An 8-bit-per-channel sRGB color with straight (non-premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 0xff }
    }

    // Named for symmetry with `rgb` above; the pair reads better at call sites
    // than a `rgb`/`new` split would.
    #[allow(clippy::self_named_constructors)]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(0xff, 0xff, 0xff);
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    /// Parse `#rgb`, `#rrggbb`, `#rrggbbaa`, or the `0x`-prefixed equivalents.
    ///
    /// The leading `#` or `0x` is required — a bare `ff0000` is far more likely
    /// to be a mistake than an intentional color, so we reject it rather than
    /// silently guessing.
    /// `#rrggbb`, the form [`parse`](Self::parse) accepts and status segments carry.
    ///
    /// Round-trips through `parse`, which is what makes it usable as an override in a
    /// `StatusItem` rather than only for display.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    pub fn parse(s: &str) -> Result<Self, ColorParseError> {
        let hex = s
            .strip_prefix('#')
            .or_else(|| s.strip_prefix("0x"))
            .or_else(|| s.strip_prefix("0X"))
            .ok_or_else(|| ColorParseError::MissingPrefix(s.to_owned()))?;

        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ColorParseError::NotHex(s.to_owned()));
        }

        // `#rgb` shorthand expands each nibble by repetition (f -> ff), which is
        // the CSS rule and what users expect.
        let nib = |i: usize| -> u8 {
            let c = hex.as_bytes()[i];
            let v = (c as char).to_digit(16).unwrap_or(0) as u8;
            v << 4 | v
        };
        let byte = |i: usize| -> u8 { u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) };

        Ok(match hex.len() {
            3 => Self::rgb(nib(0), nib(1), nib(2)),
            6 => Self::rgb(byte(0), byte(2), byte(4)),
            8 => Self::rgba(byte(0), byte(2), byte(4), byte(6)),
            _ => return Err(ColorParseError::BadLength(s.to_owned())),
        })
    }

    /// Convert to linear-space RGBA floats for the GPU.
    ///
    /// Required whenever the surface format is `*Srgb`: wgpu treats clear colors
    /// and vertex-supplied colors as linear and applies the sRGB encode on
    /// write, so handing it raw sRGB values washes everything out.
    pub fn to_linear(self) -> [f32; 4] {
        [
            srgb_to_linear(self.r),
            srgb_to_linear(self.g),
            srgb_to_linear(self.b),
            self.a as f32 / 255.0,
        ]
    }

    /// Channel values as-is, normalized to `0.0..=1.0` without any transfer
    /// function applied. Use for non-`Srgb` surface formats.
    pub fn to_unorm(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }

    /// Replace the alpha channel, keeping the color.
    pub fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }
}

/// sRGB electro-optical transfer function (the piecewise gamma ~2.4 curve).
fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 0xff {
            write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            write!(
                f,
                "#{:02x}{:02x}{:02x}{:02x}",
                self.r, self.g, self.b, self.a
            )
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ColorParseError {
    #[error("color `{0}` must start with `#` or `0x`")]
    MissingPrefix(String),
    #[error("color `{0}` contains non-hexadecimal characters")]
    NotHex(String),
    #[error("color `{0}` must have 3, 6, or 8 hex digits")]
    BadLength(String),
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgba::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Rgba {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_through_parse() {
        for color in [
            Rgba::rgb(0, 0, 0),
            Rgba::rgb(255, 255, 255),
            Rgba::rgb(0x5c, 0xf0, 0xd4),
            Rgba::rgb(1, 2, 3),
        ] {
            let hex = color.to_hex();
            assert_eq!(Rgba::parse(&hex).unwrap(), color, "{hex}");
        }
    }

    #[test]
    fn parses_all_accepted_forms() {
        assert_eq!(Rgba::parse("#f00").unwrap(), Rgba::rgb(0xff, 0, 0));
        assert_eq!(Rgba::parse("#ff0000").unwrap(), Rgba::rgb(0xff, 0, 0));
        assert_eq!(Rgba::parse("0xff0000").unwrap(), Rgba::rgb(0xff, 0, 0));
        assert_eq!(
            Rgba::parse("#1a2b3c80").unwrap(),
            Rgba::rgba(0x1a, 0x2b, 0x3c, 0x80)
        );
    }

    #[test]
    fn shorthand_expands_by_nibble_repetition() {
        // CSS semantics: #abc == #aabbcc, not #a0b0c0.
        assert_eq!(Rgba::parse("#abc").unwrap(), Rgba::rgb(0xaa, 0xbb, 0xcc));
    }

    #[test]
    fn rejects_malformed_colors() {
        assert!(matches!(
            Rgba::parse("ff0000"),
            Err(ColorParseError::MissingPrefix(_))
        ));
        assert!(matches!(
            Rgba::parse("#gg0000"),
            Err(ColorParseError::NotHex(_))
        ));
        assert!(matches!(
            Rgba::parse("#ff00"),
            Err(ColorParseError::BadLength(_))
        ));
    }

    #[test]
    fn linear_conversion_hits_known_endpoints() {
        let [r, _, _, a] = Rgba::rgb(0, 0, 0).to_linear();
        assert!(r.abs() < 1e-6);
        assert!((a - 1.0).abs() < 1e-6);

        let [r, ..] = Rgba::rgb(255, 255, 255).to_linear();
        assert!((r - 1.0).abs() < 1e-6);

        // Mid-gray sRGB 128 sits near 0.216 in linear space, well below the
        // naive 0.502 — this is the bug the conversion exists to prevent.
        let [r, ..] = Rgba::rgb(128, 128, 128).to_linear();
        assert!((r - 0.2158).abs() < 1e-3, "got {r}");
    }

    #[test]
    fn display_roundtrips_through_parse() {
        for c in [
            Rgba::rgb(0x12, 0x34, 0x56),
            Rgba::rgba(0x12, 0x34, 0x56, 0x78),
        ] {
            assert_eq!(Rgba::parse(&c.to_string()).unwrap(), c);
        }
    }
}
