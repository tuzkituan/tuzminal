//! The window icon, drawn in code rather than loaded from a file.
//!
//! Generating the pixels avoids pulling in an image decoder for one 64x64 asset, and
//! avoids the packaging problem of a binary that has to find a PNG next to itself at
//! runtime. The shape is deliberately close to the Windows Command Prompt mark
//! everyone already recognises as "a terminal": a dark rounded square with a chevron
//! prompt and a caret beside it.
//!
//! A matching `tuzminal.svg` ships in `assets/` for desktop-environment integration,
//! since Wayland takes the icon from the `.desktop` file rather than from the window.

/// Side of the generated icon, in pixels. 64 is what most compositors ask for, and
/// scaling down from it looks better than scaling up from 32.
const SIZE: u32 = 64;

/// Background, foreground and accent, as straight RGB.
const BACKGROUND: [u8; 3] = [0x14, 0x17, 0x20];
const CHEVRON: [u8; 3] = [0x5c, 0xf0, 0xd4];
const CARET: [u8; 3] = [0xe6, 0xe9, 0xf0];

/// Corner radius, matching the window's own rounding closely enough to read as the
/// same product.
const RADIUS: f32 = 12.0;

/// The icon as RGBA8 rows, top to bottom, alongside its side length.
pub fn rgba() -> (Vec<u8>, u32) {
    let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];

    for y in 0..SIZE {
        for x in 0..SIZE {
            // Coverage of the rounded square, so the corners fade rather than step.
            let alpha = rounded_coverage(x as f32 + 0.5, y as f32 + 0.5);
            if alpha <= 0.0 {
                continue;
            }

            let color = glyph_color(x, y).unwrap_or(BACKGROUND);
            let o = ((y * SIZE + x) * 4) as usize;
            pixels[o] = color[0];
            pixels[o + 1] = color[1];
            pixels[o + 2] = color[2];
            pixels[o + 3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    (pixels, SIZE)
}

/// Signed-distance coverage of a rounded square covering the whole icon.
fn rounded_coverage(x: f32, y: f32) -> f32 {
    let half = SIZE as f32 / 2.0;
    let px = (x - half).abs() - (half - RADIUS);
    let py = (y - half).abs() - (half - RADIUS);
    let qx = px.max(0.0);
    let qy = py.max(0.0);
    let distance = (qx * qx + qy * qy).sqrt() + px.max(py).min(0.0) - RADIUS;
    // One pixel of smoothing across the edge.
    (0.5 - distance).clamp(0.0, 1.0)
}

/// The prompt drawn over the background: a `>` chevron and a caret to its right.
fn glyph_color(x: u32, y: u32) -> Option<[u8; 3]> {
    let (x, y) = (x as i32, y as i32);

    // The chevron is two strokes meeting at a point, described as "distance from the
    // diagonal" so the thickness is uniform along both arms rather than pinching at
    // the join the way two drawn rectangles would.
    const APEX_X: i32 = 30;
    const APEX_Y: i32 = 32;
    const ARM: i32 = 11;
    const THICKNESS: i32 = 5;

    let dy = y - APEX_Y;
    if dy.abs() <= ARM {
        let arm_x = APEX_X - dy.abs();
        if (x - arm_x).abs() * 2 <= THICKNESS * 2 && x <= APEX_X + THICKNESS {
            return Some(CHEVRON);
        }
    }

    // The caret: a short underscore where the cursor would sit.
    if (38..=50).contains(&x) && (36..=41).contains(&y) {
        return Some(CARET);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_a_square_rgba_buffer() {
        let (pixels, size) = rgba();
        assert_eq!(size, SIZE);
        assert_eq!(
            pixels.len(),
            (SIZE * SIZE * 4) as usize,
            "winit reads the buffer as exactly size*size*4 and panics otherwise"
        );
    }

    #[test]
    fn the_corners_are_transparent_and_the_middle_is_not() {
        let (pixels, size) = rgba();
        let alpha_at = |x: u32, y: u32| pixels[((y * size + x) * 4 + 3) as usize];

        // Rounding is the whole reason for the coverage function; a fully opaque
        // corner means it silently degraded to a plain square.
        assert_eq!(alpha_at(0, 0), 0, "the top-left corner should be cut away");
        assert_eq!(alpha_at(size - 1, 0), 0);
        assert_eq!(alpha_at(0, size - 1), 0);
        assert_eq!(alpha_at(size - 1, size - 1), 0);
        assert_eq!(alpha_at(size / 2, size / 2), 255, "the middle is solid");
    }

    #[test]
    fn the_prompt_is_actually_drawn() {
        let (pixels, size) = rgba();
        let mut chevron = 0;
        let mut caret = 0;
        for chunk in pixels.chunks_exact(4) {
            match [chunk[0], chunk[1], chunk[2]] {
                c if c == CHEVRON => chevron += 1,
                c if c == CARET => caret += 1,
                _ => {}
            }
        }
        // Without this the icon is a featureless rounded square, which is a shape, not
        // an icon for a terminal.
        assert!(chevron > 100, "the chevron should cover real area, got {chevron}");
        assert!(caret > 40, "the caret should cover real area, got {caret}");
        assert!(size > 0);
    }
}
