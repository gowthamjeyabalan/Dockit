//! The Dockit mark: a page outline left open at its top right corner, where a blue dot sits as the
//! folded corner. Same language as the Grapht and Overlay marks (round strokes, one blue dot).
//! Geometry is in a 60-unit box. Shared by the interface (theme.rs) and the icon (build.rs).

/// The outline, drawn with round caps and joins.
pub const OUTLINE: [(f32, f32); 5] = [(33.0, 8.0), (14.0, 8.0), (14.0, 52.0), (46.0, 52.0), (46.0, 21.0)];
pub const STROKE: f32 = 7.0;
/// The dot: centre and radius.
pub const DOT: (f32, f32, f32) = (45.0, 9.0, 7.5);

pub const NIGHT: [u8; 3] = [0x14, 0x16, 0x1c];
pub const PAPER: [u8; 3] = [0xf5, 0xf5, 0xf7];
pub const BLUE: [u8; 3] = [0x0a, 0x84, 0xff];

fn dist_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len = abx * abx + aby * aby;
    let t = if len > 0.0 { (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len).clamp(0.0, 1.0) } else { 0.0 };
    let (x, y) = (a.0 + abx * t - p.0, a.1 + aby * t - p.1);
    (x * x + y * y).sqrt()
}

/// The app icon as RGBA pixels: the mark in Paper and Blue on a rounded Night square.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let n = size as f32;
    let s = n / 60.0;
    let radius = 13.0 * s;
    let mut out = vec![0u8; (size * size * 4) as usize];
    const SS: u32 = 4;
    for y in 0..size {
        for x in 0..size {
            let (mut bg, mut ink, mut dot) = (0.0, 0.0, 0.0);
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
                    // The mark sits a little inside the square.
                    let m = 0.82;
                    let u = ((px / s - 30.0) / m + 30.0, (py / s - 30.0) / m + 30.0);
                    if (u.0 - DOT.0).hypot(u.1 - DOT.1) <= DOT.2 {
                        dot += 1.0;
                    } else if OUTLINE.windows(2).any(|w| dist_to_segment(u, w[0], w[1]) <= STROKE / 2.0) {
                        ink += 1.0;
                    }
                }
            }
            let total = (SS * SS) as f32;
            let (bg, ink, dot) = (bg / total, ink / total, dot / total);
            if bg == 0.0 {
                continue;
            }
            let night = bg - ink - dot;
            let mix = |k: usize| ((NIGHT[k] as f32 * night + PAPER[k] as f32 * ink + BLUE[k] as f32 * dot) / bg).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&[mix(0), mix(1), mix(2), (bg * 255.0).round() as u8]);
        }
    }
    out
}
