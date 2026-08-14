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

use crate::instance::{ColorSpace, Instance, FLAG_ROUND_ALL, RADIUS_CONTROL, RADIUS_PILL};
use crate::text::{self, Align};
use tuz_config::{Rgba, Theme};
use tuz_font::{FontSystem, Style};
use tuz_layout::{ChromeButton, Rect};

/// Inset for text inside a tab or status segment.
///
/// Public because callers that decide what fits — the status bar builder in
/// particular — must measure with the same padding this draws with. A private copy
/// there drifted from this one in behaviour, not just in value.
pub const PADDING: f32 = 8.0;

/// Thickness of the bar marking the active tab.
const ACTIVE_MARKER: u32 = 2;

/// How far a coloured status segment is pulled in from the top and bottom of the bar,
/// so it reads as a badge on the bar rather than a break in it.
const SEGMENT_INSET: u32 = 3;

/// Thickness of the rule separating the window controls from the app buttons.
const DIVIDER_WIDTH: f32 = 1.0;

/// Space left beside and above the active tab's shape.
///
/// Taken out of the tab rect when drawing rather than in `tab_rects`, so layout and
/// hit-testing keep using whole, adjacent rects. The pointer therefore hits a slightly
/// larger area than the shape it can see — the forgiving direction, and the same trade
/// the menu's row highlight makes.
///
/// There is deliberately no bottom inset: see [`tab_pill`].
const TAB_GAP: u32 = 3;
const TAB_INSET: u32 = 4;

/// How far a toolbar button's active indicator sits above the bottom of its slot.
const MARKER_LIFT: u32 = 3;

/// How far a toolbar button's hover fill is inset from its slot, so the fill reads as a
/// key under the pointer rather than a block cut out of the strip.
const BUTTON_INSET: u32 = 3;

/// The shape drawn behind the active tab.
///
/// Inset at the sides and the top, but **not** at the bottom: it runs to the bottom of
/// the strip so its fill meets the pane background below without a seam, which is the
/// whole reason the active tab is painted in that colour. Only its top corners are
/// rounded, for the same reason — a curve at the bottom would cut a notch out of the
/// join.
///
/// Public so hover and hit-testing can agree with what is drawn if they ever need to;
/// today only the renderer and its tests use it.
pub fn tab_pill(tab: Rect) -> Rect {
    Rect::new(
        tab.x + TAB_GAP as i32,
        tab.y + TAB_INSET as i32,
        tab.width.saturating_sub(TAB_GAP * 2),
        tab.height.saturating_sub(TAB_INSET),
    )
}

