//! Drawing text at arbitrary pixel positions.
//!
//! The cell renderer in [`crate::instance`] draws a fixed grid, which is right for
//! terminal content and wrong for chrome: a tab title or a status segment sits at a
//! pixel offset and needs to be measured, centered and truncated.
//!
//! Glyph advance comes from the cell width rather than each glyph's own advance. The
//! font is monospace, so they agree, and using the cell width keeps chrome text
//! aligned with the grid below it — which is what makes the tab bar look like part
//! of the same terminal rather than a separate widget.

use crate::instance::{ColorSpace, Instance};
use tuz_config::Rgba;
use tuz_font::{FontSystem, Style};

/// Appended when text is truncated.
///
/// A real ellipsis rather than three dots: it occupies one cell, so a truncated
/// label loses one character instead of three.
const ELLIPSIS: char = '…';

/// Width in pixels that `text` would occupy.
///
/// Counts wide characters as two cells, so a CJK tab title is measured correctly
/// rather than overflowing its tab.
pub fn measure(text: &str, cell_width: f32) -> f32 {
    text.chars().map(|c| char_cells(c) as f32).sum::<f32>() * cell_width
}

/// How many cells a character occupies.
fn char_cells(c: char) -> u16 {
    use unicode_width::UnicodeWidthChar;
    // Control and combining characters report 0 or None; treating them as zero
    // matches how the grid stores them.
    c.width().unwrap_or(0) as u16
}

/// Shorten `text` so it fits `max_width`, appending an ellipsis when it does.
///
/// Returns the input unchanged when it already fits, so the common case allocates
/// nothing.
pub fn truncate(text: &str, max_width: f32, cell_width: f32) -> std::borrow::Cow<'_, str> {
    if measure(text, cell_width) <= max_width {
        return std::borrow::Cow::Borrowed(text);
    }
    // Reserve room for the ellipsis, or the "truncated" text still overflows.
    let budget = max_width - cell_width;
    if budget <= 0.0 {
        return std::borrow::Cow::Borrowed("");
    }

    let mut used = 0.0;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let width = char_cells(c) as f32 * cell_width;
        if used + width > budget {
            break;
        }
        used += width;
        out.push(c);
    }
    out.push(ELLIPSIS);
    std::borrow::Cow::Owned(out)
}

/// Where text sits horizontally within a box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Append instances for `text`, returning the width drawn.
///
/// `baseline` is the y coordinate of the text baseline, not the top of the line:
/// glyph bitmaps are positioned relative to it, so passing a top edge puts text a
/// descender too low.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    text: &str,
    x: f32,
    baseline: f32,
    color: Rgba,
    colors: ColorSpace,
    style: Style,
) -> f32 {
    let cell_width = fonts.metrics().width as f32;
    let rgba = colors.convert_opaque(color);
    let mut pen = x;

    for c in text.chars() {
        let cells = char_cells(c);
        // A zero-width character composes onto the previous glyph; for chrome text
        // it can simply be skipped.
        if cells == 0 {
            continue;
        }

        if c != ' ' {
            if let Some((font, glyph_id)) = fonts.font_for_char(c, style) {
                if let Some(glyph) = fonts.rasterize(font, glyph_id, style) {
                    if !glyph.is_blank() {
                        out.push(Instance::textured(
                            pen + glyph.left,
                            baseline - glyph.top,
                            glyph.rect.width as f32,
                            glyph.rect.height as f32,
                            glyph.uv,
                            rgba,
                            glyph.color,
                        ));
                    }
                }
            }
        }
        pen += cells as f32 * cell_width;
    }
    pen - x
}

