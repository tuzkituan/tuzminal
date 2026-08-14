//! Chrome icons drawn from geometry rather than font glyphs.
//!
//! The buttons used to render characters — `+`, `⚙`, `⨯` and so on — which turned
//! out to be unworkable. No single font on a typical Linux box has all of them, so
//! each icon was resolved independently and they arrived from three different
//! families at three different design sizes: a 7x7 plus next to a 12x13 split next
//! to an 8x7 gear. Which font wins depends on what is installed, so the toolbar
//! looked different on every machine, and on some it was missing icons entirely.
//!
//! Drawing them from quads costs a handful of instances each and removes the font
//! from the picture: every icon is built from the same stroke width, sits in the
//! same box, and scales exactly with the bar.

use crate::instance::Instance;
use tuz_layout::{ChromeButton, Rect};
use tuz_ui::EntryKind;

/// Side of the icon box, as a fraction of the button. The rest is breathing room.
///
/// Down from 0.34, and the number is not a fresh guess: a button's slot is the strip's
/// height, so when the strip grew the icons grew with it and began to crowd a bar that had
/// just been given room to breathe. This is the fraction that keeps the icons the same
/// *absolute* size they were at the old strip height — solve `0.34 * old = s * new` — which
/// is the size nobody had a complaint about.
const ICON_SCALE: f32 = 0.30;
/// Stroke width, as a fraction of the icon box.
const STROKE_SCALE: f32 = 0.12;
/// Thinnest stroke that still survives rasterization.
const MIN_STROKE: f32 = 1.0;
/// Teeth around the gear. Eight reads as a gear at any size; more turns to mush.
const GEAR_TEETH: usize = 8;

/// Icon scale for a menu row.
///
/// Much larger than [`ICON_SCALE`], and not a second guess at the same number: a toolbar
/// button's box is the whole strip height, while a menu row hands over a square the size
/// of one cell. The same fraction of a box less than half as wide produced an icon a
/// third the size, sitting next to text at full size.
pub const MENU_ICON_SCALE: f32 = 0.72;

/// Square icon box centered in `button`, and the stroke width to draw it with.
fn box_of_scaled(button: Rect, scale: f32) -> (f32, f32, f32, f32) {
    let side = (button.width.min(button.height) as f32 * scale).max(4.0);
    // Rounded so strokes land on pixel boundaries instead of straddling two.
    let side = side.round();
    let x = (button.x as f32 + (button.width as f32 - side) / 2.0).round();
    let y = (button.y as f32 + (button.height as f32 - side) / 2.0).round();
    let stroke = (side * STROKE_SCALE).round().max(MIN_STROKE);
    (x, y, side, stroke)
}

fn box_of(button: Rect) -> (f32, f32, f32, f32) {
    box_of_scaled(button, ICON_SCALE)
}

/// A horizontal stroke centered vertically at `cy`.
fn hbar(out: &mut Vec<Instance>, x: f32, cy: f32, width: f32, stroke: f32, color: [f32; 4]) {
    out.push(Instance::solid(x, cy - stroke / 2.0, width, stroke, color));
}

/// A vertical stroke centered horizontally at `cx`.
fn vbar(out: &mut Vec<Instance>, cx: f32, y: f32, height: f32, stroke: f32, color: [f32; 4]) {
    out.push(Instance::solid(cx - stroke / 2.0, y, stroke, height, color));
}

