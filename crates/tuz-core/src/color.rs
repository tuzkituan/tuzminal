//! Resolving terminal cell colors against a theme.
//!
//! A cell's color is not an RGB value. It is one of:
//!
//! - a **named** slot (`NamedColor::Red`, `Foreground`, `Cursor`, …), which the
//!   theme defines;
//! - an **indexed** slot into the 256-color palette;
//! - a **spec**, a literal RGB triple the program sent via SGR 38/48.
//!
//! On top of that, the running program may have *redefined* palette slots with
//! OSC 4, and cell flags (`BOLD`, `DIM`, `INVERSE`, `HIDDEN`) modify the result.
//! Getting this order wrong is the difference between a theme that works and one
//! where half of `ls --color` output is unreadable.

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use tuz_config::{Rgba, Theme};

/// Resolved foreground and background for one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellColors {
    pub fg: Rgba,
    pub bg: Rgba,
}

/// Resolve a cell's colors, applying palette overrides and attribute flags.
///
/// `overrides` holds slots the program redefined via OSC 4; they take precedence
/// over the theme, because a program that explicitly set a palette entry is being
/// deliberate and second-guessing it breaks e.g. vim colorschemes.
pub fn resolve(
    theme: &Theme,
    overrides: &Colors,
    fg: AnsiColor,
    bg: AnsiColor,
    flags: Flags,
) -> CellColors {
    let mut fg_rgba = resolve_one(theme, overrides, fg);
    let mut bg_rgba = resolve_one(theme, overrides, bg);

    // Bold promotes the 8 normal ANSI colors to their bright variants, which is
    // the near-universal terminal convention programs are written against.
    if flags.contains(Flags::BOLD) {
        if let AnsiColor::Named(named) = fg {
            if let Some(bright) = brighten(named) {
                fg_rgba = resolve_one(theme, overrides, AnsiColor::Named(bright));
            }
        }
    }

    // DIM blends toward the background rather than scaling toward black, so it
    // stays legible on a light theme.
    if flags.contains(Flags::DIM) {
        fg_rgba = mix(fg_rgba, bg_rgba, 0.4);
    }

    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg_rgba, &mut bg_rgba);
    }

    // HIDDEN (SGR 8) is used by password prompts; the glyph must not be visible
    // but must still occupy its cell.
    if flags.contains(Flags::HIDDEN) {
        fg_rgba = bg_rgba;
    }

    CellColors {
        fg: fg_rgba,
        bg: bg_rgba,
    }
}

/// Resolve the color slot a program asked to read back with OSC 4/10/11/12.
///
/// `alacritty_terminal` reports the query as an index into its *whole* color
/// space rather than into the 256-color palette: 0–255 are palette slots, 256 is
/// the foreground, 257 the background, 258 the cursor, and the rest are the dim
/// and bright variants. Truncating that index to a `u8` is how a background
/// query comes back as palette color 1, so this goes through the same resolver
/// cells use — OSC 4 overrides included, because a program that set a slot
/// expects to read its own value back.
///
/// `None` for an index outside the color space, which is a slot this build knows
/// nothing about; the caller should stay silent rather than answer with a
/// made-up color.
///
/// Shells and editors lean on this: querying the background (OSC 11) is how they
/// decide between a light and a dark palette.
pub fn resolve_query(theme: &Theme, overrides: &Colors, index: usize) -> Option<Rgba> {
    let color = match u8::try_from(index) {
        Ok(i) => AnsiColor::Indexed(i),
        Err(_) => AnsiColor::Named(named_from_index(index)?),
    };
    Some(resolve_one(theme, overrides, color))
}

/// The `NamedColor` a color-space index above the palette stands for.
///
/// Spelled out rather than transmuted: the enum's discriminants are what
/// `alacritty_terminal` indexes its color list by, and a wrong guess here would
/// answer a query with a neighboring slot.
fn named_from_index(index: usize) -> Option<NamedColor> {
    use NamedColor::*;
    Some(match index {
        256 => Foreground,
        257 => Background,
        258 => Cursor,
        259 => DimBlack,
        260 => DimRed,
        261 => DimGreen,
        262 => DimYellow,
        263 => DimBlue,
        264 => DimMagenta,
        265 => DimCyan,
        266 => DimWhite,
        267 => BrightForeground,
        268 => DimForeground,
        _ => return None,
    })
}

