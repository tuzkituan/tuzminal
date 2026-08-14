//! Theme definition and resolution.
//!
//! A theme is a standalone TOML file so it can be installed, shared and swapped
//! independently of the user's own config. Resolution order is: user theme dir
//! (`$XDG_CONFIG_HOME/tuzminal/themes`), then installed themes
//! (`$XDG_DATA_HOME/tuzminal/themes`), then the themes built into the binary.
//! User files shadow installed ones, which shadow built-ins.

use crate::color::Rgba;
use crate::paths::Paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Themes compiled into the binary so a fresh install always renders correctly,
/// even with no theme files on disk.
/// Order matters only for the picker, which shows them in this order: the two
/// house themes first, then the widely-known palettes alphabetically.
pub const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("tuz-dark", include_str!("../themes/tuz-dark.toml")),
    ("tuz-light", include_str!("../themes/tuz-light.toml")),
    (
        "catppuccin-mocha",
        include_str!("../themes/catppuccin-mocha.toml"),
    ),
    ("dracula", include_str!("../themes/dracula.toml")),
    ("gruvbox-dark", include_str!("../themes/gruvbox-dark.toml")),
    ("nord", include_str!("../themes/nord.toml")),
    ("one-dark", include_str!("../themes/one-dark.toml")),
    (
        "solarized-dark",
        include_str!("../themes/solarized-dark.toml"),
    ),
    (
        "solarized-light",
        include_str!("../themes/solarized-light.toml"),
    ),
    ("tokyo-night", include_str!("../themes/tokyo-night.toml")),
];

pub const DEFAULT_THEME: &str = "tuz-dark";

/// The 16 ANSI colors, indices 0-7 normal and 8-15 bright.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AnsiPalette {
    pub black: Rgba,
    pub red: Rgba,
    pub green: Rgba,
    pub yellow: Rgba,
    pub blue: Rgba,
    pub magenta: Rgba,
    pub cyan: Rgba,
    pub white: Rgba,
}