/// A chevron centered on `(cx, cy)`, pointing down when `down` and up otherwise.
///
/// `half` is half the chevron's width, so it spans `2 * half` across and `half` tall.
///
/// The two arms have to be placed at **different** centers. `Instance::rotated` turns a
/// rect about its own center, so giving both arms the same center and opposite angles
/// draws an `×` rather than a chevron — which is exactly what the dropdown and the
/// explorer's go-up icon were showing. Each arm's center sits half the span to one side,
/// which is what makes them meet at an apex instead of crossing.
fn chevron(
    out: &mut Vec<Instance>,
    cx: f32,
    cy: f32,
    half: f32,
    stroke: f32,
    color: [f32; 4],
    down: bool,
) {
    // Each arm runs one `half` across and one `half` down, so it is the hypotenuse of a
    // right isosceles triangle: `half * sqrt(2)` long, at 45 degrees.
    let len = half * std::f32::consts::SQRT_2;
    // Negative for a down chevron: the shader's rotation runs the other way from the
    // screen's y axis, so the intuitive sign points the arrow the wrong way.
    let sign = if down { -1.0 } else { 1.0 };
    for side in [-1.0_f32, 1.0] {
        out.push(Instance::rotated(
            cx + side * half / 2.0 - len / 2.0,
            cy - stroke / 2.0,
            len,
            stroke,
            color,
            // The left arm descends and the right one climbs, mirrored for an up chevron.
            side * sign * std::f32::consts::FRAC_PI_4,
        ));
    }
}

/// A hollow square, as four strokes rather than a fill over a fill.
///
/// Four strokes because a smaller quad punched out of a larger one would have to be
/// painted in the button's background color, which changes on hover — an outline
/// that only looks right when not hovered is worse than one more instance.
fn outline(out: &mut Vec<Instance>, x: f32, y: f32, side: f32, stroke: f32, color: [f32; 4]) {
    hbar(out, x, y + stroke / 2.0, side, stroke, color);
    hbar(out, x, y + side - stroke / 2.0, side, stroke, color);
    vbar(out, x + stroke / 2.0, y, side, stroke, color);
    vbar(out, x + side - stroke / 2.0, y, side, stroke, color);
}

/// Draw a file-list row's icon centered in `rect`.
///
/// Geometry rather than a glyph, for the reason the toolbar icons are: no font is
/// guaranteed to carry a folder character, and sourcing one per codepoint gave icons
/// at three different design sizes depending on which font happened to win.
pub fn entry(out: &mut Vec<Instance>, kind: EntryKind, rect: Rect, color: [f32; 4]) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let (x, y, side, stroke) = box_of(rect);
    let (cx, cy) = (x + side / 2.0, y + side / 2.0);

    match kind {
        // An upward chevron: the same shape as "go up" everywhere else.
        EntryKind::Parent => chevron(out, cx, cy, side * 0.45, stroke, color, false),

        // A folder: a body with a raised tab along the top-left.
        EntryKind::Directory => {
            let tab_h = side * 0.16;
            let body_y = y + tab_h;
            out.push(Instance::solid(x, y, side * 0.45, tab_h * 2.0, color));
            out.push(Instance::solid(x, body_y, side, side - tab_h, color));
        }

        // A page with the top-right corner cut away, which is what makes it read as
        // a document rather than a plain rectangle.
        EntryKind::File => {
            let w = side * 0.78;
            let fold = w * 0.34;
            let left = cx - w / 2.0;
            out.push(Instance::solid(left, y, w - fold, side, color));
            out.push(Instance::solid(
                left + w - fold,
                y + fold,
                fold,
                side - fold,
                color,
            ));
        }

        // A page with a chevron over it: a link points somewhere else.
        EntryKind::Symlink => {
            let w = side * 0.78;
            out.push(Instance::solid(cx - w / 2.0, y, w, side, color));
            let arm = side * 0.3;
            for turn in [std::f32::consts::FRAC_PI_4, -std::f32::consts::FRAC_PI_4] {
                out.push(Instance::rotated(
                    cx - arm / 2.0,
                    cy - stroke / 2.0,
                    arm,
                    stroke,
                    // Punched out of the page, so it reads on top of it.
                    [0.0, 0.0, 0.0, color[3]],
                    turn,
                ));
            }
        }
    }
}

