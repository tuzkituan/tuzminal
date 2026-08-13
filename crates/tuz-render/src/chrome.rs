//! The tab bar and status bar.
//!
//! Both are drawn with the same instanced quads as terminal content, so they cost
//! no extra pipeline and no extra draw call — they are simply more rects appended to
//! the same buffer.
//!
//! # Colors come from the theme, not from constants
//!
//! Chrome that ignores the theme is the fastest way to make a terminal look wrong
//! after a theme switch. Every color here is derived from the active theme: the
//! active tab uses the pane background so it reads as continuous with the terminal
//! below it, and inactive tabs sit on the divider color so the strip recedes.

use crate::instance::{ColorSpace, Instance};
use crate::text::{self, Align};
use tuz_config::{Rgba, Theme};
use tuz_font::{FontSystem, Style};
use tuz_layout::Rect;

/// Inset for text inside a tab or status segment.
const PADDING: f32 = 8.0;

/// Thickness of the bar marking the active tab.
const ACTIVE_MARKER: u32 = 2;

/// One tab to draw.
pub struct TabLabel<'a> {
    pub title: &'a str,
    pub active: bool,
    /// Shown as a dot before the title when the tab has unseen output.
    pub has_activity: bool,
}

/// Draw the tab bar.
///
/// Returns nothing: the caller already knows the rect, and the instance range it
/// appended is whatever the buffer grew by.
pub fn draw_tab_bar(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    bar: Rect,
    tabs: &[Rect],
    labels: &[TabLabel<'_>],
    theme: &Theme,
    colors: ColorSpace,
) {
    if bar.height == 0 || tabs.is_empty() {
        return;
    }

    // The strip itself, so the area past the last tab is filled rather than showing
    // the window clear color.
    out.push(Instance::solid(
        bar.x as f32,
        bar.y as f32,
        bar.width as f32,
        bar.height as f32,
        colors.convert(theme.split_divider()),
    ));

    for (rect, label) in tabs.iter().zip(labels) {
        // The active tab takes the pane background so it reads as continuous with
        // the terminal below; inactive tabs stay on the strip color.
        let background = if label.active {
            theme.background_focused()
        } else {
            theme.split_divider()
        };
        out.push(Instance::solid(
            rect.x as f32,
            rect.y as f32,
            rect.width as f32,
            rect.height as f32,
            colors.convert(background),
        ));

        // A colored bar along the bottom of the active tab. Bottom rather than top
        // so it sits against the panes it belongs to.
        if label.active {
            out.push(Instance::solid(
                rect.x as f32,
                (rect.bottom() - ACTIVE_MARKER as i32) as f32,
                rect.width as f32,
                ACTIVE_MARKER as f32,
                colors.convert_opaque(theme.cursor()),
            ));
        }

        // A separator between inactive tabs, skipped for the active one so its
        // background stays unbroken.
        if !label.active {
            out.push(Instance::solid(
                rect.x as f32,
                rect.y as f32 + PADDING / 2.0,
                1.0,
                (rect.height as f32 - PADDING).max(1.0),
                colors.convert(theme.normal.black),
            ));
        }

        let foreground = if label.active {
            theme.foreground
        } else {
            // Dimmed rather than a different hue, so the active tab is obvious
            // without the strip becoming noisy.
            theme.bright.black
        };

        // An activity dot occupies the left inset, so the title shifts right to make
        // room rather than being drawn over.
        let mut text_rect = *rect;
        if label.has_activity {
            let dot = (fonts.metrics().width as f32 / 3.0).max(2.0);
            out.push(Instance::solid(
                rect.x as f32 + PADDING / 2.0,
                rect.y as f32 + (rect.height as f32 - dot) / 2.0,
                dot,
                dot,
                colors.convert_opaque(theme.normal.yellow),
            ));
            let shift = (PADDING / 2.0 + dot) as u32;
            text_rect.x += shift as i32;
            text_rect.width = text_rect.width.saturating_sub(shift);
        }

        text::draw_in_box(
            out,
            fonts,
            label.title,
            text_rect,
            PADDING,
            Align::Center,
            foreground,
            colors,
            if label.active {
                Style::Bold
            } else {
                Style::Regular
            },
        );
    }
}

/// One status bar segment.
pub struct StatusItem<'a> {
    pub text: &'a str,
    /// `#rrggbb` overrides from a plugin; invalid values fall back to the theme.
    pub foreground: Option<&'a str>,
    pub background: Option<&'a str>,
}