fn resolve_one(theme: &Theme, overrides: &Colors, color: AnsiColor) -> Rgba {
    match color {
        // A literal RGB triple from the program: never themed.
        AnsiColor::Spec(rgb) => from_rgb(rgb),
        AnsiColor::Indexed(i) => overrides[i as usize]
            .map(from_rgb)
            .unwrap_or_else(|| theme.indexed_color(i)),
        AnsiColor::Named(named) => {
            // OSC 4 overrides are stored by palette index, so check the slot this
            // name maps onto before falling back to the theme.
            if let Some(rgb) = overrides[named as usize] {
                return from_rgb(rgb);
            }
            named_from_theme(theme, named)
        }
    }
}

fn named_from_theme(theme: &Theme, named: NamedColor) -> Rgba {
    use NamedColor::*;
    match named {
        Black => theme.normal.black,
        Red => theme.normal.red,
        Green => theme.normal.green,
        Yellow => theme.normal.yellow,
        Blue => theme.normal.blue,
        Magenta => theme.normal.magenta,
        Cyan => theme.normal.cyan,
        White => theme.normal.white,

        BrightBlack => theme.bright.black,
        BrightRed => theme.bright.red,
        BrightGreen => theme.bright.green,
        BrightYellow => theme.bright.yellow,
        BrightBlue => theme.bright.blue,
        BrightMagenta => theme.bright.magenta,
        BrightCyan => theme.bright.cyan,
        BrightWhite => theme.bright.white,

        Foreground => theme.foreground,
        Background => theme.background,
        Cursor => theme.cursor(),

        // `DimFg`/`Dim*` are the pre-computed dim variants some programs request
        // explicitly. Blending toward the background matches the DIM flag path,
        // so both routes look the same.
        DimForeground => mix(theme.foreground, theme.background, 0.4),
        DimBlack => mix(theme.normal.black, theme.background, 0.4),
        DimRed => mix(theme.normal.red, theme.background, 0.4),
        DimGreen => mix(theme.normal.green, theme.background, 0.4),
        DimYellow => mix(theme.normal.yellow, theme.background, 0.4),
        DimBlue => mix(theme.normal.blue, theme.background, 0.4),
        DimMagenta => mix(theme.normal.magenta, theme.background, 0.4),
        DimCyan => mix(theme.normal.cyan, theme.background, 0.4),
        DimWhite => mix(theme.normal.white, theme.background, 0.4),

        // `BrightForeground` is what bold text resolves to when the program has
        // not chosen a color.
        BrightForeground => theme.bright.white,
    }
    // Deliberately exhaustive rather than ending in a catch-all: if a future
    // release of alacritty_terminal adds a color slot, this should fail to
    // compile so the new slot gets a real mapping, instead of silently
    // resolving to the default foreground.
}

/// The bright counterpart of a normal ANSI color, if it has one.
fn brighten(named: NamedColor) -> Option<NamedColor> {
    use NamedColor::*;
    Some(match named {
        Black => BrightBlack,
        Red => BrightRed,
        Green => BrightGreen,
        Yellow => BrightYellow,
        Blue => BrightBlue,
        Magenta => BrightMagenta,
        Cyan => BrightCyan,
        White => BrightWhite,
        Foreground => BrightForeground,
        // Already bright, or not a palette color.
        _ => return None,
    })
}

pub fn from_rgb(rgb: Rgb) -> Rgba {
    Rgba::rgb(rgb.r, rgb.g, rgb.b)
}

/// Color for inline ghost text — a suggestion the user has not accepted yet.
///
/// Derived from the theme rather than taken from a palette slot, because the
/// direction has to reverse between a dark theme and a light one and only the
/// theme's own foreground and background know which way that is. It is the same
/// blend-toward-the-background rule as the DIM flag above, pushed slightly further:
/// ghost text should read as secondary even next to genuinely dim output.
///
/// Deliberately not `bright.black`. Several themes put that within a few units of
/// their background, which would make a suggestion invisible in exactly the themes
/// people use.
pub fn inline_hint_color(theme: &Theme) -> Rgba {
    // The same colour secondary text uses, and deliberately shared rather than
    // reimplemented: ghost text *is* secondary text, and two definitions of "dim" drift.
    theme.muted_foreground()
}