/// Draw `button`'s icon centered in its rect.
///
/// `background` is whatever was painted behind the icon, needed only by the gear,
/// whose hub is a ring: the hole is punched by drawing a smaller disc back in the
/// background color.
pub fn draw(
    out: &mut Vec<Instance>,
    button: ChromeButton,
    rect: Rect,
    color: [f32; 4],
    background: [f32; 4],
) {
    draw_scaled(out, button, rect, ICON_SCALE, color, background);
}

/// Draw `button`'s icon at an explicit fraction of `rect`.
///
/// Exists because the same icon is asked for at two very different box sizes: a toolbar
/// button hands over the whole strip height, a menu row hands over one cell. A single
/// fraction cannot serve both, and the caller is the only one that knows which it is.
pub fn draw_scaled(
    out: &mut Vec<Instance>,
    button: ChromeButton,
    rect: Rect,
    scale: f32,
    color: [f32; 4],
    background: [f32; 4],
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let (x, y, side, stroke) = box_of_scaled(rect, scale);
    let (cx, cy) = (x + side / 2.0, y + side / 2.0);

    match button {
        ChromeButton::NewTab => {
            hbar(out, x, cy, side, stroke, color);
            vbar(out, cx, y, side, stroke, color);
        }

        // A single bar on the baseline, not through the middle: minimize means "down
        // to the bottom", and a centered bar reads as a subtract sign.
        ChromeButton::Minimize => hbar(out, x, y + side - stroke / 2.0, side, stroke, color),

        ChromeButton::Maximize => outline(out, x, y, side, stroke, color),

        // Three bars: the menu mark, and the only icon here that is meant to read as
        // "more things than fit on a toolbar".
        ChromeButton::AppMenu => {
            for i in 0..3 {
                let gap = (side - stroke) / 2.0;
                hbar(
                    out,
                    x,
                    y + stroke / 2.0 + i as f32 * gap,
                    side,
                    stroke,
                    color,
                );
            }
        }

        // Two overlapping panels: the "extension" mark, and distinct at this size
        // from the explorer's panel-with-a-column.
        ChromeButton::Plugins => {
            let small = side * 0.62;
            outline(out, x, y + side - small, small, stroke, color);
            outline(out, x + side - small, y, small, stroke, color);
        }

        // A downward chevron, the same shape a dropdown wears everywhere.
        //
        // The arms are most of the box, not a third of it. Two 45° arms span
        // `arm * sqrt(2)` across, so a third of the box made a chevron half the width of
        // every other icon — at toolbar size that is four pixels, which reads as a dot
        // rather than as an arrow.
        ChromeButton::NewTabMenu => chevron(out, cx, cy, side * 0.45, stroke, color, true),

        // A blocky question mark. Built from bars rather than a glyph for the same
        // reason as everything else here, and squared off rather than curved because
        // an arc approximated at sixteen pixels reads as a smudge.
        ChromeButton::Help => {
            let left = cx - side * 0.28;
            let right = cx + side * 0.28;
            let hook_bottom = y + side * 0.46;

            // The top of the hook, then down its right side, then back to the centre.
            hbar(out, left, y + stroke / 2.0, right - left, stroke, color);
            vbar(out, right - stroke / 2.0, y, hook_bottom - y, stroke, color);
            hbar(
                out,
                cx,
                hook_bottom - stroke / 2.0,
                right - cx,
                stroke,
                color,
            );

            // The stem, and the dot beneath it with a gap so the two stay distinct.
            vbar(out, cx, hook_bottom, side * 0.22, stroke, color);
            let dot = stroke * 1.4;
            out.push(Instance::solid(
                cx - dot / 2.0,
                y + side - dot,
                dot,
                dot,
                color,
            ));
        }

        // A panel with a filled column down its left: the sidebar this opens.
        ChromeButton::Explorer => {
            outline(out, x, y, side, stroke, color);
            out.push(Instance::solid(x, y, side * 0.36, side, color));
        }

        // Two strokes turned a quarter turn apart. The length is the box diagonal so
        // the cross reaches the corners rather than stopping short of them.
        ChromeButton::Close => {
            let diagonal = side * std::f32::consts::SQRT_2;
            for turn in [std::f32::consts::FRAC_PI_4, -std::f32::consts::FRAC_PI_4] {
                out.push(Instance::rotated(
                    cx - diagonal / 2.0,
                    cy - stroke / 2.0,
                    diagonal,
                    stroke,
                    color,
                    turn,
                ));
            }
        }

        ChromeButton::SplitRight => {
            outline(out, x, y, side, stroke, color);
            vbar(out, cx, y, side, stroke, color);
        }
        ChromeButton::SplitDown => {
            outline(out, x, y, side, stroke, color);
            hbar(out, x, cy, side, stroke, color);
        }

        // A ring with teeth around it. Drawn as a filled disc with a smaller disc of
        // teeth-colored nothing would need the background color, so the hub is left
        // solid and the gear reads from its silhouette.
        ChromeButton::Settings => {
            let hub = side * 0.30;
            let tooth_len = side * 0.22;
            let tooth_w = (stroke * 1.6).max(MIN_STROKE);

            for i in 0..GEAR_TEETH {
                let angle = std::f32::consts::TAU * i as f32 / GEAR_TEETH as f32;
                // Each tooth is a short bar pushed out along its own angle, then
                // turned to match, so the set forms a ring rather than a row.
                let mid = hub + tooth_len / 2.0;
                let (dx, dy) = (angle.cos() * mid, angle.sin() * mid);
                out.push(Instance::rotated(
                    cx + dx - tooth_len / 2.0,
                    cy + dy - tooth_w / 2.0,
                    tooth_len,
                    tooth_w,
                    color,
                    angle,
                ));
            }

            // The body, then a hole punched back out of it. Without the hole the
            // shape reads as a sun rather than a gear.
            out.push(Instance::circle(cx, cy, hub + stroke * 0.5, color));
            out.push(Instance::circle(cx, cy, hub * 0.45, background));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arms(button: ChromeButton) -> Vec<Instance> {
        let mut out = Vec::new();
        draw(
            &mut out,
            button,
            Rect::new(0, 0, 40, 40),
            [1.0; 4],
            [0.0; 4],
        );
        out.into_iter().filter(|i| i.rotation != 0.0).collect()
    }

    #[test]
    fn a_chevrons_two_arms_meet_at_an_apex_rather_than_crossing() {
        // `Instance::rotated` turns a rect about its own center, so two arms sharing a
        // center and given opposite angles draw an `x`. That is what the dropdown button
        // and the explorer's go-up icon both showed, and it is invisible in a unit test
        // that only counts instances — so this asserts the centers actually differ.
        let arms = arms(ChromeButton::NewTabMenu);
        assert_eq!(arms.len(), 2, "a chevron is two rotated strokes");

        let center = |i: &Instance| i.position[0] + i.size[0] / 2.0;
        let apart = (center(&arms[0]) - center(&arms[1])).abs();
        assert!(
            apart > 2.0,
            "the arms are {apart}px apart, so they cross instead of meeting"
        );

        // Opposite directions, or both arms lean the same way and it is a straight line.
        assert!(
            arms[0].rotation * arms[1].rotation < 0.0,
            "the arms must turn opposite ways"
        );
    }

    #[test]
    fn a_chevron_is_wide_enough_to_read_as_an_arrow() {
        // At a third of the icon box the whole chevron was four pixels across at toolbar
        // size, which reads as a dot. Absolute bounds against the box it is drawn in.
        let arms = arms(ChromeButton::NewTabMenu);
        let (_, _, side, _) = box_of(Rect::new(0, 0, 40, 40));

        let left = arms.iter().map(|i| i.position[0]).fold(f32::MAX, f32::min);
        let right = arms
            .iter()
            .map(|i| i.position[0] + i.size[0])
            .fold(f32::MIN, f32::max);

        assert!(
            right - left > side * 0.5,
            "the chevron spans {}px of a {side}px box",
            right - left
        );
    }
}
