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
use tuz_layout::{ChromeButton, Rect};

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
    /// Draw the close button. Only the hovered tab gets one: a permanent × on every
    /// tab is visual noise, and an accidental click costs a running shell.
    pub show_close: bool,
    /// The close button itself is hovered, so highlight it.
    pub close_hovered: bool,
}

/// Draw the tab bar.
///
/// Returns nothing: the caller already knows the rect, and the instance range it
/// appended is whatever the buffer grew by.
#[allow(clippy::too_many_arguments)]
pub fn draw_tab_bar(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    bar: Rect,
    tabs: &[Rect],
    closes: &[Rect],
    labels: &[TabLabel<'_>],
    theme: &Theme,
    colors: ColorSpace,
    radius: f32,
) {
    if bar.height == 0 || tabs.is_empty() {
        return;
    }

    // The strip itself, so the area past the last tab is filled rather than showing
    // the window clear color.
    //
    // Only its top corners are rounded: it is the topmost thing in the window, so it
    // owns the window's top curve, while its bottom edge must stay square to meet the
    // panes below without a seam.
    out.push(Instance::rounded(
        bar.x as f32,
        bar.y as f32,
        bar.width as f32,
        bar.height as f32,
        colors.convert(theme.split_divider()),
        radius,
        crate::instance::FLAG_ROUND_TOP,
    ));

    // A tab against the strip's left edge sits in the window's top-left corner, and
    // as a plain rectangle it would paint the curve square again. Only that one
    // corner is rounded — the other three meet neighbours, not the outside.
    let corner_of = |rect: &Rect| {
        if rect.x <= bar.x {
            crate::instance::FLAG_ROUND_TL
        } else {
            0
        }
    };

    for (rect, label) in tabs.iter().zip(labels) {
        // The active tab takes the pane background so it reads as continuous with
        // the terminal below; inactive tabs stay on the strip color.
        let background = if label.active {
            theme.background_focused()
        } else {
            theme.split_divider()
        };
        out.push(Instance::rounded(
            rect.x as f32,
            rect.y as f32,
            rect.width as f32,
            rect.height as f32,
            colors.convert(background),
            radius,
            corner_of(rect),
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
        // background stays unbroken, and skipped at the strip's left edge where there
        // is no tab to separate from. Drawing it there put a hard vertical stroke
        // through the window's rounded top-left corner, which showed up as a stray
        // mark sitting outside the curve.
        if !label.active && rect.x > bar.x {
            out.push(Instance::solid(
                rect.x as f32,
                rect.y as f32 + PADDING / 2.0,
                1.0,
                (rect.height as f32 - PADDING).max(1.0),
                colors.convert(theme.normal.black),
            ));
        }

        // Unseen output brightens the tab's own title rather than adding a mark
        // beside it. A colored dot is a second thing to look at, in a strip whose job
        // is to stay quiet, and it shifted the title sideways as it came and went.
        // Undimming says the same thing using the text already there: an idle tab
        // recedes, one with output does not, and the active tab is bold regardless.
        let foreground = if label.active || label.has_activity {
            theme.foreground
        } else {
            theme.bright.black
        };

        let mut text_rect = *rect;

        // Room for the close button is reserved whether or not it is showing.
        //
        // Only reserving it while hovered made the title re-truncate and re-center
        // the moment the pointer arrived, so every tab's text visibly jumped as the
        // mouse crossed it. The button fades in over space that was already set
        // aside, and the title does not move.
        let index = tabs.iter().position(|t| t == rect).unwrap_or(0);
        if let Some(close) = closes.get(index) {
            let shrink = (rect.right() - close.x).max(0) as u32;
            text_rect.width = text_rect.width.saturating_sub(shrink);

            if label.show_close && label.close_hovered {
                out.push(Instance::solid(
                    close.x as f32,
                    close.y as f32,
                    close.width as f32,
                    close.height as f32,
                    colors.convert_opaque(theme.normal.red),
                ));
            }
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

        if label.show_close {
            if let Some(close) = closes.get(index) {
                crate::icon::draw(
                    out,
                    ChromeButton::Close,
                    *close,
                    colors.convert_opaque(if label.close_hovered {
                        theme.background
                    } else {
                        theme.bright.black
                    }),
                    colors.convert_opaque(if label.close_hovered {
                        theme.normal.red
                    } else {
                        background
                    }),
                );
            }
        }
    }
}

/// Draw the action buttons packed at the right of the tab strip.
#[allow(clippy::too_many_arguments)]
pub fn draw_chrome_buttons(
    out: &mut Vec<Instance>,
    bar: Rect,
    buttons: &[(ChromeButton, Rect)],
    hovered: Option<ChromeButton>,
    // `pressed` is the button held down right now; `active` are the ones whose panel
    // is currently showing, drawn lit so the toolbar says what is open rather than
    // only what is under the pointer.
    pressed: Option<ChromeButton>,
    active: &[ChromeButton],
    theme: &Theme,
    colors: ColorSpace,
    radius: f32,
) {
    for (button, rect) in buttons {
        let is_hovered = hovered == Some(*button);
        let is_pressed = pressed == Some(*button);
        let is_active = active.contains(button);
        // Close gets a red highlight, because it is the one that loses work.
        let accent = if *button == ChromeButton::Close {
            theme.normal.red
        } else {
            theme.cursor()
        };

        // Three states, most specific first. Pressed inverts — the strongest signal,
        // and the one that makes a click feel like it landed rather than like the
        // pointer merely passed over. Active is a quieter lit state that persists
        // while the panel is open.
        let (fill, tint) = if is_pressed {
            (Some(accent), theme.background)
        } else if is_hovered {
            (
                Some(if *button == ChromeButton::Close {
                    theme.normal.red
                } else {
                    theme.background_focused()
                }),
                theme.foreground,
            )
        } else if is_active {
            (Some(theme.background_focused()), accent)
        } else {
            // `bright.black` is the strip's own dim grey — near-invisible against it,
            // which is what an idle toolbar looked like. Icons are the only thing
            // saying the buttons exist, so they read at close to full strength and
            // let hover and press supply the contrast instead.
            (None, theme.foreground)
        };

        let background = fill.unwrap_or_else(|| theme.split_divider());
        if let Some(fill) = fill {
            out.push(Instance::rounded(
                rect.x as f32,
                rect.y as f32,
                rect.width as f32,
                rect.height as f32,
                colors.convert_opaque(fill),
                radius,
                if rect.right() >= bar.right() {
                    crate::instance::FLAG_ROUND_TR
                } else {
                    0
                },
            ));
        }

        // A rule under an active button, the way the active tab is marked. It survives
        // hover, so moving the pointer away does not make the toolbar forget which
        // panel is open.
        if is_active && !is_pressed {
            out.push(Instance::solid(
                rect.x as f32,
                (rect.bottom() - ACTIVE_MARKER as i32) as f32,
                rect.width as f32,
                ACTIVE_MARKER as f32,
                colors.convert_opaque(accent),
            ));
        }

        crate::icon::draw(
            out,
            *button,
            *rect,
            colors.convert_opaque(tint),
            colors.convert_opaque(background),
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
#[allow(clippy::too_many_arguments)]
pub fn draw_status_bar(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    bar: Rect,
    left: &[StatusItem<'_>],
    items: &[StatusItem<'_>],
    theme: &Theme,
    colors: ColorSpace,
    radius: f32,
) -> Vec<Rect> {
    // Rects of the right-hand segments, in the order given, so the caller can
    // hit-test what it drew rather than recomputing the layout and hoping the two
    // agree.
    let mut placed = Vec::with_capacity(items.len());

    if bar.height == 0 {
        return placed;
    }

    // The strip is the bottom-most thing in the window, so it owns the window's
    // bottom curve. Its top edge stays square to meet the panes above it without a
    // seam — the same split the tab bar makes at the other end.
    out.push(Instance::rounded(
        bar.x as f32,
        bar.y as f32,
        bar.width as f32,
        bar.height as f32,
        colors.convert(theme.split_divider()),
        radius,
        crate::instance::FLAG_ROUND_BOTTOM,
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

        placed.push(segment);
        right = left;
    }

    // The left block goes second, into whatever the right block left. That order is
    // deliberate: plugin content on the right is volatile — a clock changing width
    // every minute — and laying it out first means it can never shove the built-in
    // segments sideways. It also gives the left block an exact budget, so it stops
    // rather than drawing underneath.
    let mut pen = bar.x as f32 + PADDING;
    for item in left {
        let width = text::measure(item.text, cell_width) + PADDING * 2.0;
        // `right` is where the right block ended; running past it would overlap.
        if pen + width > right {
            break;
        }

        let segment = Rect::new(pen as i32, bar.y, width as u32, bar.height);

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

        pen += width;
    }

    placed
}

/// Draw a tooltip below a hovered chrome button.
///
/// The strip buttons are bare glyphs. `+` and `×` are self-evident; the split
/// buttons are not, and a control nobody can identify may as well not be there.
///
/// Drawn after the strip and clamped to the window, so a button near the right edge
/// gets a tooltip that shifts left rather than one that runs off-screen.
pub fn draw_tooltip(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    button: ChromeButton,
    anchor: Rect,
    window: Rect,
    theme: &Theme,
    colors: ColorSpace,
) {
    let text = button.describe();
    let metrics = fonts.metrics();
    let width = text::measure(text, metrics.width as f32) as u32 + (PADDING * 2.0) as u32;
    let height = metrics.height + 6;

    // Centred under the button, then pulled inside the window.
    let mut x = anchor.center_x() - (width / 2) as i32;
    x = x.clamp(window.x, (window.right() - width as i32).max(window.x));
    let y = anchor.bottom() + 2;

    // A tooltip that would fall outside the window vertically is simply not drawn:
    // better absent than clipped to a sliver.
    if y + height as i32 > window.bottom() {
        return;
    }

    let rect = Rect::new(x, y, width, height);
    out.push(Instance::solid(
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
        colors.convert_opaque(theme.split_divider()),
    ));
    let inner = rect.inset(1, 1);
    out.push(Instance::solid(
        inner.x as f32,
        inner.y as f32,
        inner.width as f32,
        inner.height as f32,
        colors.convert_opaque(theme.background_focused()),
    ));

    text::draw_in_box(
        out,
        fonts,
        text,
        rect,
        PADDING,
        Align::Center,
        theme.foreground,
        colors,
        Style::Regular,
    );
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
                show_close: false,
                close_hovered: false,
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
            &[],
            &labels(0, 1),
            &Theme::builtin_default(),
            colors(),
            0.0,
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
            &[],
            &Theme::builtin_default(),
            colors(),
            0.0,
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
            &[],
            &labels(0, 2),
            &Theme::builtin_default(),
            colors(),
            0.0,
        );

        let first = out.first().expect("something should be drawn");
        assert_eq!(
            first.flags & crate::FLAG_TEXTURED,
            0,
            "the strip background is a solid, not a glyph"
        );
        assert_eq!(first.size, [600.0, 24.0]);
    }

    #[test]
    fn the_first_tab_draws_nothing_into_the_rounded_corner() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut out = Vec::new();
        let bar = bar();
        let tabs = tuz_layout::tab_rects(bar, 2, 180, 60);

        // Tab 1 active, so tab 0 — the one in the corner — takes the inactive path
        // that used to draw a separator down its left edge.
        draw_tab_bar(
            &mut out,
            &mut fonts,
            bar,
            &tabs,
            &[],
            &labels(1, 2),
            &theme,
            colors(),
            10.0,
        );

        // Anything one pixel wide starting exactly at the strip's left edge is the
        // separator, and it would cut across the corner the strip just rounded.
        assert!(
            !out.iter().any(|i| i.position[0] == bar.x as f32 && i.size[0] == 1.0),
            "nothing should be drawn along the strip's left edge"
        );
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
            &[],
            &labels(0, 2),
            &theme,
            colors(),
            0.0,
        );

        // The active tab's background is the focused pane background, so the strip
        // reads as continuous with the terminal below it.
        let active_bg = colors().convert(theme.background_focused());
        assert!(
            out.iter()
                .any(|i| i.flags & crate::FLAG_TEXTURED == 0 && i.color == active_bg),
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
            &[],
            &labels(2, 4),
            &theme,
            colors(),
            0.0,
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
            &[],
            &labels(0, 2),
            &Theme::builtin_default(),
            colors(),
            0.0,
        );

        let glyphs = out
            .iter()
            .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
            .count();
        // "shell" twice, five visible glyphs each.
        assert!(glyphs >= 10, "expected tab titles, got {glyphs} glyphs");
    }

    #[test]
    fn unseen_output_undims_the_tab_rather_than_marking_it() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let tabs = tuz_layout::tab_rects(bar(), 2, 180, 60);

        let draw = |fonts: &mut FontSystem, activity: bool| {
            let labels = vec![
                TabLabel {
                    title: "shell",
                    active: true,
                    has_activity: false,
                    show_close: false,
                    close_hovered: false,
                },
                TabLabel {
                    title: "shell",
                    active: false,
                    has_activity: activity,
                    show_close: false,
                    close_hovered: false,
                },
            ];
            let mut out = Vec::new();
            draw_tab_bar(
                &mut out,
                fonts,
                bar(),
                &tabs,
                &[],
                &labels,
                &theme,
                colors(),
                0.0,
            );
            out
        };

        let idle = draw(&mut fonts, false);
        let busy = draw(&mut fonts, true);

        let glyph_left = |out: &[Instance]| {
            out.iter()
                .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
                .map(|i| i.position[0])
                .fold(f32::MAX, f32::min)
        };
        // The title must not move: a mark that appears and disappears beside it made
        // the text shuffle sideways every time a background tab printed something.
        assert_eq!(
            glyph_left(&idle),
            glyph_left(&busy),
            "activity should not shift the title"
        );

        let dim = colors().convert_opaque(theme.bright.black);
        let lit = colors().convert_opaque(theme.foreground);
        let count = |out: &[Instance], color: [f32; 4]| {
            out.iter()
                .filter(|i| i.flags & crate::FLAG_TEXTURED != 0 && i.color == color)
                .count()
        };

        // Idle: the inactive tab recedes. With output: it does not.
        assert!(count(&idle, dim) > 0, "an idle background tab should be dimmed");
        assert!(
            count(&busy, dim) < count(&idle, dim),
            "unseen output should undim the tab"
        );
        assert!(count(&busy, lit) > count(&idle, lit));
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
            show_close: false,
            close_hovered: false,
        }];

        let mut out = Vec::new();
        draw_tab_bar(
            &mut out,
            &mut fonts,
            bar(),
            &tabs[..1],
            &[],
            &long,
            &Theme::builtin_default(),
            colors(),
            0.0,
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
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 100, 600, 20),
            &[],
            &[],
            &Theme::builtin_default(),
            colors(),
            0.0,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size, [600.0, 20.0]);
    }

    #[test]
    fn a_hidden_status_bar_draws_nothing() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 100, 600, 0),
            &[],
            &[StatusItem {
                text: "x",
                foreground: None,
                background: None,
            }],
            &Theme::builtin_default(),
            colors(),
            0.0,
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
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            bar,
            &[],
            &items,
            &Theme::builtin_default(),
            colors(),
            0.0,
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
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 100, 600, 20),
            &[],
            &items,
            &Theme::builtin_default(),
            colors(),
            0.0,
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
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 100, 600, 20),
            &[],
            &items,
            &theme,
            colors(),
            0.0,
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
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            narrow,
            &[],
            &items,
            &Theme::builtin_default(),
            colors(),
            0.0,
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
    #[test]
    fn built_in_segments_start_at_the_left_edge() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        let bar = Rect::new(0, 100, 600, 24);
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            bar,
            &[StatusItem {
                text: "~/src",
                foreground: None,
                background: Some("#00ff00"),
            }],
            &[],
            &Theme::builtin_default(),
            colors(),
            0.0,
        );

        // The left block anchors to `bar.x`, unlike the plugin block which hugs the
        // right edge. Its background quad is the one thing whose position is exact.
        let green = colors().convert(Rgba::parse("#00ff00").unwrap());
        let seg = out
            .iter()
            .find(|i| i.color == green)
            .expect("the left segment should be drawn");
        assert!(
            seg.position[0] < bar.width as f32 / 2.0,
            "a left segment landed at x={}, which is not the left half",
            seg.position[0]
        );
    }

    #[test]
    fn the_two_blocks_never_overlap() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        let bar = Rect::new(0, 0, 600, 24);
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            bar,
            &[StatusItem {
                text: "~/some/long/path/here",
                foreground: None,
                background: Some("#00ff00"),
            }],
            &[StatusItem {
                text: "main",
                foreground: None,
                background: Some("#ff0000"),
            }],
            &Theme::builtin_default(),
            colors(),
            0.0,
        );

        let find = |hex: &str| {
            let want = colors().convert(Rgba::parse(hex).unwrap());
            out.iter()
                .find(|i| i.color == want)
                .map(|i| (i.position[0], i.position[0] + i.size[0]))
                .expect("both segments should be drawn")
        };
        let (left_x, left_end) = find("#00ff00");
        let (right_x, _) = find("#ff0000");

        assert!(left_x < right_x);
        assert!(
            left_end <= right_x,
            "the left block ends at {left_end} but the right one starts at {right_x}"
        );
    }

    #[test]
    fn a_left_block_with_no_room_is_dropped_rather_than_drawn_under() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        // A plugin segment wide enough to leave the built-ins nothing.
        let _ = draw_status_bar(
            &mut out,
            &mut fonts,
            Rect::new(0, 0, 120, 24),
            &[StatusItem {
                text: "~/a/very/long/directory/name",
                foreground: None,
                background: Some("#00ff00"),
            }],
            &[StatusItem {
                text: "a plugin segment that fills the bar",
                foreground: None,
                background: None,
            }],
            &Theme::builtin_default(),
            colors(),
            0.0,
        );

        let green = colors().convert(Rgba::parse("#00ff00").unwrap());
        assert!(
            !out.iter().any(|i| i.color == green),
            "with no room left the built-in segment must be skipped, not overlapped"
        );
    }

    /// Draw one button in a given state and return what was emitted.
    fn button_in(
        hovered: Option<ChromeButton>,
        pressed: Option<ChromeButton>,
        active: &[ChromeButton],
    ) -> Vec<Instance> {
        let mut out = Vec::new();
        let bar = Rect::new(0, 0, 600, 40);
        draw_chrome_buttons(
            &mut out,
            bar,
            &[(ChromeButton::Explorer, Rect::new(0, 0, 40, 40))],
            hovered,
            pressed,
            active,
            &Theme::builtin_default(),
            colors(),
            0.0,
        );
        out
    }

    #[test]
    fn an_idle_button_paints_no_background() {
        let out = button_in(None, None, &[]);
        // Only the icon strokes. A button that fills when nothing is happening reads
        // as permanently hovered.
        assert!(out.iter().all(|i| i.size[0] < 40.0 || i.size[1] < 40.0));
    }

    #[test]
    fn hover_press_and_active_are_three_different_looks() {
        let theme = Theme::builtin_default();
        let fill_of = |out: &[Instance]| {
            out.iter()
                .find(|i| i.size[0] == 40.0 && i.size[1] == 40.0)
                .map(|i| i.color)
        };

        let hover = fill_of(&button_in(Some(ChromeButton::Explorer), None, &[]));
        let press = fill_of(&button_in(
            Some(ChromeButton::Explorer),
            Some(ChromeButton::Explorer),
            &[],
        ));
        let active = fill_of(&button_in(None, None, &[ChromeButton::Explorer]));

        assert!(hover.is_some() && press.is_some() && active.is_some());
        // Pressed has to differ from hover, or a click gives no feedback at all: the
        // pointer is already over the button when you press it.
        assert_ne!(hover, press, "a press must look different from a hover");
        assert_eq!(
            press,
            Some(colors().convert_opaque(theme.cursor())),
            "pressed inverts to the accent color"
        );
    }

    #[test]
    fn an_open_panel_keeps_its_button_marked_after_the_pointer_leaves() {
        let out = button_in(None, None, &[ChromeButton::Explorer]);
        let accent = colors().convert_opaque(Theme::builtin_default().cursor());
        // The rule under the button, like the one under the active tab. Hover alone
        // would forget which panel is open the moment the mouse moved away.
        assert!(
            out.iter()
                .any(|i| i.color == accent && i.size[1] == ACTIVE_MARKER as f32),
            "an active button should keep a marker"
        );
    }

    #[test]
    fn close_gets_a_red_press_rather_than_the_usual_accent() {
        let mut out = Vec::new();
        draw_chrome_buttons(
            &mut out,
            Rect::new(0, 0, 600, 40),
            &[(ChromeButton::Close, Rect::new(0, 0, 40, 40))],
            Some(ChromeButton::Close),
            Some(ChromeButton::Close),
            &[],
            &Theme::builtin_default(),
            colors(),
            0.0,
        );
        let red = colors().convert_opaque(Theme::builtin_default().normal.red);
        assert!(
            out.iter().any(|i| i.color == red),
            "the button that loses work should say so"
        );
    }

}