impl AnsiPalette {
    /// Index into the palette in ANSI order (0=black .. 7=white).
    pub fn get(&self, i: u8) -> Rgba {
        match i & 0x7 {
            0 => self.black,
            1 => self.red,
            2 => self.green,
            3 => self.yellow,
            4 => self.blue,
            5 => self.magenta,
            6 => self.cyan,
            _ => self.white,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    /// Display name. Defaults to the file stem when absent.
    #[serde(default)]
    pub name: String,

    pub background: Rgba,
    pub foreground: Rgba,

    /// Cursor body color. Falls back to `foreground`.
    #[serde(default)]
    pub cursor: Option<Rgba>,
    /// Color of the glyph *under* the cursor. Falls back to `background`.
    #[serde(default)]
    pub cursor_text: Option<Rgba>,

    #[serde(default)]
    pub selection_background: Option<Rgba>,
    #[serde(default)]
    pub selection_foreground: Option<Rgba>,

    /// Background of the pane that currently has focus. When set, unfocused
    /// panes keep `background`, which makes the active split obvious.
    #[serde(default)]
    pub background_focused: Option<Rgba>,

    /// Color of the divider drawn between splits.
    #[serde(default)]
    pub split_divider: Option<Rgba>,

    /// Outline drawn around the window edge. Falls back to a mix of `background`
    /// and `foreground`, which lands on the right side of either a dark or a light
    /// theme without the theme having to say so.
    #[serde(default)]
    pub window_border: Option<Rgba>,

    pub normal: AnsiPalette,
    pub bright: AnsiPalette,

    /// Overrides for individual slots of the 256-color cube. Rarely needed —
    /// indices 16-255 are generated procedurally when absent.
    #[serde(default)]
    pub indexed: BTreeMap<u8, Rgba>,
}

impl Theme {
    pub fn cursor(&self) -> Rgba {
        self.cursor.unwrap_or(self.foreground)
    }
    pub fn cursor_text(&self) -> Rgba {
        self.cursor_text.unwrap_or(self.background)
    }
    pub fn selection_background(&self) -> Rgba {
        self.selection_background.unwrap_or(self.foreground)
    }
    pub fn selection_foreground(&self) -> Rgba {
        self.selection_foreground.unwrap_or(self.background)
    }
    pub fn background_focused(&self) -> Rgba {
        self.background_focused.unwrap_or(self.background)
    }
    pub fn split_divider(&self) -> Rgba {
        self.split_divider.unwrap_or(self.normal.black)
    }

    /// The window's outline.
    ///
    /// A borderless window has no frame from the compositor, so against a wallpaper
    /// of a similar tone its edge simply is not there. This is that edge.
    ///
    /// Derived rather than a fixed grey because the direction has to reverse between
    /// themes: on a dark theme the edge must be lighter than the window to be seen,
    /// on a light theme darker. Stepping a fifth of the way from `background` toward
    /// `foreground` does both, since a theme's foreground is by definition the thing
    /// that contrasts with its background.
    pub fn window_border(&self) -> Rgba {
        self.window_border.unwrap_or_else(|| {
            let t = 0.2;
            let step = |bg: u8, fg: u8| (bg as f32 + (fg as f32 - bg as f32) * t).round() as u8;
            Rgba::rgb(
                step(self.background.r, self.foreground.r),
                step(self.background.g, self.foreground.g),
                step(self.background.b, self.foreground.b),
            )
        })
    }

    /// Secondary text and quiet UI furniture: a shortcut hint, an unfocused tab's title,
    /// a placeholder, a scrollbar thumb.
    ///
    /// Derived, and this is the third attempt at it — the first two were palette slots and
    /// both were wrong for half the themes that exist. `bright.black` is the chrome's own
    /// grey, so anything drawn in it vanished against the strip. `normal.white` fixed that
    /// on dark themes and broke it completely on light ones, where near-white text on a
    /// white page is invisible.
    ///
    /// A palette slot cannot work, because "dim" is not a colour — it is a *relationship*
    /// to the background, and which direction that runs is exactly what a light and a dark
    /// theme disagree about. Blending the foreground toward the background is the only
    /// definition that holds in both, the same reasoning [`Theme::window_border`] uses.
    pub fn muted_foreground(&self) -> Rgba {
        let t = 0.45;
        let step = |fg: u8, bg: u8| (fg as f32 + (bg as f32 - fg as f32) * t).round() as u8;
        Rgba::rgb(
            step(self.foreground.r, self.background.r),
            step(self.foreground.g, self.background.g),
            step(self.foreground.b, self.background.b),
        )
    }

    /// A rule separating groups of controls inside the toolbar strip.
    ///
    /// Cannot be [`Theme::split_divider`], despite the name: that colour *is* the
    /// strip's own background, so a rule painted in it is invisible. Derived from the
    /// strip toward the foreground for the same reason [`Theme::window_border`] is —
    /// the direction has to reverse between a dark theme and a light one, and only the
    /// theme's own foreground knows which way that is.
    ///
    /// A third of the way rather than a fifth: this sits on the strip's colour rather
    /// than on the window background, and the two are close enough that a fifth does
    /// not separate anything.
    pub fn chrome_divider(&self) -> Rgba {
        let strip = self.split_divider();
        let t = 0.33;
        let step = |bg: u8, fg: u8| (bg as f32 + (fg as f32 - bg as f32) * t).round() as u8;
        Rgba::rgb(
            step(strip.r, self.foreground.r),
            step(strip.g, self.foreground.g),
            step(strip.b, self.foreground.b),
        )
    }

    /// Resolve any of the 256 indexed colors.
    ///
    /// 0-15 come from the ANSI palettes, 16-231 from the 6x6x6 cube, and
    /// 232-255 from the grayscale ramp — all overridable via `[indexed]`.
    pub fn indexed_color(&self, i: u8) -> Rgba {
        if let Some(&c) = self.indexed.get(&i) {
            return c;
        }
        match i {
            0..=7 => self.normal.get(i),
            8..=15 => self.bright.get(i - 8),
            16..=231 => {
                // Standard xterm cube: level 0 is 0, levels 1-5 are 95+40*(n-1).
                let n = i - 16;
                let level = |v: u8| -> u8 {
                    if v == 0 {
                        0
                    } else {
                        95 + 40 * (v - 1)
                    }
                };
                Rgba::rgb(level(n / 36), level((n % 36) / 6), level(n % 6))
            }
            232..=255 => {
                let v = 8 + 10 * (i - 232);
                Rgba::rgb(v, v, v)
            }
        }
    }

    fn parse(src: &str, fallback_name: &str) -> Result<Self, ThemeError> {
        let mut theme: Theme = toml::from_str(src).map_err(|e| ThemeError::Parse {
            name: fallback_name.to_owned(),
            source: Box::new(e),
        })?;
        if theme.name.is_empty() {
            theme.name = fallback_name.to_owned();
        }
        Ok(theme)
    }

    /// Load a theme by name, searching user dir -> data dir -> built-ins.
    pub fn load(name: &str, paths: &Paths) -> Result<Self, ThemeError> {
        // Reject path separators: a theme name comes from config and must not be
        // able to escape the theme directories.
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(ThemeError::InvalidName(name.to_owned()));
        }

        for dir in paths.theme_dirs() {
            let path = dir.join(format!("{name}.toml"));
            match std::fs::read_to_string(&path) {
                Ok(src) => {
                    log::debug!("loaded theme `{name}` from {}", path.display());
                    return Theme::parse(&src, name);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(ThemeError::Io { path, source: e }),
            }
        }

        if let Some((_, src)) = BUILTIN_THEMES.iter().find(|(n, _)| *n == name) {
            log::debug!("loaded built-in theme `{name}`");
            return Theme::parse(src, name);
        }

        Err(ThemeError::NotFound {
            name: name.to_owned(),
            searched: paths.theme_dirs().to_vec(),
        })
    }

    /// Load a theme from an explicit file path, bypassing name resolution.
    pub fn load_path(path: &Path) -> Result<Self, ThemeError> {
        let src = std::fs::read_to_string(path).map_err(|e| ThemeError::Io {
            path: path.to_owned(),
            source: e,
        })?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed");
        Theme::parse(&src, stem)
    }

    /// The built-in default, guaranteed to parse. Panics only if a bundled
    /// theme file is malformed, which a unit test prevents from shipping.
    pub fn builtin_default() -> Self {
        let (name, src) = BUILTIN_THEMES
            .iter()
            .find(|(n, _)| *n == DEFAULT_THEME)
            .expect("DEFAULT_THEME must be present in BUILTIN_THEMES");
        Theme::parse(src, name).expect("bundled default theme must parse")
    }

    /// Every theme available on this system, deduplicated by name with earlier
    /// search paths winning. Used by `tuzminal theme list`.
    pub fn available(paths: &Paths) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();

        for dir in paths.theme_dirs() {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if seen.insert(stem.to_owned()) {
                        names.push(stem.to_owned());
                    }
                }
            }
        }
        for (name, _) in BUILTIN_THEMES {
            if seen.insert((*name).to_owned()) {
                names.push((*name).to_owned());
            }
        }
        names.sort();
        names
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::builtin_default()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme `{name}` not found (searched {})", searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    NotFound {
        name: String,
        searched: Vec<PathBuf>,
    },
    #[error("theme name `{0}` is invalid: must not contain path separators")]
    InvalidName(String),
    #[error("failed to read theme at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("theme `{name}` is malformed: {source}")]
    Parse {
        name: String,
        #[source]
        source: Box<toml::de::Error>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_theme_parses() {
        // Guards the `expect` in builtin_default() and catches a typo'd bundled
        // theme at test time instead of at the user's first launch.
        for (name, src) in BUILTIN_THEMES {
            let theme = Theme::parse(src, name)
                .unwrap_or_else(|e| panic!("bundled theme `{name}` failed: {e}"));
            assert_eq!(
                &theme.name, name,
                "theme `{name}` declares a mismatched name"
            );
        }
    }

    /// Per-channel distance, which is what "visibly different" means for these colours.
    fn distance(a: Rgba, b: Rgba) -> i32 {
        (a.r as i32 - b.r as i32).abs()
            + (a.g as i32 - b.g as i32).abs()
            + (a.b as i32 - b.b as i32).abs()
    }

    #[test]
    fn secondary_text_is_legible_on_every_bundled_theme() {
        // The bug this exists to prevent: `muted_foreground` was a palette slot twice, and
        // both choices vanished on half the themes — `bright.black` against the chrome's
        // own grey, then `normal.white` against a light theme's white page. Asserted across
        // every bundled theme, light ones included, because one theme passing proves
        // nothing about the direction the blend runs.
        for (name, src) in BUILTIN_THEMES {
            let theme = Theme::parse(src, name).expect("bundled themes parse");
            let muted = theme.muted_foreground();

            // Far enough from the background to be read at all...
            let from_bg = distance(muted, theme.background);
            assert!(
                from_bg > 60,
                "`{name}`: secondary text is {from_bg} from its background, so it is invisible"
            );
            // ...and far enough from the foreground to read as secondary rather than as
            // ordinary text someone forgot to style.
            let from_fg = distance(muted, theme.foreground);
            assert!(
                from_fg > 60,
                "`{name}`: secondary text is only {from_fg} from the foreground"
            );
        }
    }

    #[test]
    fn the_toolbar_divider_is_visible_against_the_strip_it_sits_on() {
        // `split_divider` *is* the strip's background, so a rule painted in it would be
        // invisible — the trap this accessor exists to avoid.
        for (name, src) in BUILTIN_THEMES {
            let theme = Theme::parse(src, name).expect("bundled themes parse");
            let apart = distance(theme.chrome_divider(), theme.split_divider());
            assert!(
                apart > 30,
                "`{name}`: the divider is {apart} from the strip behind it"
            );
        }
    }

    #[test]
    fn builtin_default_is_available() {
        let t = Theme::builtin_default();
        assert_eq!(t.name, DEFAULT_THEME);
    }

    #[test]
    fn optional_colors_fall_back_to_sensible_bases() {
        let mut t = Theme::builtin_default();
        t.cursor = None;
        t.cursor_text = None;
        t.background_focused = None;
        assert_eq!(t.cursor(), t.foreground);
        assert_eq!(t.cursor_text(), t.background);
        assert_eq!(t.background_focused(), t.background);
    }

    #[test]
    fn indexed_colors_follow_the_xterm_cube() {
        let t = Theme::builtin_default();
        // 16 is the cube origin (pure black), 231 its opposite corner.
        assert_eq!(t.indexed_color(16), Rgba::rgb(0, 0, 0));
        assert_eq!(t.indexed_color(231), Rgba::rgb(255, 255, 255));
        // First non-zero level is 95, not 51 — a classic off-by-one here makes
        // every 256-color app look subtly wrong.
        assert_eq!(t.indexed_color(17), Rgba::rgb(0, 0, 95));
        // Grayscale ramp endpoints.
        assert_eq!(t.indexed_color(232), Rgba::rgb(8, 8, 8));
        assert_eq!(t.indexed_color(255), Rgba::rgb(238, 238, 238));
    }

    #[test]
    fn indexed_colors_0_through_15_come_from_ansi_palettes() {
        let t = Theme::builtin_default();
        assert_eq!(t.indexed_color(1), t.normal.red);
        assert_eq!(t.indexed_color(9), t.bright.red);
    }

    #[test]
    fn explicit_indexed_overrides_win() {
        let mut t = Theme::builtin_default();
        t.indexed.insert(200, Rgba::rgb(1, 2, 3));
        assert_eq!(t.indexed_color(200), Rgba::rgb(1, 2, 3));
    }

    #[test]
    fn theme_names_cannot_traverse_directories() {
        let paths = Paths::for_test();
        for bad in ["../secret", "sub/theme", "..", r"a\b"] {
            assert!(
                matches!(Theme::load(bad, &paths), Err(ThemeError::InvalidName(_))),
                "`{bad}` should be rejected"
            );
        }
    }

    #[test]
    fn unknown_theme_reports_where_it_looked() {
        let paths = Paths::for_test();
        let err = Theme::load("definitely-not-a-theme", &paths).unwrap_err();
        assert!(matches!(err, ThemeError::NotFound { .. }));
    }

    #[test]
    fn available_includes_builtins() {
        let names = Theme::available(&Paths::for_test());
        assert!(names.contains(&DEFAULT_THEME.to_owned()));
    }
}

