//! Finds installed TrueType fonts by family and style, so edited text can fall back to a font that
//! looks like the original when the PDF's own (usually subset) font lacks the typed characters.

use crate::pdf::FontInfo;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

struct Face {
    family: String,
    bold: bool,
    italic: bool,
    /// A plain style (Regular, Bold, Italic, Bold Italic) rather than a width or weight variant.
    plain: bool,
    path: PathBuf,
}

fn faces() -> &'static [Face] {
    FACES.get_or_init(scan)
}

static FACES: OnceLock<Vec<Face>> = OnceLock::new();

/// Start reading the installed fonts in the background, so the first text edit does not wait.
pub fn warm_up() {
    std::thread::spawn(|| {
        let _ = faces();
    });
}

/// "Times New Roman PS-BoldMT" → "timesnewroman".
fn norm(s: &str) -> String {
    let mut n: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
    for suffix in ["psmt", "mt", "ps"] {
        if n.len() > suffix.len() + 2 && n.ends_with(suffix) {
            n.truncate(n.len() - suffix.len());
        }
    }
    n
}

fn scan() -> Vec<Face> {
    let mut dirs = vec![PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into())).join("Fonts")];
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft\\Windows\\Fonts"));
    }
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("ttf")) {
                continue;
            }
            if let Some((family, style, bold, italic)) = read_names(&path) {
                let s = style.to_lowercase();
                let plain = ["regular", "normal", "roman", "bold", "italic", "oblique", "bold italic", "bold oblique"].contains(&s.as_str());
                out.push(Face { family: norm(&family), bold, italic, plain, path });
            }
        }
    }
    out
}

/// Reads only the table directory, `name` and `OS/2` tables of a font file.
fn read_names(path: &Path) -> Option<(String, String, bool, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    let n = u16::from_be_bytes([head[4], head[5]]) as usize;
    let mut dir = vec![0u8; n * 16];
    f.read_exact(&mut dir).ok()?;
    let mut table = |tag: &[u8; 4]| -> Option<Vec<u8>> {
        let rec = dir.as_chunks::<16>().0.iter().find(|r| &r[0..4] == tag)?;
        let off = u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as u64;
        let len = u32::from_be_bytes([rec[12], rec[13], rec[14], rec[15]]) as usize;
        if len > 1 << 20 {
            return None;
        }
        let mut buf = vec![0u8; len];
        f.seek(SeekFrom::Start(off)).ok()?;
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    };
    let name = table(b"name")?;
    let os2 = table(b"OS/2");
    let u16_at = |b: &[u8], o: usize| b.get(o..o + 2).map(|s| u16::from_be_bytes([s[0], s[1]]));
    let count = u16_at(&name, 2)? as usize;
    let storage = u16_at(&name, 4)? as usize;
    // Typographic family (16), family (1), typographic style (17), style (2).
    let mut found: [Option<String>; 4] = [None, None, None, None];
    for k in 0..count {
        let r = 6 + k * 12;
        let (platform, encoding, id) = (u16_at(&name, r)?, u16_at(&name, r + 2)?, u16_at(&name, r + 6)?);
        let (len, off) = (u16_at(&name, r + 8)? as usize, u16_at(&name, r + 10)? as usize);
        let slot = match id {
            16 => 0,
            1 => 1,
            17 => 2,
            2 => 3,
            _ => continue,
        };
        if platform != 3 || encoding > 10 || found[slot].is_some() {
            continue;
        }
        let bytes = name.get(storage + off..storage + off + len)?;
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        found[slot] = Some(String::from_utf16_lossy(&units));
    }
    let [typo, fam, typo_style, style] = found;
    let family = typo.or(fam)?;
    let style = typo_style.or(style).unwrap_or_default();
    let (weight, selection) = os2.as_deref().map(|b| (u16_at(b, 4).unwrap_or(400), u16_at(b, 62).unwrap_or(0))).unwrap_or((400, 0));
    Some((family, style, weight >= 600 || selection & 0x20 != 0, selection & 0x01 != 0))
}

fn pick(family: &str, bold: bool, italic: bool) -> Option<&'static Path> {
    let all: Vec<&Face> = faces().iter().filter(|f| f.family == family).collect();
    let score = |f: &&Face| (f.bold == bold) as i32 * 2 + (f.italic == italic) as i32 + f.plain as i32 * 4;
    all.into_iter().max_by_key(score).map(|f| f.path.as_path())
}

/// The installed font closest to the PDF font described by `info`.
pub fn closest(info: &FontInfo) -> Option<&'static Path> {
    let (bold, italic) = (info.bold(), info.italic());
    let clean = info.clean_name();
    let family_part = clean.split(['-', ',']).next().unwrap_or(clean);
    for candidate in [norm(family_part), norm(&info.family), norm(clean)] {
        if candidate.is_empty() {
            continue;
        }
        if let Some(p) = pick(&candidate, bold, italic) {
            return Some(p);
        }
    }
    let lower = clean.to_lowercase();
    let generic = if info.fixed() || lower.contains("courier") || lower.contains("mono") {
        "couriernew"
    } else if info.serif()
        || ["times", "roman", "georgia", "garamond", "cambria", "minion", "palatino", "antiqua", "century", "bookman"].iter().any(|k| lower.contains(k))
        || lower.contains("serif") && !lower.contains("sans")
    {
        "timesnewroman"
    } else {
        "arial"
    };
    pick(generic, bold, italic).or_else(|| pick("arial", false, false))
}

/// A font for new text: "sans", "serif" or "mono", optionally bold or italic.
pub fn generic(kind: &str, bold: bool, italic: bool) -> Option<&'static Path> {
    let family = match kind {
        "serif" => "timesnewroman",
        "mono" => "couriernew",
        _ => "arial",
    };
    pick(family, bold, italic)
}

/// A handwriting font for typed signatures.
pub fn script() -> Option<&'static Path> {
    ["segoescript", "inkfree", "lucidahandwriting", "segoeprint"].iter().find_map(|f| pick(f, false, false))
}