/// A tab's close button: a square, centred against the tab's drawn shape.
///
/// Square because it holds an `×`, which is as tall as it is wide, and a slot that is not
/// square puts the glyph off-centre inside its own fill. Centred on `shape` rather than on
/// `slot` because the shape is inset at the top and flush at the bottom, so anything
/// centred in the slot sits high inside the tab — the same correction the title makes.
///
/// Used for both the fill and the icon, so the two cannot drift apart.
fn close_box(slot: Rect, shape: Rect) -> Rect {
    let side = slot.width.min(slot.height);
    Rect::new(
        slot.x + (slot.width.saturating_sub(side) / 2) as i32,
        shape.y + (shape.height.saturating_sub(side) / 2) as i32,
        side,
        side,
    )
}

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

    for (rect, label) in tabs.iter().zip(labels) {
        // Whatever the tab's title and close button sit on: the active tab's fill, or the
        // bare strip for an inactive tab. Not drawn for an inactive tab — see below — but
        // the close button still needs to know what colour it is over.
        //
        // `background`, not `background_focused()`: the active tab's shape runs into the
        // pane below it, so the two have to be the *same* colour for the join to be
        // invisible. `background_focused()` is a shade off on any theme that sets it, and
        // that shade showed up as a seam exactly where the tab meets the terminal.
        let background = if label.active {
            theme.background
        } else {
            theme.split_divider()
        };

        // Only the active tab is filled, and the fill *is* the marker.
        //
        // An inactive tab is just its title sitting on the strip: filling it in the
        // strip's own colour would draw an invisible rectangle, and filling it in
        // anything else would make every tab compete for attention. That also removes the
        // need for a separator between tabs — the gap between two shapes is the
        // separator, which is why the 1px rule that used to be drawn here is gone — and
        // for the bar that used to underline the active title, which was only load-bearing
        // while every tab was a full-height rectangle in nearly the same colour.
        if label.active {
            let pill = tab_pill(*rect);
            out.push(Instance::rounded(
                pill.x as f32,
                pill.y as f32,
                pill.width as f32,
                pill.height as f32,
                colors.convert(background),
                // A rounded rectangle, not a full pill. At the strip's height a true pill
                // curves for most of the tab's short sides, which fights the straight run
                // of text inside it; a control radius reads as a tab that has been
                // softened rather than as a lozenge.
                RADIUS_CONTROL,
                // Top corners only. The shape runs to the bottom of the strip so its fill
                // continues into the pane below it, and a rounded bottom would cut two
                // notches out of that join.
                crate::instance::FLAG_ROUND_TOP,
            ));
        }

        // Unseen output brightens the tab's own title rather than adding a mark
        // beside it. A colored dot is a second thing to look at, in a strip whose job
        // is to stay quiet, and it shifted the title sideways as it came and went.
        // Undimming says the same thing using the text already there: an idle tab
        // recedes, one with output does not, and the active tab is bold regardless.
        // Three readable levels. `bright.black` was the dim one and it is the strip's
        // own grey — an inactive title written in it was very nearly invisible. The
        // active tab is still obvious without help from colour: it is bold, sits on
        // the pane background, and carries the marker bar.
        let foreground = if label.active || label.has_activity {
            theme.foreground
        } else {
            theme.muted_foreground()
        };

        // Centred in the tab's *shape*, not its slot. The shape is inset at the top and
        // flush at the bottom, so its centre sits below the slot's — a title centred in
        // the slot reads as sitting high inside the tab it belongs to. Derived for every
        // tab, active or not, so titles line up with each other as well as with the
        // shape.
        let shape = tab_pill(*rect);
        let mut text_rect = Rect::new(rect.x, shape.y, rect.width, shape.height);

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
                // A rounded square, not a disc: it is the same kind of thing as a toolbar
                // button, so it takes the same radius. A pill here made it the roundest
                // element on the strip, which read as a stray dot rather than a control.
                //
                let box_ = close_box(*close, shape);
                out.push(Instance::rounded(
                    box_.x as f32,
                    box_.y as f32,
                    box_.width as f32,
                    box_.height as f32,
                    colors.convert_opaque(theme.normal.red),
                    RADIUS_CONTROL,
                    FLAG_ROUND_ALL,
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
            // Regular for every tab, including the active one. Bold was a second signal
            // from when the active tab needed all the help it could get; now that it has
            // its own shape in the pane's colour, the weight change only made the title
            // wider than its neighbours' and shifted where it truncated.
            Style::Regular,
        );

        if label.show_close {
            if let Some(close) = closes.get(index) {
                crate::icon::draw(
                    out,
                    ChromeButton::Close,
                    close_box(*close, shape),
                    colors.convert_opaque(if label.close_hovered {
                        theme.background
                    } else {
                        theme.muted_foreground()
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
) {
    // A rule between the app buttons and the window controls, so close does not sit
    // flush against settings. Placed midway through the gap that already exists between
    // the two groups, so it needs no extra space and cannot shift any button.
    //
    // Drawn only when both groups are on the strip: with every app button collapsed into
    // the menu there is nothing to separate, and a rule against the strip's left edge
    // would look like a stray mark.
    if let Some(first_control) = buttons
        .iter()
        .filter(|(b, _)| b.is_window_control())
        .map(|(_, r)| r.x)
        .min()
    {
        let last_app = buttons
            .iter()
            .filter(|(b, r)| !b.is_window_control() && r.right() <= first_control)
            .map(|(_, r)| r.right())
            .max();
        if let Some(last_app) = last_app {
            let x = (last_app + first_control) / 2;
            let inset = (bar.height / 4).max(1);
            out.push(Instance::solid(
                x as f32,
                (bar.y + inset as i32) as f32,
                DIVIDER_WIDTH,
                bar.height.saturating_sub(inset * 2) as f32,
                colors.convert_opaque(theme.chrome_divider()),
            ));
        }
    }

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

        // Three states, most specific first. Pressed is the strongest, and the one that
        // makes a click feel like it landed rather than like the pointer merely passed
        // over. Active is a quieter lit state that persists while the panel is open.
        //
        // Pressed goes a step further than hover on the same neutral rather than flooding
        // the button with the accent: the accent is what marks *state* on this strip — the
        // active indicator, a focused control — and spending it on a transient press made
        // every click read as "this is now selected". Close keeps its red, which is a
        // warning rather than an accent.
        let (fill, tint) = if is_pressed {
            if *button == ChromeButton::Close {
                (Some(theme.normal.red), theme.background)
            } else {
                (Some(theme.chrome_divider()), theme.foreground)
            }
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
            // Rounded on all four corners and inset from the strip, so hover and press
            // read as a key under the pointer rather than as a block of colour cut out
            // of the toolbar. This used to pass a radius with no corner flags for every
            // button except the one in the window's corner, so the radius did nothing.
            //
            // The button in the corner still keeps the window's own larger radius on that
            // one corner: it paints the pixels the window curve occupies, and a smaller
            // radius there would square the curve off.
            // Every button, including the one in the window's corner, gets the same inset
            // and the same radius. The corner used to be special-cased to the window's own
            // radius so its fill would not square off the curve — but an inset fill never
            // reaches the corner, and the strip beneath it already carries the curve. The
            // exception only made close visibly larger than its neighbours.
            let box_ = rect.inset(BUTTON_INSET, BUTTON_INSET);
            out.push(Instance::rounded(
                box_.x as f32,
                box_.y as f32,
                box_.width as f32,
                box_.height as f32,
                colors.convert_opaque(fill),
                RADIUS_CONTROL,
                FLAG_ROUND_ALL,
            ));
        }

        // The mark on a button whose panel is open. It survives hover, so moving the
        // pointer away does not make the toolbar forget which panel is showing.
        //
        // A short rounded indicator rather than a full-width bar, matching the active
        // tab: a square bar along the bottom edge would poke out through the rounded
        // fill above it.
        if is_active && !is_pressed {
            let width = (rect.width / 3).max(1);
            out.push(Instance::rounded(
                (rect.x + (rect.width.saturating_sub(width) / 2) as i32) as f32,
                (rect.bottom() - (ACTIVE_MARKER + MARKER_LIFT) as i32) as f32,
                width as f32,
                ACTIVE_MARKER as f32,
                colors.convert_opaque(accent),
                RADIUS_PILL,
                FLAG_ROUND_ALL,
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
            // A pill, and inset vertically so it does not fill the bar's full height: a
            // coloured segment reaching both edges reads as a break in the bar rather
            // than as a badge sitting on it.
            let pill = segment.inset(0, SEGMENT_INSET);
            out.push(Instance::rounded(
                pill.x as f32,
                pill.y as f32,
                pill.width as f32,
                pill.height as f32,
                colors.convert(background),
                RADIUS_PILL,
                FLAG_ROUND_ALL,
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
            // A pill, and inset vertically so it does not fill the bar's full height: a
            // coloured segment reaching both edges reads as a break in the bar rather
            // than as a badge sitting on it.
            let pill = segment.inset(0, SEGMENT_INSET);
            out.push(Instance::rounded(
                pill.x as f32,
                pill.y as f32,
                pill.width as f32,
                pill.height as f32,
                colors.convert(background),
                RADIUS_PILL,
                FLAG_ROUND_ALL,
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

/// Gap between a tooltip's label and its keyboard shortcut.
///
/// Wide enough that the chord reads as a separate column rather than as more of the
/// sentence — "New tab ctrl+shift+t" run together is worse than no shortcut at all.
const TOOLTIP_GAP: f32 = 12.0;

/// Draw a tooltip below a hovered chrome button.
///
/// The strip buttons are bare glyphs. `+` and `×` are self-evident; the split
/// buttons are not, and a control nobody can identify may as well not be there.
///
/// `shortcut` is the chord bound to the button's action, or `None` for a button with
/// no binding — the window controls, and the two dropdowns. It is passed in rather
/// than looked up here because the binding is the user's: rebinding the chord has to
/// change what the tooltip says, so it comes from the live keymap every frame.
///
/// Drawn after the strip and clamped to the window, so a button near the right edge
/// gets a tooltip that shifts left rather than one that runs off-screen.
#[allow(clippy::too_many_arguments)]
pub fn draw_tooltip(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    button: ChromeButton,
    shortcut: Option<&str>,
    anchor: Rect,
    window: Rect,
    theme: &Theme,
    colors: ColorSpace,
) {
    let text = button.describe();
    let metrics = fonts.metrics();
    let cell = metrics.width as f32;

    let label_width = text::measure(text, cell);
    let chord_width = shortcut.map_or(0.0, |s| text::measure(s, cell) + TOOLTIP_GAP);
    let width = (label_width + chord_width) as u32 + (PADDING * 2.0) as u32;
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

    // Border then interior, rounded like the menu: both float over the terminal, and two
    // floating surfaces with different corners read as two different UIs. The interior's
    // radius steps down by the border width so the curves stay concentric.
    let rect = Rect::new(x, y, width, height);
    out.push(Instance::rounded(
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
        colors.convert_opaque(theme.split_divider()),
        RADIUS_CONTROL,
        FLAG_ROUND_ALL,
    ));
    let inner = rect.inset(1, 1);
    out.push(Instance::rounded(
        inner.x as f32,
        inner.y as f32,
        inner.width as f32,
        inner.height as f32,
        colors.convert_opaque(theme.background_focused()),
        RADIUS_CONTROL - 1.0,
        FLAG_ROUND_ALL,
    ));

    // With a shortcut the label goes left and the chord right, so the two line up as
    // columns. Without one the label is centred, which is how this has always looked
    // for the buttons that have no binding.
    let align = if shortcut.is_some() {
        Align::Left
    } else {
        Align::Center
    };
    text::draw_in_box(
        out,
        fonts,
        text,
        rect,
        PADDING,
        align,
        theme.foreground,
        colors,
        Style::Regular,
    );

    if let Some(chord) = shortcut {
        // Dimmer than the label: the chord is a reminder, not the answer to what the
        // button does, and at equal weight it competes with the thing it annotates.
        //
        // `normal.white` and not `bright.black`, which is the mistake this file has
        // already made twice — see the tab title and the button icons. Bright black is
        // the chrome's own grey, so text in it very nearly disappears.
        text::draw_in_box(
            out,
            fonts,
            chord,
            rect,
            PADDING,
            Align::Right,
            theme.muted_foreground(),
            colors,
            Style::Regular,
        );
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

    /// Bounding box of everything a tooltip drew, or `None` if it drew nothing.
    fn tooltip_bounds(shortcut: Option<&str>, anchor: Rect, window: Rect) -> Option<Rect> {
        let theme = tuz_config::Theme::builtin_default();
        let mut out = Vec::new();
        draw_tooltip(
            &mut out,
            &mut fonts(),
            ChromeButton::NewTab,
            shortcut,
            anchor,
            window,
            &theme,
            colors(),
        );
        let left = out.iter().map(|i| i.position[0]).reduce(f32::min)?;
        let top = out.iter().map(|i| i.position[1]).reduce(f32::min)?;
        let right = out
            .iter()
            .map(|i| i.position[0] + i.size[0])
            .reduce(f32::max)?;
        let bottom = out
            .iter()
            .map(|i| i.position[1] + i.size[1])
            .reduce(f32::max)?;
        Some(Rect::new(
            left as i32,
            top as i32,
            (right - left) as u32,
            (bottom - top) as u32,
        ))
    }

    #[test]
    fn a_tooltip_with_a_shortcut_is_wider_than_one_without() {
        // The chord needs room of its own. Reusing the label-only width would print it
        // over the label or have the renderer clip it away.
        let window = Rect::new(0, 0, 1200, 800);
        let anchor = Rect::new(400, 0, 30, 30);
        let bare = tooltip_bounds(None, anchor, window).expect("should draw");
        let with = tooltip_bounds(Some("ctrl+shift+t"), anchor, window).expect("should draw");
        assert!(
            with.width > bare.width,
            "{} is not wider than {}",
            with.width,
            bare.width
        );
    }

    #[test]
    fn a_longer_chord_takes_more_room() {
        // Guards the measurement actually being of the chord: a constant reservation
        // would pass the test above and then clip a long chord.
        let window = Rect::new(0, 0, 1200, 800);
        let anchor = Rect::new(400, 0, 30, 30);
        let short = tooltip_bounds(Some("f1"), anchor, window).expect("should draw");
        let long =
            tooltip_bounds(Some("ctrl+shift+super+alt+f12"), anchor, window).expect("should draw");
        assert!(long.width > short.width);
    }

    #[test]
    fn a_tooltip_with_a_shortcut_still_stays_inside_the_window() {
        // The extra width is what makes this worth asserting: a button at the right
        // edge now needs to shift further left than it used to.
        let window = Rect::new(0, 0, 400, 800);
        let anchor = Rect::new(380, 0, 20, 30);
        let bounds =
            tooltip_bounds(Some("ctrl+shift+super+t"), anchor, window).expect("should draw");
        assert!(
            bounds.x >= window.x,
            "ran off the left edge at x={}",
            bounds.x
        );
        assert!(
            bounds.right() <= window.right(),
            "ran off the right edge to {}",
            bounds.right()
        );
    }

    #[test]
    fn the_label_and_the_chord_do_not_overlap() {
        // Two `draw_in_box` calls into the same rect, one left-aligned and one right.
        // Nothing but the reserved gap keeps them apart.
        let theme = tuz_config::Theme::builtin_default();
        let mut label_only = Vec::new();
        draw_tooltip(
            &mut label_only,
            &mut fonts(),
            ChromeButton::NewTab,
            None,
            Rect::new(400, 0, 30, 30),
            Rect::new(0, 0, 1200, 800),
            &theme,
            colors(),
        );
        let mut both = Vec::new();
        draw_tooltip(
            &mut both,
            &mut fonts(),
            ChromeButton::NewTab,
            Some("ctrl+shift+t"),
            Rect::new(400, 0, 30, 30),
            Rect::new(0, 0, 1200, 800),
            &theme,
            colors(),
        );

        // Glyph quads are the textured ones; the first two instances are the border and
        // the fill. The chord's glyphs must all start right of where the label's end.
        let glyphs = |v: &[Instance]| -> Vec<Instance> {
            v.iter()
                .filter(|i| i.flags & crate::instance::FLAG_TEXTURED != 0)
                .copied()
                .collect()
        };
        let label_glyphs = glyphs(&label_only).len();
        let all = glyphs(&both);
        assert!(
            all.len() > label_glyphs,
            "the chord drew no glyphs of its own"
        );

        // Label glyphs come first, chord glyphs after, so the split is by index.
        let label_end = all[..label_glyphs]
            .iter()
            .map(|i| i.position[0] + i.size[0])
            .fold(f32::MIN, f32::max);
        let chord_start = all[label_glyphs..]
            .iter()
            .map(|i| i.position[0])
            .fold(f32::MAX, f32::min);
        assert!(
            chord_start > label_end,
            "the chord starts at {chord_start}, which is inside the label ending at {label_end}"
        );
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
            !out.iter()
                .any(|i| i.position[0] == bar.x as f32 && i.size[0] == 1.0),
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
        let active_bg = colors().convert(theme.background);
        assert!(
            out.iter()
                .any(|i| i.flags & crate::FLAG_TEXTURED == 0 && i.color == active_bg),
            "the active tab should use the pane background it runs into"
        );

        // And it is a pill inset from its slot, not a full-height rectangle: the inset is
        // what leaves a gap between tabs, which is what replaced the separator rule and
        // the underline bar.
        let pill = tab_pill(tabs[0]);
        assert!(
            out.iter().any(|i| i.flags & crate::FLAG_TEXTURED == 0
                && i.color == active_bg
                && i.size[0] == pill.width as f32
                && i.size[1] == pill.height as f32),
            "the active tab's fill should be the inset pill"
        );
        assert!(
            pill.height < tabs[0].height,
            "the pill must be shorter than its slot to leave a gap"
        );
    }

    #[test]
    fn only_one_tab_is_filled() {
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

        // The fill is the only thing saying which tab is active, so exactly one tab may
        // have it — with four tabs drawn and the third active.
        let active_bg = colors().convert(theme.background);
        let pill = tab_pill(tabs[0]);
        let fills = out
            .iter()
            .filter(|i| {
                i.flags & crate::FLAG_TEXTURED == 0
                    && i.color == active_bg
                    && i.size[1] == pill.height as f32
            })
            .count();
        assert_eq!(fills, 1, "exactly one tab is filled");
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

        // Whatever the dim slot is, not a specific colour: the point is that an
        // idle tab recedes and a busy one does not, and pinning the exact shade here
        // made the test fail when the shade was made readable.
        let dim = colors().convert_opaque(theme.muted_foreground());
        let lit = colors().convert_opaque(theme.foreground);
        let count = |out: &[Instance], color: [f32; 4]| {
            out.iter()
                .filter(|i| i.flags & crate::FLAG_TEXTURED != 0 && i.color == color)
                .count()
        };

        // Idle: the inactive tab recedes. With output: it does not.
        assert!(
            count(&idle, dim) > 0,
            "an idle background tab should be dimmed"
        );
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
            out.iter()
                .any(|i| i.flags & crate::FLAG_TEXTURED == 0 && i.color == green),
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
        // The largest untextured quad is the button's fill: the icon is drawn from small
        // pieces of geometry and the active indicator is a sliver. Matching on the exact
        // slot size broke when the fill was inset from its slot, which is a change in how
        // it looks and not in which colour it is — the thing this test is about.
        let fill_of = |out: &[Instance]| {
            out.iter()
                .filter(|i| i.flags & crate::FLAG_TEXTURED == 0)
                .max_by(|a, b| {
                    (a.size[0] * a.size[1])
                        .partial_cmp(&(b.size[0] * b.size[1]))
                        .expect("instance sizes are finite")
                })
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
        // And it must not be the accent. The accent marks state on this strip — the active
        // indicator, a focused control — so spending it on a transient press made every
        // click read as "this is now selected".
        assert_ne!(
            press,
            Some(colors().convert_opaque(theme.cursor())),
            "a press must not use the accent color"
        );
        assert_eq!(
            press,
            Some(colors().convert_opaque(theme.chrome_divider())),
            "a press is a step further than hover on the same neutral"
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
        );
        let red = colors().convert_opaque(Theme::builtin_default().normal.red);
        assert!(
            out.iter().any(|i| i.color == red),
            "the button that loses work should say so"
        );
    }
}