#[cfg(test)]
mod border_tests {
    use super::*;

    #[test]
    fn the_fallback_border_contrasts_with_the_background_either_way() {
        // The point of deriving it: the step has to go up on a dark theme and down on
        // a light one, and a fixed grey can only do one of those.
        let mut dark = Theme::builtin_default();
        dark.background = Rgba::rgb(0x10, 0x10, 0x10);
        dark.foreground = Rgba::rgb(0xf0, 0xf0, 0xf0);
        dark.window_border = None;
        assert!(dark.window_border().r > dark.background.r);

        let mut light = Theme::builtin_default();
        light.background = Rgba::rgb(0xf0, 0xf0, 0xf0);
        light.foreground = Rgba::rgb(0x20, 0x20, 0x20);
        light.window_border = None;
        assert!(light.window_border().r < light.background.r);
    }

    #[test]
    fn the_fallback_stays_nearer_the_background_than_the_foreground() {
        // An outline is meant to be noticed and not read. Landing halfway would draw a
        // frame in the text colour.
        let mut t = Theme::builtin_default();
        t.background = Rgba::rgb(0, 0, 0);
        t.foreground = Rgba::rgb(0xff, 0xff, 0xff);
        t.window_border = None;
        let v = t.window_border().r;
        assert!(v > 0, "invisible against the background");
        assert!(
            v < 0x80,
            "{v} is closer to the foreground than the background"
        );
    }

    #[test]
    fn an_explicit_border_wins_over_the_fallback() {
        let mut t = Theme::builtin_default();
        t.window_border = Some(Rgba::rgb(1, 2, 3));
        assert_eq!(t.window_border(), Rgba::rgb(1, 2, 3));
    }

    #[test]
    fn the_fallback_is_opaque() {
        // Drawn under the background quad, so a translucent outline would show the
        // cleared surface through the window edge rather than the desktop.
        let mut t = Theme::builtin_default();
        t.window_border = None;
        assert_eq!(t.window_border().a, 0xff);
    }
}
