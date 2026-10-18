//! Copies pdfium.dll next to the built executable so `cargo run` finds it, and gives the Windows
//! executable its icon (drawn from the mark in src/mark.rs) and version details.

#[path = "src/mark.rs"]
mod mark;

use std::path::PathBuf;

/// An .ico file with 32-bit images at the usual sizes.
fn ico(sizes: &[u32]) -> Vec<u8> {
    let mut images = Vec::new();
    for &s in sizes {
        let rgba = mark::icon_rgba(s);
        let mut img = Vec::new();
        // BITMAPINFOHEADER; the height counts the colour rows and the (empty) mask rows.
        for v in [40u32, s, s * 2] {
            img.extend_from_slice(&v.to_le_bytes());
        }
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        for _ in 0..6 {
            img.extend_from_slice(&0u32.to_le_bytes());
        }
        for y in (0..s).rev() {
            for x in 0..s {
                let i = ((y * s + x) * 4) as usize;
                img.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        let mask_row = s.div_ceil(32) * 4;
        img.extend(std::iter::repeat_n(0u8, (mask_row * s) as usize));
        images.push((s, img));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, img) in &images {
        let b = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[b, b, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for (_, img) in images {
        out.extend_from_slice(&img);
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=pdfium/pdfium.dll");
    println!("cargo:rerun-if-changed=src/mark.rs");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("pdfium").join("pdfium.dll");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR is target/<profile>/build/<crate>-<hash>/out; the exe sits in target/<profile>.
    if let Some(profile_dir) = out.ancestors().nth(3) {
        let dst = profile_dir.join("pdfium.dll");
        let stale = std::fs::metadata(&dst).ok().map(|m| m.len()) != std::fs::metadata(&src).ok().map(|m| m.len());
        if src.exists()
            && stale
            && let Err(e) = std::fs::copy(&src, &dst)
        {
            println!("cargo:warning=Could not copy pdfium.dll next to the executable: {e}");
        }
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = out.join("dockit.ico");
        if std::fs::write(&icon, ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).is_ok() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon(icon.to_str().unwrap()).set("ProductName", "Dockit").set("FileDescription", "Dockit").set("LegalCopyright", "");
            if let Err(e) = res.compile() {
                // A missing resource compiler should not stop the build; the exe just has no icon.
                println!("cargo:warning=Could not embed the icon: {e}");
            }
        }
    }
}