/// Linear interpolation in sRGB space.
///
/// Not colorimetrically correct, but it is what every other terminal does for
/// DIM, so matching it keeps output looking the way authors intended.
fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgba::rgba(lerp(a.r, b.r), lerp(a.g, b.g), lerp(a.b, b.b), a.a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::builtin_default()
    }

    fn no_overrides() -> Colors {
        Colors::default()
    }

    fn plain(fg: AnsiColor, bg: AnsiColor) -> CellColors {
        resolve(&theme(), &no_overrides(), fg, bg, Flags::empty())
    }

    #[test]
    fn named_colors_come_from_the_theme() {
        let t = theme();
        let c = plain(
            AnsiColor::Named(NamedColor::Red),
            AnsiColor::Named(NamedColor::Background),
        );
        assert_eq!(c.fg, t.normal.red);
        assert_eq!(c.bg, t.background);
    }

    #[test]
    fn default_foreground_and_background_map_to_theme_defaults() {
        let t = theme();
        let c = plain(
            AnsiColor::Named(NamedColor::Foreground),
            AnsiColor::Named(NamedColor::Background),
        );
        assert_eq!(c.fg, t.foreground);
        assert_eq!(c.bg, t.background);
    }

    #[test]
    fn spec_colors_bypass_the_theme_entirely() {
        // A program that sent an explicit RGB triple must get exactly that.
        let c = plain(
            AnsiColor::Spec(Rgb { r: 1, g: 2, b: 3 }),
            AnsiColor::Named(NamedColor::Background),
        );
        assert_eq!(c.fg, Rgba::rgb(1, 2, 3));
    }

    #[test]
    fn indexed_colors_use_the_theme_palette() {
        let t = theme();
        let c = plain(AnsiColor::Indexed(200), AnsiColor::Indexed(16));
        assert_eq!(c.fg, t.indexed_color(200));
        assert_eq!(c.bg, t.indexed_color(16));
    }

    #[test]
    fn osc4_overrides_win_over_the_theme() {
        // A program that redefined a palette slot is being deliberate; ignoring
        // it breaks vim colorschemes that set up their own palette.
        let mut overrides = no_overrides();
        overrides[1] = Some(Rgb { r: 9, g: 9, b: 9 });

        let c = resolve(
            &theme(),
            &overrides,
            AnsiColor::Named(NamedColor::Red),
            AnsiColor::Named(NamedColor::Background),
            Flags::empty(),
        );
        assert_eq!(c.fg, Rgba::rgb(9, 9, 9));
    }

    #[test]
    fn a_background_query_answers_with_the_background() {
        // The regression this guards: 257 truncated to a `u8` is 1, so an OSC 11
        // query used to come back as palette red and every shell that sniffs the
        // background picked a dark prompt on a light theme.
        let t = theme();
        let index = NamedColor::Background as usize;
        assert_eq!(index, 257, "the color space, not the palette");
        assert_eq!(
            resolve_query(&t, &no_overrides(), index),
            Some(t.background)
        );
        assert_eq!(
            resolve_query(&t, &no_overrides(), NamedColor::Foreground as usize),
            Some(t.foreground)
        );
        assert_eq!(
            resolve_query(&t, &no_overrides(), NamedColor::Cursor as usize),
            Some(t.cursor())
        );
    }

    #[test]
    fn a_palette_query_answers_from_the_palette() {
        let t = theme();
        assert_eq!(resolve_query(&t, &no_overrides(), 1), Some(t.normal.red));
        assert_eq!(
            resolve_query(&t, &no_overrides(), 200),
            Some(t.indexed_color(200))
        );
    }

    #[test]
    fn a_query_reads_back_what_the_program_set() {
        // A program that set the background with OSC 11, or a palette slot with
        // OSC 4, must read its own value back rather than the theme's.
        let mut overrides = no_overrides();
        overrides[NamedColor::Background as usize] = Some(Rgb { r: 7, g: 8, b: 9 });
        overrides[4] = Some(Rgb { r: 1, g: 2, b: 3 });

        assert_eq!(
            resolve_query(&theme(), &overrides, NamedColor::Background as usize),
            Some(Rgba::rgb(7, 8, 9))
        );
        assert_eq!(
            resolve_query(&theme(), &overrides, 4),
            Some(Rgba::rgb(1, 2, 3))
        );
    }

    #[test]
    fn a_query_for_an_unknown_slot_goes_unanswered() {
        // Better silent than confidently wrong: the caller skips the reply.
        assert_eq!(resolve_query(&theme(), &no_overrides(), 269), None);
        assert_eq!(resolve_query(&theme(), &no_overrides(), usize::MAX), None);
    }

    #[test]
    fn bold_promotes_normal_ansi_colors_to_bright() {
        let t = theme();
        let c = resolve(
            &theme(),
            &no_overrides(),
            AnsiColor::Named(NamedColor::Red),
            AnsiColor::Named(NamedColor::Background),
            Flags::BOLD,
        );
        assert_eq!(c.fg, t.bright.red, "bold red should render as bright red");
    }

    #[test]
    fn bold_does_not_alter_an_explicit_rgb_color() {
        // Promoting a Spec color would override an explicit choice.
        let c = resolve(
            &theme(),
            &no_overrides(),
            AnsiColor::Spec(Rgb {
                r: 10,
                g: 20,
                b: 30,
            }),
            AnsiColor::Named(NamedColor::Background),
            Flags::BOLD,
        );
        assert_eq!(c.fg, Rgba::rgb(10, 20, 30));
    }

    #[test]
    fn inverse_swaps_foreground_and_background() {
        let t = theme();
        let c = resolve(
            &theme(),
            &no_overrides(),
            AnsiColor::Named(NamedColor::Foreground),
            AnsiColor::Named(NamedColor::Background),
            Flags::INVERSE,
        );
        assert_eq!(c.fg, t.background);
        assert_eq!(c.bg, t.foreground);
    }

    #[test]
    fn hidden_makes_the_glyph_match_its_background() {
        // Password prompts rely on this; the cell must still occupy space.
        let c = resolve(
            &theme(),
            &no_overrides(),
            AnsiColor::Named(NamedColor::Foreground),
            AnsiColor::Named(NamedColor::Background),
            Flags::HIDDEN,
        );
        assert_eq!(c.fg, c.bg);
    }

    #[test]
    fn hidden_wins_over_inverse() {
        // Order matters: applying HIDDEN before INVERSE would reveal the text.
        let c = resolve(
            &theme(),
            &no_overrides(),
            AnsiColor::Named(NamedColor::Foreground),
            AnsiColor::Named(NamedColor::Background),
            Flags::HIDDEN | Flags::INVERSE,
        );
        assert_eq!(c.fg, c.bg, "hidden text must stay invisible when inverted");
    }

    #[test]
    fn dim_blends_toward_the_background_not_toward_black() {
        // On a light theme, scaling toward black would make dim text *darker*
        // and more prominent, which is backwards.
        let light = Theme::load("tuz-light", &tuz_config::Paths::for_test()).unwrap();
        let c = resolve(
            &light,
            &no_overrides(),
            AnsiColor::Named(NamedColor::Foreground),
            AnsiColor::Named(NamedColor::Background),
            Flags::DIM,
        );

        let dist = |a: Rgba, b: Rgba| {
            (a.r as i32 - b.r as i32).abs()
                + (a.g as i32 - b.g as i32).abs()
                + (a.b as i32 - b.b as i32).abs()
        };
        assert!(
            dist(c.fg, light.background) < dist(light.foreground, light.background),
            "dim should move the foreground closer to the background"
        );
    }

    #[test]
    fn mixing_endpoints_is_exact() {
        let a = Rgba::rgb(0, 0, 0);
        let b = Rgba::rgb(255, 255, 255);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        // Out-of-range factors clamp rather than producing garbage channels.
        assert_eq!(mix(a, b, -1.0), a);
        assert_eq!(mix(a, b, 2.0), b);
    }
}
