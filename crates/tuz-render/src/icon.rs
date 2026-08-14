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

/// Side of the icon box, as a fraction of the button. The rest is breathing room.
///
/// Landed on by eye: 0.4 crowded the strip and 0.28 was too faint to read at a
/// glance, so the icons sit between the two.
const ICON_SCALE: f32 = 0.34;
/// Stroke width, as a fraction of the icon box.
const STROKE_SCALE: f32 = 0.12;
/// Thinnest stroke that still survives rasterization.
const MIN_STROKE: f32 = 1.0;
/// Teeth around the gear. Eight reads as a gear at any size; more turns to mush.
const GEAR_TEETH: usize = 8;

/// Square icon box centered in `button`, and the stroke width to draw it with.
fn box_of(button: Rect) -> (f32, f32, f32, f32) {
    let side = (button.width.min(button.height) as f32 * ICON_SCALE).max(4.0);
    // Rounded so strokes land on pixel boundaries instead of straddling two.
    let side = side.round();
    let x = (button.x as f32 + (button.width as f32 - side) / 2.0).round();
    let y = (button.y as f32 + (button.height as f32 - side) / 2.0).round();
    let stroke = (side * STROKE_SCALE).round().max(MIN_STROKE);
    (x, y, side, stroke)
}

/// A horizontal stroke centered vertically at `cy`.
fn hbar(out: &mut Vec<Instance>, x: f32, cy: f32, width: f32, stroke: f32, color: [f32; 4]) {
    out.push(Instance::solid(x, cy - stroke / 2.0, width, stroke, color));
}

/// A vertical stroke centered horizontally at `cx`.
fn vbar(out: &mut Vec<Instance>, cx: f32, y: f32, height: f32, stroke: f32, color: [f32; 4]) {
    out.push(Instance::solid(cx - stroke / 2.0, y, stroke, height, color));
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
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let (x, y, side, stroke) = box_of(rect);
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

        // Two strokes turned a quarter turn apart. The length is the box diagonal so
        // the cross reaches the corners rather than stopping short of them.
        ChromeButton::Close => {
            let diagonal = side * std::f32::consts::SQRT_2;
            for turn in [
                std::f32::consts::FRAC_PI_4,
                -std::f32::consts::FRAC_PI_4,
            ] {
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