/// Draw the status bar, laying segments out from the right.
///
/// Right-aligned because status content is usually short and volatile (a clock, a
/// branch name); anchoring it to the right edge stops the whole row jittering when
/// one segment changes width.
pub fn draw_status_bar(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    bar: Rect,
    items: &[StatusItem<'_>],
    theme: &Theme,
    colors: ColorSpace,
) {
    if bar.height == 0 {
        return;
    }

    out.push(Instance::solid(
        bar.x as f32,
        bar.y as f32,
        bar.width as f32,
        bar.height as f32,
        colors.convert(theme.split_divider()),
    ));

    let cell_width = fonts.metrics().width as f32;
    let mut right = bar.right() as f32 - PADDING;

    for item in items {
        let width = text::measure(item.text, cell_width) + PADDING * 2.0;
        let left = right - width;
        // Stop once segments would run off the left edge, rather than drawing them
        // on top of each other.
        if left < bar.x as f32 {
            break;
        }

        let segment = Rect::new(left as i32, bar.y, width as u32, bar.height);

        if let Some(background) = item.background.and_then(parse_color) {
            out.push(Instance::solid(
                segment.x as f32,
                segment.y as f32,
                segment.width as f32,
                segment.height as f32,
                colors.convert(background),
            ));
        }

        let foreground = item
            .foreground
            .and_then(parse_color)
            .unwrap_or(theme.foreground);

        text::draw_in_box(
            out,
            fonts,
            item.text,
            segment,
            PADDING,
            Align::Center,
            foreground,
            colors,
            Style::Regular,
        );

        right = left;
    }
}

/// Parse a plugin-supplied color, ignoring anything malformed.
///
/// A plugin sending a bad color should lose that color, not crash the frame or
/// paint something arbitrary.
fn parse_color(text: &str) -> Option<Rgba> {
    Rgba::parse(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn bar() -> Rect {
        Rect::new(0, 0, 600, 24)
    }

    fn labels(active: usize, count: usize) -> Vec<TabLabel<'static>> {
        (0..count)
            .map(|i| TabLabel {
                title: "shell",
                active: i == active,
                has_activity: false,
            })
            .collect()
    }

    #[test]
    fn a_hidden_bar_draws_nothing() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        draw_tab_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 0, 600, 0),
            &[Rect::new(0, 0, 180, 0)],
            &labels(0, 1),
            &Theme::builtin_default(),
            colors(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn no_tabs_draws_nothing() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        draw_tab_bar(
            &mut out,
            &mut fonts,
            bar(),
            &[],
            &[],
            &Theme::builtin_default(),
            colors(),
        );
        assert!(
            out.is_empty(),
            "an empty strip should not paint a background"
        );
    }

    #[test]
    fn the_strip_background_covers_the_whole_bar() {
        // Otherwise the area past the last tab shows the window clear color.
        let mut fonts = fonts();
        let mut out = Vec::new();
        let tabs = tuz_layout::tab_rects(bar(), 2, 180, 60);
        draw_tab_bar(
            &mut out,
            &mut fonts,
            bar(),
            &tabs,
            &labels(0, 2),
            &Theme::builtin_default(),
            colors(),
        );

        let first = out.first().expect("something should be drawn");
        assert_eq!(first.flags, 0, "the strip background is a solid");
        assert_eq!(first.size, [600.0, 24.0]);
    }

    #[test]
    fn the_active_tab_is_visually_distinguished() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let tabs = tuz_layout::tab_rects(bar(), 2, 180, 60);

        let mut out = Vec::new();
        draw_tab_bar(
            &mut out,
            &mut fonts,
            bar(),
            &tabs,
            &labels(0, 2),
            &theme,
            colors(),
        );

        // The active tab's background is the focused pane background, so the strip
        // reads as continuous with the terminal below it.
        let active_bg = colors().convert(theme.background_focused());
        assert!(
            out.iter().any(|i| i.flags == 0 && i.color == active_bg),
            "the active tab should use the focused pane background"
        );

        // And it carries the marker bar in the cursor color.
        let marker = colors().convert_opaque(theme.cursor());
        assert!(
            out.iter()
                .any(|i| i.flags == 0 && i.color == marker && i.size[1] == ACTIVE_MARKER as f32),
            "the active tab should have a marker bar"
        );
    }

    #[test]
    fn only_one_tab_gets_the_active_marker() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let tabs = tuz_layout::tab_rects(bar(), 4, 180, 60);

        let mut out = Vec::new();
        draw_tab_bar(
            &mut out,
            &mut fonts,
            bar(),
            &tabs,
            &labels(2, 4),
            &theme,
            colors(),
        );

        let marker = colors().convert_opaque(theme.cursor());
        let markers = out
            .iter()
            .filter(|i| i.flags == 0 && i.color == marker && i.size[1] == ACTIVE_MARKER as f32)
            .count();
        assert_eq!(markers, 1, "exactly one tab is active");
    }

    #[test]
    fn tab_titles_are_drawn_as_glyphs() {
        let mut fonts = fonts();
        let tabs = tuz_layout::tab_rects(bar(), 2, 180, 60);

        let mut out = Vec::new();
        draw_tab_bar(
            &mut out,
            &mut fonts,
            bar(),
            &tabs,
            &labels(0, 2),
            &Theme::builtin_default(),
            colors(),
        );

        let glyphs = out
            .iter()
            .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
            .count();
        // "shell" twice, five visible glyphs each.
        assert!(glyphs >= 10, "expected tab titles, got {glyphs} glyphs");
    }

    #[test]
    fn an_activity_dot_shifts_the_title_rather_than_overlapping_it() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let tabs = tuz_layout::tab_rects(bar(), 1, 180, 60);

        let without = {
            let mut out = Vec::new();
            draw_tab_bar(
                &mut out,
                &mut fonts,
                bar(),
                &tabs,
                &labels(0, 1),
                &theme,
                colors(),
            );
            out.iter()
                .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
                .map(|i| i.position[0])
                .fold(f32::MAX, f32::min)
        };

        let with = {
            let mut out = Vec::new();
            let marked = vec![TabLabel {
                title: "shell",
                active: true,
                has_activity: true,
            }];
            draw_tab_bar(
                &mut out,
                &mut fonts,
                bar(),
                &tabs,
                &marked,
                &theme,
                colors(),
            );
            out.iter()
                .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
                .map(|i| i.position[0])
                .fold(f32::MAX, f32::min)
        };

        assert!(
            with > without,
            "the title should move right to make room for the dot ({with} vs {without})"
        );
    }

    #[test]
    fn a_long_title_stays_inside_its_tab() {
        let mut fonts = fonts();
        // Narrow tabs, so truncation is forced.
        let tabs = tuz_layout::tab_rects(bar(), 8, 180, 60);

        let long = vec![TabLabel {
            title: "an extremely long tab title that cannot possibly fit",
            active: true,
            has_activity: false,
        }];

        let mut out = Vec::new();
        draw_tab_bar(
            &mut out,
            &mut fonts,
            bar(),
            &tabs[..1],
            &long,
            &Theme::builtin_default(),
            colors(),
        );

        let right = out
            .iter()
            .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
            .map(|i| i.position[0] + i.size[0])
            .fold(f32::MIN, f32::max);
        assert!(
            right <= tabs[0].right() as f32 + 1.0,
            "title ran to {right}, past the tab edge {}",
            tabs[0].right()
        );
    }

    // --- status bar -------------------------------------------------------

    #[test]
    fn an_empty_status_bar_still_paints_its_background() {
        // The strip is reserved space; leaving it unpainted shows the clear color.
        let mut fonts = fonts();
        let mut out = Vec::new();
        draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 100, 600, 20),
            &[],
            &Theme::builtin_default(),
            colors(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size, [600.0, 20.0]);
    }

    #[test]
    fn a_hidden_status_bar_draws_nothing() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 100, 600, 0),
            &[StatusItem {
                text: "x",
                foreground: None,
                background: None,
            }],
            &Theme::builtin_default(),
            colors(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn segments_are_laid_out_from_the_right() {
        // Right-anchored so a segment changing width does not shift the others.
        let mut fonts = fonts();
        let bar = Rect::new(0, 100, 600, 20);
        let items = vec![
            StatusItem {
                text: "first",
                foreground: None,
                background: None,
            },
            StatusItem {
                text: "second",
                foreground: None,
                background: None,
            },
        ];

        let mut out = Vec::new();
        draw_status_bar(
            &mut out,
            &mut fonts,
            bar,
            &items,
            &Theme::builtin_default(),
            colors(),
        );

        let glyph_xs: Vec<f32> = out
            .iter()
            .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
            .map(|i| i.position[0])
            .collect();
        assert!(!glyph_xs.is_empty());
        let rightmost = glyph_xs.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            rightmost > bar.width as f32 / 2.0,
            "segments should hug the right edge, rightmost glyph at {rightmost}"
        );
    }

    #[test]
    fn a_plugin_background_color_is_honored() {
        let mut fonts = fonts();
        let items = vec![StatusItem {
            text: "hot",
            foreground: Some("#ff0000"),
            background: Some("#00ff00"),
        }];

        let mut out = Vec::new();
        draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 100, 600, 20),
            &items,
            &Theme::builtin_default(),
            colors(),
        );

        let green = colors().convert(Rgba::rgb(0, 255, 0));
        assert!(
            out.iter().any(|i| i.flags == 0 && i.color == green),
            "the segment background should use the plugin's color"
        );
    }

    #[test]
    fn a_malformed_plugin_color_falls_back_instead_of_failing() {
        // A plugin sending nonsense should lose the color, not the frame.
        assert_eq!(parse_color("not a color"), None);
        assert_eq!(parse_color("#ff0000"), Some(Rgba::rgb(255, 0, 0)));

        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let items = vec![StatusItem {
            text: "x",
            foreground: Some("garbage"),
            background: Some("garbage"),
        }];

        let mut out = Vec::new();
        draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 100, 600, 20),
            &items,
            &theme,
            colors(),
        );

        // Text still drawn, in the theme foreground.
        let expected = colors().convert_opaque(theme.foreground);
        assert!(out
            .iter()
            .any(|i| i.flags & crate::FLAG_TEXTURED != 0 && i.color == expected));
    }

    #[test]
    fn segments_that_would_overflow_the_bar_are_dropped() {
        // Drawing them anyway would stack them on top of each other.
        let mut fonts = fonts();
        let narrow = Rect::new(0, 0, 80, 20);
        let items: Vec<StatusItem> = (0..10)
            .map(|_| StatusItem {
                text: "segment",
                foreground: None,
                background: None,
            })
            .collect();

        let mut out = Vec::new();
        draw_status_bar(
            &mut out,
            &mut fonts,
            narrow,
            &items,
            &Theme::builtin_default(),
            colors(),
        );

        let leftmost = out
            .iter()
            .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
            .map(|i| i.position[0])
            .fold(f32::MAX, f32::min);
        assert!(
            leftmost >= narrow.x as f32 || out.len() == 1,
            "a segment was drawn off the left edge at {leftmost}"
        );
    }
}