/// Draw `text` inside `rect`, truncated to fit and vertically centered.
///
/// The single entry point chrome should use: it handles the three things that are
/// easy to get subtly wrong — truncation, baseline placement and alignment.
#[allow(clippy::too_many_arguments)]
pub fn draw_in_box(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    text: &str,
    rect: tuz_layout::Rect,
    padding: f32,
    align: Align,
    color: Rgba,
    colors: ColorSpace,
    style: Style,
) -> f32 {
    let metrics = fonts.metrics();
    let cell_width = metrics.width as f32;

    let available = (rect.width as f32 - padding * 2.0).max(0.0);
    if available <= 0.0 {
        return 0.0;
    }

    let shown = truncate(text, available, cell_width);
    let width = measure(&shown, cell_width);

    let x = match align {
        Align::Left => rect.x as f32 + padding,
        // `max` so text wider than its box still starts at the left edge rather
        // than being pushed off it.
        Align::Center => rect.x as f32 + ((rect.width as f32 - width) / 2.0).max(padding),
        Align::Right => rect.x as f32 + (rect.width as f32 - width - padding).max(padding),
    };

    // Center the line box, then place the baseline within it by ascent. Using the
    // ascent rather than half the height keeps text optically centered when a font
    // has large descenders.
    let slack = ((rect.height as f32 - metrics.height as f32) / 2.0).max(0.0);
    let baseline = rect.y as f32 + slack + metrics.ascent;

    draw(out, fonts, &shown, x, baseline, color, colors, style)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: f32 = 10.0;

    fn fonts() -> FontSystem {
        FontSystem::new(
            &tuz_config::Font {
                family: "monospace".to_owned(),
                size: 14.0,
                ..Default::default()
            },
            1.0,
        )
        .expect("a monospace font is required for these tests")
    }

    fn colors() -> ColorSpace {
        ColorSpace {
            srgb: false,
            opacity: 1.0,
        }
    }

    #[test]
    fn ascii_measures_one_cell_per_character() {
        assert_eq!(measure("abc", CELL), 30.0);
        assert_eq!(measure("", CELL), 0.0);
    }

    #[test]
    fn wide_characters_measure_two_cells() {
        // Measuring CJK as one cell each is what makes a tab title overflow its tab.
        assert_eq!(measure("日本", CELL), 40.0);
        assert_eq!(measure("a日", CELL), 30.0);
    }

    #[test]
    fn combining_marks_add_no_width() {
        assert_eq!(measure("e\u{0301}", CELL), 10.0);
    }

    #[test]
    fn text_that_fits_is_returned_unchanged_and_unallocated() {
        let out = truncate("abc", 100.0, CELL);
        assert_eq!(out, "abc");
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn overlong_text_is_truncated_to_something_that_actually_fits() {
        // The result must fit the budget *including* the ellipsis, or the label
        // still overflows and the truncation was pointless.
        let out = truncate("abcdefghij", 50.0, CELL);
        assert!(out.ends_with('…'), "got {out:?}");
        assert!(
            measure(&out, CELL) <= 50.0,
            "`{out}` measures {} against a 50px budget",
            measure(&out, CELL)
        );
        assert_eq!(out, "abcd…");
    }

    #[test]
    fn truncation_accounts_for_wide_characters() {
        let out = truncate("日本語", 40.0, CELL);
        assert!(measure(&out, CELL) <= 40.0, "`{out}` is too wide");
        assert!(out.ends_with('…'));
    }

    #[test]
    fn a_budget_too_small_for_an_ellipsis_yields_nothing() {
        // Better to draw nothing than an ellipsis that itself overflows.
        assert_eq!(truncate("abc", 5.0, CELL), "");
        assert_eq!(truncate("abc", 0.0, CELL), "");
    }

    #[test]
    fn drawing_produces_one_instance_per_visible_glyph() {
        let mut fonts = fonts();
        let expected = fonts.metrics().width as f32 * 3.0;
        let mut out = Vec::new();
        let width = draw(
            &mut out,
            &mut fonts,
            "abc",
            0.0,
            20.0,
            Rgba::WHITE,
            colors(),
            Style::Regular,
        );

        assert_eq!(out.len(), 3);
        assert_eq!(width, expected);
    }

    #[test]
    fn spaces_advance_without_emitting_an_instance() {
        let mut fonts = fonts();
        let expected = fonts.metrics().width as f32 * 3.0;
        let mut out = Vec::new();
        let width = draw(
            &mut out,
            &mut fonts,
            "a b",
            0.0,
            20.0,
            Rgba::WHITE,
            colors(),
            Style::Regular,
        );

        assert_eq!(out.len(), 2, "the space should draw nothing");
        assert_eq!(width, expected, "but it must still advance");
    }

    #[test]
    fn glyphs_advance_left_to_right_from_the_given_origin() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        draw(
            &mut out,
            &mut fonts,
            "abc",
            100.0,
            20.0,
            Rgba::WHITE,
            colors(),
            Style::Regular,
        );

        let xs: Vec<f32> = out.iter().map(|i| i.position[0]).collect();
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "got {xs:?}");
        assert!(xs[0] >= 100.0);
    }

    #[test]
    fn box_alignment_places_text_where_asked() {
        let mut fonts = fonts();
        let cell = fonts.metrics().width as f32;
        let rect = tuz_layout::Rect::new(0, 0, 200, 30);

        let x_of = |align: Align, fonts: &mut FontSystem| {
            let mut out = Vec::new();
            draw_in_box(
                &mut out,
                fonts,
                "ab",
                rect,
                4.0,
                align,
                Rgba::WHITE,
                colors(),
                Style::Regular,
            );
            out.first().map(|i| i.position[0]).unwrap_or(0.0)
        };

        let left = x_of(Align::Left, &mut fonts);
        let center = x_of(Align::Center, &mut fonts);
        let right = x_of(Align::Right, &mut fonts);

        assert!(left < center && center < right, "{left} {center} {right}");
        let expected_center = (200.0 - cell * 2.0) / 2.0;
        assert!(
            (center - expected_center).abs() < cell,
            "center {center} vs expected {expected_center}"
        );
    }

    #[test]
    fn text_is_vertically_centered_within_its_box() {
        let mut fonts = fonts();
        let metrics = fonts.metrics();
        // A box much taller than the line, so centering is measurable.
        let rect = tuz_layout::Rect::new(0, 0, 200, metrics.height * 3);

        let mut out = Vec::new();
        draw_in_box(
            &mut out,
            &mut fonts,
            "Ay",
            rect,
            0.0,
            Align::Left,
            Rgba::WHITE,
            colors(),
            Style::Regular,
        );

        let top = out.iter().map(|i| i.position[1]).fold(f32::MAX, f32::min);
        let bottom = out
            .iter()
            .map(|i| i.position[1] + i.size[1])
            .fold(f32::MIN, f32::max);

        let gap_above = top - rect.y as f32;
        let gap_below = rect.bottom() as f32 - bottom;
        assert!(gap_above > 0.0 && gap_below > 0.0, "text should be inset");
        // Not exact: the baseline sits by ascent, not by geometric center.
        assert!(
            (gap_above - gap_below).abs() < metrics.height as f32,
            "lopsided: {gap_above} above, {gap_below} below"
        );
    }

    #[test]
    fn a_zero_width_box_draws_nothing() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        let width = draw_in_box(
            &mut out,
            &mut fonts,
            "hello",
            tuz_layout::Rect::new(0, 0, 0, 20),
            4.0,
            Align::Left,
            Rgba::WHITE,
            colors(),
            Style::Regular,
        );
        assert_eq!(width, 0.0);
        assert!(out.is_empty());
    }

    #[test]
    fn overlong_text_in_a_box_stays_inside_it() {
        let mut fonts = fonts();
        let cell = fonts.metrics().width as f32;
        let rect = tuz_layout::Rect::new(0, 0, (cell * 5.0) as u32, 20);

        let mut out = Vec::new();
        draw_in_box(
            &mut out,
            &mut fonts,
            "a very long tab title indeed",
            rect,
            0.0,
            Align::Left,
            Rgba::WHITE,
            colors(),
            Style::Regular,
        );

        let right_edge = out
            .iter()
            .map(|i| i.position[0] + i.size[0])
            .fold(f32::MIN, f32::max);
        assert!(
            right_edge <= rect.right() as f32 + 1.0,
            "text ran to {right_edge}, past the box edge {}",
            rect.right()
        );
    }
}
