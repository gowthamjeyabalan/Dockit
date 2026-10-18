//! The Dockit mark: a lowercase "d" whose bowl holds the blue dot. Same language as the Grapht and
//! Overlay marks (round strokes, one dot). Geometry is in a 100-unit box with a 10-unit stroke,
//! centred like Overlay's. Shared by the interface (theme.rs) and the icon (build.rs).

/// The bowl: centre and radius (of the stroke's middle). Large, so the counter stays open.
pub const BOWL: (f32, f32, f32) = (46.0, 60.0, 24.0);
/// The stem: x, the centre of its round top, and its flat foot on the baseline (the bowl's
/// bottom edge), as in a geometric typeface.
pub const STEM: (f32, f32, f32) = (70.0, 14.0, 89.0);
pub const STROKE: f32 = 10.0;
/// The dot: centre and radius.
pub const DOT: (f32, f32, f32) = (46.0, 60.0, 8.0);
/// The letter's extent (x from, x to, y from, y to).
pub const EXTENT: (f32, f32, f32, f32) = (17.0, 75.0, 9.0, 89.0);
/// How much of the icon's square the letter's height takes.
const ICON_FILL: f32 = 0.66;
/// Corner radius of the icon's square (as Overlay's).
pub const TILE_RADIUS: f32 = 22.4;

/// The app icon is white on Hub Blue, like Overlay's and Grapht's.
pub const BLUE: [u8; 3] = [0x0a, 0x84, 0xff];
pub const WHITE: [u8; 3] = [0xff, 0xff, 0xff];

/// Is the letter point `u` on the ink (bowl or stem), and is it on the dot?
fn hit(u: (f32, f32)) -> (bool, bool) {
    let half = STROKE / 2.0;
    let dot = (u.0 - DOT.0).hypot(u.1 - DOT.1) <= DOT.2;
    let bowl = ((u.0 - BOWL.0).hypot(u.1 - BOWL.1) - BOWL.2).abs() <= half;
    let (x, top, foot) = STEM;
    let stem = ((u.0 - x).abs() <= half && u.1 >= top && u.1 <= foot) || (u.0 - x).hypot(u.1 - top) <= half;
    (bowl || stem, dot)
}

/// Coverage masks (ink, dot) of the bare letter, `size` pixels square, cropped to the letter
/// (a square around EXTENT), for drawing the mark in the interface with the same shape as the icon.
#[allow(dead_code)]
pub fn masks(size: u32) -> (Vec<u8>, Vec<u8>) {
    let n = size as f32;
    let (mut ink, mut dot) = (vec![0u8; (size * size) as usize], vec![0u8; (size * size) as usize]);
    const SS: u32 = 4;
    // A square around the letter, centred on it.
    let (x0, x1, y0, y1) = EXTENT;
    let side = (x1 - x0).max(y1 - y0);
    let (ox, oy) = ((x0 + x1 - side) / 2.0, (y0 + y1 - side) / 2.0);
    for y in 0..size {
        for x in 0..size {
            let (mut a, mut b) = (0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (ox + (x as f32 + (sx as f32 + 0.5) / SS as f32) / n * side, oy + (y as f32 + (sy as f32 + 0.5) / SS as f32) / n * side);
                    match hit(u) {
                        (_, true) => b += 1,
                        (true, false) => a += 1,
                        _ => {}
                    }
                }
            }
            let i = (y * size + x) as usize;
            ink[i] = (a * 255 / (SS * SS)) as u8;
            dot[i] = (b * 255 / (SS * SS)) as u8;
        }
    }
    (ink, dot)
}

/// The app icon as RGBA pixels: the mark in white on a rounded Hub Blue square.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let n = size as f32;
    let s = n / 100.0;
    let radius = TILE_RADIUS * s;
    let mut out = vec![0u8; (size * size * 4) as usize];
    const SS: u32 = 4;
    for y in 0..size {
        for x in 0..size {
            let (mut bg, mut ink) = (0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    // Rounded square.
                    let qx = (px - n / 2.0).abs() - (n / 2.0 - radius);
                    let qy = (py - n / 2.0).abs() - (n / 2.0 - radius);
                    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
                    if d > 0.0 {
                        continue;
                    }
                    bg += 1.0;
                    // The letter, centred in the square at ICON_FILL of its height.
                    let (x0, x1, y0, y1) = EXTENT;
                    let k = (y1 - y0) / (100.0 * ICON_FILL);
                    let u = ((x0 + x1) / 2.0 + (px / s - 50.0) * k, (y0 + y1) / 2.0 + (py / s - 50.0) * k);
                    let (on_ink, on_dot) = hit(u);
                    if on_ink || on_dot {
                        ink += 1.0;
                    }
                }
            }
            let total = (SS * SS) as f32;
            let (bg, ink) = (bg / total, ink / total);
            if bg == 0.0 {
                continue;
            }
            let mix = |k: usize| ((BLUE[k] as f32 * (bg - ink) + WHITE[k] as f32 * ink) / bg).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&[mix(0), mix(1), mix(2), (bg * 255.0).round() as u8]);
        }
    }
    out
}
