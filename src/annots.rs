//! Annotations and form fields written straight into the file: arrows, measurements, text boxes,
//! callouts, comment replies, review states and new form fields. PDFium cannot draw most of these
//! itself, so each one gets its own appearance stream here, built from the annotation's own
//! dictionary (so it can be rebuilt after a move, recolour or text change).
//!
//! Changes are appended as an incremental update, like the page edits in incremental.rs.

use crate::content::num;
use crate::incremental;
use crate::pdf::Rgba;
use lopdf::{Dictionary, Object, ObjectId, Stream, StringFormat};
use std::collections::BTreeSet;

/// A set of changes to the file, written as one incremental update.
pub struct Update {
    pub doc: lopdf::Document,
    bytes: Vec<u8>,
    changed: BTreeSet<ObjectId>,
}

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn real(v: f32) -> Object {
    Object::Real(v)
}

fn nums(v: &[f32]) -> Object {
    Object::Array(v.iter().map(|&x| real(x)).collect())
}

/// A PDF text string: plain bytes for ASCII, UTF-16 with a byte order mark otherwise.
pub fn text_string(s: &str) -> Object {
    if s.is_ascii() {
        return Object::String(s.as_bytes().to_vec(), StringFormat::Literal);
    }
    let mut b = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        b.extend_from_slice(&u.to_be_bytes());
    }
    Object::String(b, StringFormat::Hexadecimal)
}

/// The reverse of `text_string`.
pub fn read_text(o: &Object) -> String {
    let Ok(b) = o.as_str() else { return String::new() };
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..].chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    b.iter().map(|&c| c as char).collect()
}

fn f32_of(o: &Object) -> Option<f32> {
    o.as_float().ok().or_else(|| o.as_i64().ok().map(|i| i as f32))
}

fn floats(d: &Dictionary, key: &[u8]) -> Option<Vec<f32>> {
    d.get(key).ok()?.as_array().ok()?.iter().map(f32_of).collect()
}

fn color_of(d: &Dictionary, key: &[u8]) -> Option<Rgba> {
    let c = floats(d, key)?;
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match c.len() {
        3 => Some([to(c[0]), to(c[1]), to(c[2]), 255]),
        1 => Some([to(c[0]), to(c[0]), to(c[0]), 255]),
        _ => None,
    }
}

fn color_array(c: Rgba) -> Object {
    nums(&[c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0])
}

fn rgb(c: Rgba) -> String {
    format!("{} {} {}", num(c[0] as f64 / 255.0), num(c[1] as f64 / 255.0), num(c[2] as f64 / 255.0))
}

fn n(v: f32) -> String {
    num(v as f64)
}

/// Local time now: year, month, day, hour, minute, second.
pub fn now() -> [u16; 6] {
    #[cfg(windows)]
    unsafe {
        let mut t = std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>();
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t);
        [t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond]
    }
    #[cfg(not(windows))]
    [2026, 1, 1, 0, 0, 0]
}

pub fn pdf_date() -> String {
    let t = now();
    format!("D:{:04}{:02}{:02}{:02}{:02}{:02}", t[0], t[1], t[2], t[3], t[4], t[5])
}

/// "21 Oct 2026, 14:05" from a PDF date string; empty if it cannot be read.
pub fn show_date(d: &str) -> String {
    let s = d.trim_start_matches("D:");
    let part = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<u32>().ok());
    let (Some(y), Some(mo), Some(day)) = (part(0, 4), part(4, 6), part(6, 8)) else { return String::new() };
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let m = MONTHS.get(mo.saturating_sub(1) as usize).copied().unwrap_or("");
    match (part(8, 10), part(10, 12)) {
        (Some(h), Some(mi)) => format!("{day} {m} {y}, {h:02}:{mi:02}"),
        _ => format!("{day} {m} {y}"),
    }
}

/// The name comments are signed with.
pub fn author() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "Dockit".into())
}

/// The character's code in WinAnsiEncoding, used by the standard fonts.
fn winansi(c: char) -> Option<u8> {
    const HIGH: &str = "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ\u{8D}Ž\u{8F}\u{90}‘’“”•–—˜™š›œ\u{9D}žŸ";
    let u = c as u32;
    if (0x20..0x7F).contains(&u) || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    HIGH.chars().position(|h| h == c).map(|k| 0x80 + k as u8)
}

/// A string literal for text in a standard font.
pub fn pdf_literal(s: &str) -> String {
    let mut out = String::from("(");
    for c in s.chars() {
        let b = winansi(c).unwrap_or(b'?');
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push(')');
    out
}

/// Whether every character can be shown in the standard fonts.
pub fn latin(s: &str) -> bool {
    s.chars().all(|c| c == '\n' || winansi(c).is_some())
}

fn helvetica() -> Object {
    let mut f = Dictionary::new();
    f.set("Type", name("Font"));
    f.set("Subtype", name("Type1"));
    f.set("BaseFont", name("Helvetica"));
    f.set("Encoding", name("WinAnsiEncoding"));
    Object::Dictionary(f)
}

fn zapf() -> Object {
    let mut f = Dictionary::new();
    f.set("Type", name("Font"));
    f.set("Subtype", name("Type1"));
    f.set("BaseFont", name("ZapfDingbats"));
    Object::Dictionary(f)
}

/// A form XObject for an appearance, drawn in page coordinates (BBox = the area it covers).
fn appearance(bbox: [f32; 4], content: String, fonts: &[(&str, Object)]) -> Stream {
    let mut d = Dictionary::new();
    d.set("Type", name("XObject"));
    d.set("Subtype", name("Form"));
    d.set("BBox", nums(&bbox));
    if !fonts.is_empty() {
        let mut f = Dictionary::new();
        for (k, v) in fonts {
            f.set(*k, v.clone());
        }
        let mut r = Dictionary::new();
        r.set("Font", Object::Dictionary(f));
        d.set("Resources", Object::Dictionary(r));
    }
    let mut s = Stream::new(d, content.into_bytes());
    let _ = s.compress();
    s
}

/// A form XObject in its own coordinates (bbox), with Helvetica available as /Helv.
pub fn form_xobject(bbox: [f32; 4], content: String) -> Stream {
    appearance(bbox, content, &[("Helv", helvetica())])
}

// ── measuring text ──

/// Helvetica advance widths (per 1000 units) for ASCII 32..126.
const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584,
    584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278,
    278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
    500, 334, 260, 334, 584,
];

pub fn helv_width(s: &str, size: f32) -> f32 {
    s.chars().map(|c| if (32..127).contains(&(c as u32)) { HELV[c as usize - 32] as f32 } else { 556.0 }).sum::<f32>() * size / 1000.0
}

/// Break text into lines no wider than `width`, keeping its own line breaks.
pub fn wrap(text: &str, size: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if helv_width(&candidate, size) <= width || line.is_empty() {
                line = candidate;
                // A single word longer than the line is broken by characters.
                while helv_width(&line, size) > width && line.chars().count() > 1 {
                    let mut cut = line.len();
                    while cut > 0 && helv_width(&line[..cut], size) > width {
                        cut = line[..cut].char_indices().last().map_or(0, |(i, _)| i);
                    }
                    let cut = cut.max(line.chars().next().map_or(1, |c| c.len_utf8()));
                    lines.push(line[..cut].to_string());
                    line = line[cut..].to_string();
                }
            } else {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        lines.push(line);
    }
    lines
}

// ── shapes ──

/// Line ending styles for arrows and callouts.
fn ending(kind: &[u8], tip: (f32, f32), from: (f32, f32), w: f32) -> String {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let len = dx.hypot(dy).max(1e-3);
    let (ux, uy) = (dx / len, dy / len);
    let size = (w * 3.0 + 5.0).min(len * 0.6);
    let back = (tip.0 - ux * size, tip.1 - uy * size);
    let (px, py) = (-uy * size * 0.5, ux * size * 0.5);
    let a = (back.0 + px, back.1 + py);
    let b = (back.0 - px, back.1 - py);
    match kind {
        b"OpenArrow" => format!("{} {} m {} {} l {} {} l S\n", n(a.0), n(a.1), n(tip.0), n(tip.1), n(b.0), n(b.1)),
        b"ClosedArrow" => format!("{} {} m {} {} l {} {} l h B\n", n(a.0), n(a.1), n(tip.0), n(tip.1), n(b.0), n(b.1)),
        b"Butt" => {
            let (qx, qy) = (-uy * size * 0.6, ux * size * 0.6);
            format!("{} {} m {} {} l S\n", n(tip.0 + qx), n(tip.1 + qy), n(tip.0 - qx), n(tip.1 - qy))
        }
        _ => String::new(),
    }
}

fn line_ends(d: &Dictionary) -> (Vec<u8>, Vec<u8>) {
    match d.get(b"LE") {
        Ok(Object::Array(a)) => {
            let get = |k: usize| a.get(k).and_then(|o| o.as_name().ok()).unwrap_or(b"None").to_vec();
            (get(0), get(1))
        }
        Ok(Object::Name(nm)) => (nm.clone(), b"None".to_vec()),
        _ => (b"None".to_vec(), b"None".to_vec()),
    }
}

fn border_width(d: &Dictionary) -> f32 {
    d.get(b"BS").ok().and_then(|o| o.as_dict().ok()).and_then(|bs| bs.get(b"W").ok()).and_then(f32_of).unwrap_or(1.0)
}

fn bounds(pts: &[(f32, f32)], pad: f32) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for &(x, y) in pts {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]
}

/// Font size and text colour from a /DA string such as "/Helv 12 Tf 0 0 0 rg".
fn parse_da(da: &str) -> (f32, Rgba) {
    let t: Vec<&str> = da.split_whitespace().collect();
    let mut size = 12.0;
    let mut color = [0, 0, 0, 255];
    for (k, w) in t.iter().enumerate() {
        let f = |j: usize| t.get(j).and_then(|s| s.parse::<f32>().ok());
        match *w {
            "Tf" => size = k.checked_sub(1).and_then(f).filter(|&s| s > 0.0).unwrap_or(size),
            "rg" if k >= 3 => {
                if let (Some(r), Some(g), Some(b)) = (f(k - 3), f(k - 2), f(k - 1)) {
                    color = [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255];
                }
            }
            "g" if k >= 1 => {
                if let Some(v) = f(k - 1) {
                    let v = (v * 255.0) as u8;
                    color = [v, v, v, 255];
                }
            }
            _ => {}
        }
    }
    (size, color)
}

const TEXT_PAD: f32 = 4.0;

/// Rebuild a Line or FreeText annotation's appearance (and its /Rect) from its dictionary.
/// Returns None for other kinds, which are left as they are.
pub fn build(d: &mut Dictionary) -> Option<Stream> {
    match d.get(b"Subtype").ok()?.as_name().ok()? {
        b"Line" => Some(build_line(d)),
        b"FreeText" => Some(build_free_text(d)),
        _ => None,
    }
}

fn build_line(d: &mut Dictionary) -> Stream {
    let l = floats(d, b"L").filter(|l| l.len() == 4).unwrap_or(vec![0.0, 0.0, 0.0, 0.0]);
    let (a, b) = ((l[0], l[1]), (l[2], l[3]));
    let w = border_width(d);
    let color = color_of(d, b"C").unwrap_or([225, 87, 89, 255]);
    let (start, end) = line_ends(d);
    let mut c = format!("q {} RG {} rg {} w 1 J 1 j\n{} {} m {} {} l S\n", rgb(color), rgb(color), n(w), n(a.0), n(a.1), n(b.0), n(b.1));
    c += &ending(&start, a, b, w);
    c += &ending(&end, b, a, w);
    let mut pts = vec![a, b];
    let mut pad = w * 3.0 + 6.0;
    let caption = d.get(b"Contents").map(read_text).unwrap_or_default();
    let dimension = d.get(b"IT").ok().and_then(|o| o.as_name().ok()) == Some(b"LineDimension");
    let mut fonts = Vec::new();
    if dimension && !caption.is_empty() {
        // The length sits above the middle of the line, turned to read along it.
        let size = 9.0 + w;
        let mut ang = (b.1 - a.1).atan2(b.0 - a.0);
        if ang.abs() > std::f32::consts::FRAC_PI_2 {
            ang += std::f32::consts::PI;
        }
        let (s, co) = ang.sin_cos();
        let tw = helv_width(&caption, size);
        let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        // Back half the text width along the line, up a little across it.
        let gap = w + 2.5;
        let org = (mid.0 - co * tw / 2.0 - s * gap, mid.1 - s * tw / 2.0 + co * gap);
        c += &format!(
            "BT /Helv {} Tf {} {} {} {} {} {} Tm {} Tj ET\n",
            n(size),
            n(co),
            n(s),
            n(-s),
            n(co),
            n(org.0),
            n(org.1),
            pdf_literal(&caption)
        );
        pts.push((org.0 + co * tw - s * size, org.1 + s * tw + co * size));
        pts.push((org.0 - s * size, org.1 + co * size));
        pad += 2.0;
        fonts.push(("Helv", helvetica()));
    }
    c += "Q\n";
    let rect = bounds(&pts, pad);
    d.set("Rect", nums(&rect));
    appearance(rect, c, &fonts)
}

fn build_free_text(d: &mut Dictionary) -> Stream {
    let rect = floats(d, b"Rect").filter(|r| r.len() == 4).unwrap_or(vec![0.0, 0.0, 100.0, 20.0]);
    let rd = floats(d, b"RD").filter(|r| r.len() == 4).unwrap_or(vec![0.0; 4]);
    let inner = [rect[0] + rd[0], rect[1] + rd[1], rect[2] - rd[2], rect[3] - rd[3]];
    let da = d.get(b"DA").map(read_text).unwrap_or_default();
    let (size, text_color) = parse_da(&da);
    let border = color_of(d, b"C");
    let w = if border.is_some() { border_width(d) } else { 0.0 };
    let text = d.get(b"Contents").map(read_text).unwrap_or_default();
    let width = (inner[2] - inner[0]).max(20.0);
    let lines = wrap(&text, size, width - 2.0 * TEXT_PAD - w);
    // The box keeps its top edge and grows down to fit the text.
    let need = lines.len().max(1) as f32 * size * 1.2 + 2.0 * TEXT_PAD + w;
    let inner = [inner[0], inner[3] - (inner[3] - inner[1]).max(need), inner[0] + width, inner[3]];

    let mut c = String::from("q\n");
    let callout = floats(d, b"CL").filter(|v| v.len() == 4 || v.len() == 6);
    let mut pts = vec![(inner[0], inner[1]), (inner[2], inner[3])];
    if let (Some(cl), Some(col)) = (&callout, border) {
        let p: Vec<(f32, f32)> = cl.chunks(2).map(|q| (q[0], q[1])).collect();
        c += &format!("{} RG {} rg {} w 1 J 1 j\n{} {} m", rgb(col), rgb(col), n(w.max(0.5)), n(p[0].0), n(p[0].1));
        for q in &p[1..] {
            c += &format!(" {} {} l", n(q.0), n(q.1));
        }
        c += " S\n";
        let le = d.get(b"LE").ok().and_then(|o| o.as_name().ok()).unwrap_or(b"OpenArrow").to_vec();
        c += &ending(&le, p[0], p[1], w.max(0.5));
        pts.extend(p);
    }
    if let Some(col) = border {
        let h = w / 2.0;
        c += &format!(
            "1 1 1 rg {} RG {} w {} {} {} {} re B\n",
            rgb(col),
            n(w),
            n(inner[0] + h),
            n(inner[1] + h),
            n(inner[2] - inner[0] - w),
            n(inner[3] - inner[1] - w)
        );
    }
    c += &format!("{} {} {} {} re W n\n", n(inner[0]), n(inner[1]), n(inner[2] - inner[0]), n(inner[3] - inner[1]));
    c += &format!("BT /Helv {} Tf {} rg\n", n(size), rgb(text_color));
    let x = inner[0] + w + TEXT_PAD;
    let mut y = inner[3] - w - TEXT_PAD - size * 0.9;
    for line in &lines {
        c += &format!("1 0 0 1 {} {} Tm {} Tj\n", n(x), n(y), pdf_literal(line));
        y -= size * 1.2;
    }
    c += "ET Q\n";
    let outer = if callout.is_some() { bounds(&pts, w * 3.0 + 6.0) } else { inner };
    d.set("Rect", nums(&outer));
    if callout.is_some() {
        d.set("RD", nums(&[inner[0] - outer[0], inner[1] - outer[1], outer[2] - inner[2], outer[3] - inner[3]]));
    } else {
        d.remove(b"RD");
    }
    appearance(outer, c, &[("Helv", helvetica())])
}

/// Shift a Line or FreeText annotation's own coordinates (the caller rebuilds its appearance).
pub fn translate(d: &mut Dictionary, dx: f32, dy: f32) {
    for key in [&b"Rect"[..], b"L", b"CL"] {
        if let Some(v) = floats(d, key) {
            let moved: Vec<f32> = v.iter().enumerate().map(|(k, x)| x + if k % 2 == 0 { dx } else { dy }).collect();
            d.set(key, nums(&moved));
        }
    }
}

// ── the update ──

impl Update {
    pub fn load(bytes: Vec<u8>) -> Result<Update, String> {
        let doc = lopdf::Document::load_mem(&bytes).map_err(|e| format!("The file structure could not be read ({e})."))?;
        if doc.is_encrypted() {
            return Err("The file is encrypted.".into());
        }
        Ok(Update { doc, bytes, changed: BTreeSet::new() })
    }

    pub fn add(&mut self, o: Object) -> ObjectId {
        self.doc.max_id += 1;
        let id = (self.doc.max_id, 0);
        self.put(id, o);
        id
    }

    pub fn put(&mut self, id: ObjectId, o: Object) {
        self.doc.objects.insert(id, o);
        self.changed.insert(id);
    }

    pub fn dict(&self, id: ObjectId) -> Result<Dictionary, String> {
        self.doc.get_dictionary(id).cloned().map_err(|_| "An annotation could not be read.".into())
    }

    pub fn page_id(&self, page: usize) -> Result<ObjectId, String> {
        self.doc.get_pages().get(&(page as u32 + 1)).copied().ok_or_else(|| "The page was not found.".into())
    }

    /// The page's /Annots entries and the object holding the array, if it is a separate one.
    fn annots(&self, page_id: ObjectId) -> (Option<ObjectId>, Vec<Object>) {
        let Ok(page) = self.doc.get_dictionary(page_id) else { return (None, Vec::new()) };
        match page.get(b"Annots") {
            Ok(Object::Reference(id)) => (Some(*id), self.doc.get_object(*id).and_then(Object::as_array).cloned().unwrap_or_default()),
            Ok(Object::Array(a)) => (None, a.clone()),
            _ => (None, Vec::new()),
        }
    }

    fn set_annots(&mut self, page_id: ObjectId, holder: Option<ObjectId>, list: Vec<Object>) -> Result<(), String> {
        match holder {
            Some(id) => self.put(id, Object::Array(list)),
            None => {
                let mut page = self.dict(page_id)?;
                page.set("Annots", Object::Array(list));
                self.put(page_id, Object::Dictionary(page));
            }
        }
        Ok(())
    }

    /// The object number of the page's `index`th annotation (PDFium's numbering), giving it one
    /// if it was written inline.
    pub fn annot_id(&mut self, page: usize, index: usize) -> Result<ObjectId, String> {
        let page_id = self.page_id(page)?;
        let (holder, mut list) = self.annots(page_id);
        match list.get(index).cloned() {
            Some(Object::Reference(id)) => Ok(id),
            Some(Object::Dictionary(d)) => {
                let id = self.add(Object::Dictionary(d));
                list[index] = Object::Reference(id);
                self.set_annots(page_id, holder, list)?;
                Ok(id)
            }
            _ => Err("The annotation was not found.".into()),
        }
    }

    /// Add an annotation to the page. Returns its object number and its index on the page.
    pub fn push_annot(&mut self, page: usize, mut d: Dictionary, ap: Option<Stream>) -> Result<(ObjectId, usize), String> {
        let page_id = self.page_id(page)?;
        d.set("Type", name("Annot"));
        d.set("P", Object::Reference(page_id));
        if !d.has(b"F") {
            d.set("F", Object::Integer(4));
        }
        d.set("M", text_string(&pdf_date()));
        if !d.has(b"NM") {
            d.set("NM", text_string(&format!("dockit-{}-{}", self.doc.max_id + 1, pdf_date().trim_start_matches("D:"))));
        }
        if let Some(s) = ap {
            let sid = self.add(Object::Stream(s));
            let mut apd = Dictionary::new();
            apd.set("N", Object::Reference(sid));
            d.set("AP", Object::Dictionary(apd));
        }
        let id = self.add(Object::Dictionary(d));
        let (holder, mut list) = self.annots(page_id);
        list.push(Object::Reference(id));
        let index = list.len() - 1;
        self.set_annots(page_id, holder, list)?;
        Ok((id, index))
    }

    /// Change an annotation with `f`, then redraw it if Dockit draws that kind.
    pub fn modify(&mut self, id: ObjectId, f: impl FnOnce(&mut Dictionary)) -> Result<(), String> {
        let mut d = self.dict(id)?;
        f(&mut d);
        d.set("M", text_string(&pdf_date()));
        if let Some(s) = build(&mut d) {
            let sid = self.add(Object::Stream(s));
            let mut apd = Dictionary::new();
            apd.set("N", Object::Reference(sid));
            d.set("AP", Object::Dictionary(apd));
        }
        self.put(id, Object::Dictionary(d));
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<u8>, String> {
        let objects: Vec<(ObjectId, Object)> = self.changed.iter().filter_map(|id| self.doc.objects.get(id).map(|o| (*id, o.clone()))).collect();
        incremental::append(&self.bytes, &self.doc, objects, self.doc.max_id + 1)
    }

    // ── the document's form ──

    fn root_id(&self) -> Result<ObjectId, String> {
        self.doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|_| "The document catalog is missing.".into())
    }

    /// The AcroForm dictionary and where it lives, creating one if the document has none.
    fn form(&mut self) -> Result<(ObjectId, Dictionary), String> {
        let root_id = self.root_id()?;
        let mut root = self.dict(root_id)?;
        let form = match root.get(b"AcroForm") {
            Ok(Object::Reference(id)) => return Ok((*id, self.dict(*id)?)),
            Ok(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        };
        // Give the form its own object so it can be changed on its own from now on.
        let id = self.add(Object::Dictionary(form.clone()));
        root.set("AcroForm", Object::Reference(id));
        self.put(root_id, Object::Dictionary(root));
        Ok((id, form))
    }

    /// Names of the form's fields (top level and one level down), to keep new names unique.
    pub fn field_names(&self) -> Vec<String> {
        let Ok(root) = self.root_id().and_then(|r| self.dict(r)) else { return Vec::new() };
        let form = match root.get(b"AcroForm") {
            Ok(Object::Reference(id)) => self.doc.get_dictionary(*id).ok().cloned(),
            Ok(Object::Dictionary(d)) => Some(d.clone()),
            _ => None,
        };
        let Some(fields) = form.and_then(|f| f.get(b"Fields").ok().cloned()) else { return Vec::new() };
        let fields = match fields {
            Object::Reference(id) => self.doc.get_object(id).and_then(Object::as_array).cloned().unwrap_or_default(),
            Object::Array(a) => a,
            _ => Vec::new(),
        };
        let mut names = Vec::new();
        for f in fields {
            let Ok(id) = f.as_reference() else { continue };
            let Ok(d) = self.doc.get_dictionary(id) else { continue };
            if let Ok(t) = d.get(b"T") {
                names.push(read_text(t));
            }
        }
        names
    }

    /// Add a new field (one widget) on `page`. Returns its index on the page.
    pub fn add_field(&mut self, page: usize, kind: NewField, rect: [f32; 4], field_name: &str) -> Result<usize, String> {
        let (form_id, mut form) = self.form()?;
        // Default appearance and fonts, so viewers can draw the typed-in values.
        if !form.has(b"DA") {
            form.set("DA", text_string("/Helv 0 Tf 0 g"));
        }
        let mut dr = match form.get(b"DR") {
            Ok(Object::Dictionary(d)) => d.clone(),
            Ok(Object::Reference(id)) => self.dict(*id).unwrap_or_default(),
            _ => Dictionary::new(),
        };
        let mut fonts = match dr.get(b"Font") {
            Ok(Object::Dictionary(d)) => d.clone(),
            Ok(Object::Reference(id)) => self.dict(*id).unwrap_or_default(),
            _ => Dictionary::new(),
        };
        if !fonts.has(b"Helv") {
            fonts.set("Helv", helvetica());
        }
        if !fonts.has(b"ZaDb") {
            fonts.set("ZaDb", zapf());
        }
        dr.set("Font", Object::Dictionary(fonts));
        form.set("DR", Object::Dictionary(dr));

        let [x0, y0, x1, y1] = rect;
        let (w, h) = (x1 - x0, y1 - y0);
        let mut d = Dictionary::new();
        d.set("Subtype", name("Widget"));
        d.set("T", text_string(field_name));
        d.set("Rect", nums(&rect));
        let mut mk = Dictionary::new();
        mk.set("BC", nums(&[0.45, 0.45, 0.5]));
        mk.set("BG", nums(&[1.0, 1.0, 1.0]));
        let frame = |fill: &str| format!("q {fill} rg 0.45 0.45 0.5 RG 1 w 0.5 0.5 {} {} re B Q\n", n(w - 1.0), n(h - 1.0));
        let local = |content: String, fonts: &[(&str, Object)]| appearance([0.0, 0.0, w, h], content, fonts);
        let mut ap = Dictionary::new();
        match kind {
            NewField::Text => {
                d.set("FT", name("Tx"));
                d.set("DA", text_string("/Helv 0 Tf 0 g"));
                d.set("V", text_string(""));
                ap.set("N", Object::Reference(self.add(Object::Stream(local(frame("1 1 1"), &[])))));
            }
            NewField::Checkbox => {
                d.set("FT", name("Btn"));
                d.set("DA", text_string("/ZaDb 0 Tf 0 g"));
                d.set("V", name("Off"));
                d.set("AS", name("Off"));
                mk.set("CA", text_string("4"));
                let size = h.min(w) * 0.8;
                let tick = format!(
                    "{}BT /ZaDb {} Tf 0 g {} {} Td (4) Tj ET\n",
                    frame("1 1 1"),
                    n(size),
                    n((w - size * 0.846) / 2.0),
                    n((h - size * 0.7) / 2.0)
                );
                let on = self.add(Object::Stream(local(tick, &[("ZaDb", zapf())])));
                let off = self.add(Object::Stream(local(frame("1 1 1"), &[])));
                let mut states = Dictionary::new();
                states.set("Yes", Object::Reference(on));
                states.set("Off", Object::Reference(off));
                ap.set("N", Object::Dictionary(states));
            }
            NewField::Dropdown => {
                d.set("FT", name("Ch"));
                d.set("Ff", Object::Integer(1 << 17));
                d.set("DA", text_string("/Helv 0 Tf 0 g"));
                d.set("Opt", Object::Array(vec![text_string("Option 1"), text_string("Option 2"), text_string("Option 3")]));
                d.set("V", text_string(""));
                // A small down arrow on the right, like a list box.
                let s = (h * 0.3).min(6.0);
                let (cx, cy) = (w - h / 2.0, h / 2.0);
                let arrow = format!(
                    "{}q 0.35 g {} {} m {} {} l {} {} l f Q\n",
                    frame("1 1 1"),
                    n(cx - s),
                    n(cy + s * 0.5),
                    n(cx + s),
                    n(cy + s * 0.5),
                    n(cx),
                    n(cy - s * 0.5)
                );
                ap.set("N", Object::Reference(self.add(Object::Stream(local(arrow, &[])))));
            }
            NewField::Signature => {
                d.set("FT", name("Sig"));
                let line = format!("{}q 0.6 G 0.75 w 6 {} m {} {} l S Q\n", frame("0.96 0.97 1"), n(h * 0.3), n(w - 6.0), n(h * 0.3));
                ap.set("N", Object::Reference(self.add(Object::Stream(local(line, &[])))));
            }
        }
        d.set("MK", Object::Dictionary(mk));
        d.set("AP", Object::Dictionary(ap));
        d.set("F", Object::Integer(4));
        let (id, index) = self.push_annot(page, d, None)?;

        let mut fields = match form.get(b"Fields") {
            Ok(Object::Array(a)) => a.clone(),
            Ok(Object::Reference(r)) => self.doc.get_object(*r).and_then(Object::as_array).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        fields.push(Object::Reference(id));
        form.set("Fields", Object::Array(fields));
        self.put(form_id, Object::Dictionary(form));
        Ok(index)
    }

    /// Draw every visible annotation and form field on `page` into its content (each one's own
    /// appearance, placed as the PDF specification lays out), then take them off the page. Links
    /// stay, as they are not marks. The existing content is kept byte for byte. Returns how many
    /// were drawn.
    pub fn flatten_page(&mut self, page: usize) -> Result<usize, String> {
        let page_id = self.page_id(page)?;
        let (holder, list) = self.annots(page_id);
        if list.is_empty() {
            return Ok(0);
        }
        let mut keep = Vec::new();
        let mut draws = String::new();
        let mut forms: Vec<(Vec<u8>, ObjectId)> = Vec::new();
        for entry in &list {
            let d = match entry {
                Object::Reference(id) => self.doc.get_dictionary(*id).ok().cloned(),
                Object::Dictionary(d) => Some(d.clone()),
                _ => None,
            };
            let Some(d) = d else { continue };
            let subtype = d.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"");
            if subtype == b"Link" {
                keep.push(entry.clone());
                continue;
            }
            let flags = d.get(b"F").and_then(Object::as_i64).unwrap_or(0);
            // Hidden (2) and no-view (32) annotations are not drawn, only removed.
            if flags & (2 | 32) != 0 || subtype == b"Popup" {
                continue;
            }
            let Some((sid, stream)) = self.normal_appearance(&d) else { continue };
            let Some(rect) = floats(&d, b"Rect").filter(|r| r.len() == 4) else { continue };
            let bbox = floats(&stream.dict, b"BBox").filter(|b| b.len() == 4).unwrap_or(vec![0.0, 0.0, 1.0, 1.0]);
            let m = floats(&stream.dict, b"Matrix").filter(|m| m.len() == 6).unwrap_or(vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            // The box the appearance covers after its own matrix, fitted onto the annotation's rect.
            let corners = [(bbox[0], bbox[1]), (bbox[2], bbox[1]), (bbox[0], bbox[3]), (bbox[2], bbox[3])]
                .map(|(x, y)| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]));
            let b = bounds(&corners, 0.0);
            let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
            if bw.abs() < 1e-6 || bh.abs() < 1e-6 {
                continue;
            }
            let (x0, y0, x1, y1) = (rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3]));
            let (sx, sy) = ((x1 - x0) / bw, (y1 - y0) / bh);
            // Appearance streams must be form XObjects to be drawn with Do; some omit /Subtype.
            let form_id = if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form") {
                sid
            } else {
                let mut s = stream.clone();
                s.dict.set("Type", name("XObject"));
                s.dict.set("Subtype", name("Form"));
                self.add(Object::Stream(s))
            };
            let res_name = format!("DkFlat{}", forms.len() + self.doc.max_id as usize).into_bytes();
            draws += &format!("q {} 0 0 {} {} {} cm /{} Do Q\n", n(sx), n(sy), n(x0 - b[0] * sx), n(y0 - b[1] * sy), String::from_utf8_lossy(&res_name));
            forms.push((res_name, form_id));
        }
        let drawn = forms.len();
        if drawn == 0 && keep.len() == list.len() {
            return Ok(0);
        }
        let mut page_dict = self.dict(page_id)?;
        if drawn > 0 {
            // The old content runs inside q ... Q so its graphics state cannot leak into the marks.
            let open = self.add(Object::Stream(Stream::new(Dictionary::new(), b"q\n".to_vec())));
            let mut tail = Stream::new(Dictionary::new(), format!("Q\n{draws}").into_bytes());
            let _ = tail.compress();
            let close = self.add(Object::Stream(tail));
            let mut contents = vec![Object::Reference(open)];
            match page_dict.get(b"Contents") {
                Ok(Object::Reference(id)) => match self.doc.get_object(*id) {
                    Ok(Object::Array(a)) => contents.extend(a.iter().cloned()),
                    _ => contents.push(Object::Reference(*id)),
                },
                Ok(Object::Array(a)) => contents.extend(a.iter().cloned()),
                _ => {}
            }
            contents.push(Object::Reference(close));
            page_dict.set("Contents", Object::Array(contents));
            let mut res = incremental::resources(&self.doc, page_id);
            let mut xo = incremental::sub_dict(&self.doc, &res, b"XObject");
            for (nm, id) in forms {
                xo.set(nm, Object::Reference(id));
            }
            res.set("XObject", Object::Dictionary(xo));
            page_dict.set("Resources", Object::Dictionary(res));
        }
        if keep.is_empty() {
            page_dict.remove(b"Annots");
        } else if holder.is_none() {
            page_dict.set("Annots", Object::Array(keep.clone()));
        }
        self.put(page_id, Object::Dictionary(page_dict));
        if let (Some(h), false) = (holder, keep.is_empty()) {
            self.put(h, Object::Array(keep));
        }
        Ok(drawn)
    }

    /// The annotation's normal appearance stream (for its current state, if it has several).
    fn normal_appearance(&self, d: &Dictionary) -> Option<(ObjectId, Stream)> {
        let ap = match d.get(b"AP").ok()? {
            Object::Reference(id) => self.doc.get_dictionary(*id).ok()?.clone(),
            Object::Dictionary(a) => a.clone(),
            _ => return None,
        };
        let states = match ap.get(b"N").ok()? {
            Object::Reference(id) => match self.doc.get_object(*id).ok()? {
                Object::Stream(s) => return Some((*id, s.clone())),
                Object::Dictionary(states) => states.clone(),
                _ => return None,
            },
            // Checkboxes and option buttons keep one appearance per state.
            Object::Dictionary(states) => states.clone(),
            _ => return None,
        };
        let state = d.get(b"AS").and_then(Object::as_name).ok()?;
        let sid = states.get(state).and_then(Object::as_reference).ok()?;
        let s = self.doc.get_object(sid).and_then(Object::as_stream).ok()?.clone();
        Some((sid, s))
    }

    /// Mark the form as holding signatures that must be kept intact (SigFlags 3).
    pub fn set_sig_flags(&mut self) -> Result<(), String> {
        let (id, mut form) = self.form()?;
        form.set("SigFlags", Object::Integer(3));
        self.put(id, Object::Dictionary(form));
        Ok(())
    }

    /// Take the form out of the document (after flattening, its fields no longer exist).
    pub fn remove_form(&mut self) -> Result<(), String> {
        let root_id = self.root_id()?;
        let mut root = self.dict(root_id)?;
        if root.remove(b"AcroForm").is_some() {
            self.put(root_id, Object::Dictionary(root));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum NewField {
    Text,
    Checkbox,
    Dropdown,
    Signature,
}

impl NewField {
    pub fn label(self) -> &'static str {
        match self {
            NewField::Text => "Text",
            NewField::Checkbox => "Checkbox",
            NewField::Dropdown => "Drop-down",
            NewField::Signature => "Signature",
        }
    }
}

// ── annotation dictionaries ──

/// A straight arrow from `a` to `b`.
pub fn arrow(a: (f32, f32), b: (f32, f32), color: Rgba, width: f32) -> Dictionary {
    let mut d = Dictionary::new();
    d.set("Subtype", name("Line"));
    d.set("L", nums(&[a.0, a.1, b.0, b.1]));
    d.set("LE", Object::Array(vec![name("None"), name("OpenArrow")]));
    d.set("C", color_array(color));
    d.set("IC", color_array(color));
    let mut bs = Dictionary::new();
    bs.set("W", real(width));
    d.set("BS", Object::Dictionary(bs));
    d.set("T", text_string(&author()));
    d.set("Subj", text_string("Arrow"));
    d.set("Rect", nums(&[a.0, a.1, b.0, b.1]));
    d
}

/// A measured distance from `a` to `b`, labelled `label`. `factor` turns points into the unit.
pub fn dimension(a: (f32, f32), b: (f32, f32), color: Rgba, width: f32, label: &str, unit: &str, factor: f32, ratio: &str) -> Dictionary {
    let mut d = arrow(a, b, color, width);
    d.set("LE", Object::Array(vec![name("OpenArrow"), name("OpenArrow")]));
    d.set("IT", name("LineDimension"));
    d.set("Cap", Object::Boolean(true));
    d.set("CP", name("Top"));
    d.set("Contents", text_string(label));
    d.set("Subj", text_string("Length Measurement"));
    let format = |u: &str, c: f32| {
        let mut f = Dictionary::new();
        f.set("Type", name("NumberFormat"));
        f.set("U", text_string(u));
        f.set("C", real(c));
        f.set("D", Object::Integer(100));
        Object::Array(vec![Object::Dictionary(f)])
    };
    let mut m = Dictionary::new();
    m.set("Type", name("Measure"));
    m.set("Subtype", name("RL"));
    m.set("R", text_string(ratio));
    m.set("X", format(unit, factor));
    m.set("D", format(unit, 1.0));
    m.set("A", format(&format!("sq {unit}"), 1.0));
    d.set("Measure", Object::Dictionary(m));
    d
}

/// A text box. `box_rect` is l, b, r, t; `callout` is the point it points at, if any.
pub fn text_box(box_rect: [f32; 4], text: &str, size: f32, text_color: Rgba, border: Rgba, callout: Option<(f32, f32)>) -> Dictionary {
    let mut d = Dictionary::new();
    d.set("Subtype", name("FreeText"));
    d.set("Rect", nums(&box_rect));
    d.set("Contents", text_string(text));
    d.set("DA", text_string(&format!("/Helv {} Tf {} rg", n(size), rgb(text_color))));
    d.set("C", color_array(border));
    let mut bs = Dictionary::new();
    bs.set("W", real(1.0));
    d.set("BS", Object::Dictionary(bs));
    d.set("T", text_string(&author()));
    if let Some(p) = callout {
        // From the point to the nearest side of the box.
        let (cx, cy) = ((box_rect[0] + box_rect[2]) / 2.0, (box_rect[1] + box_rect[3]) / 2.0);
        let end = if p.0 < box_rect[0] {
            (box_rect[0], cy)
        } else if p.0 > box_rect[2] {
            (box_rect[2], cy)
        } else if p.1 < box_rect[1] {
            (cx, box_rect[1])
        } else {
            (cx, box_rect[3])
        };
        d.set("IT", name("FreeTextCallout"));
        d.set("CL", nums(&[p.0, p.1, end.0, end.1]));
        d.set("LE", name("OpenArrow"));
        d.set("Subj", text_string("Callout"));
    } else {
        d.set("Subj", text_string("Text Box"));
    }
    d
}

/// A reply to the annotation `parent`, or (with `state`) a review state such as "Completed".
pub fn reply(parent: ObjectId, rect: [f32; 4], text: &str, state: Option<&str>) -> (Dictionary, Stream) {
    let mut d = Dictionary::new();
    d.set("Subtype", name("Text"));
    d.set("Rect", nums(&rect));
    d.set("IRT", Object::Reference(parent));
    d.set("T", text_string(&author()));
    d.set("Contents", text_string(text));
    d.set("Name", name("Comment"));
    match state {
        Some(s) => {
            d.set("StateModel", text_string("Review"));
            d.set("State", text_string(s));
            // Hidden, as review states are in other viewers.
            d.set("F", Object::Integer(4 | 2));
        }
        None => d.set("RT", name("R")),
    }
    // An empty appearance: replies live in the comment list, not as a second icon on the page.
    let ap = appearance([rect[0], rect[1], rect[0], rect[1]], String::new(), &[]);
    (d, ap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_keeps_breaks() {
        let lines = wrap("one two three four five six seven", 12.0, 60.0);
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|l| helv_width(l, 12.0) <= 60.0 || !l.contains(' ')));
        assert_eq!(wrap("a\nb", 12.0, 100.0), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn literal_escapes() {
        assert_eq!(pdf_literal("a(b)\\é"), "(a\\(b\\)\\\\\\351)");
    }

    /// Lists each page's annotations in the file named by DOCKIT_INSPECT (run with --ignored).
    #[test]
    #[ignore]
    fn inspect() {
        let Ok(path) = std::env::var("DOCKIT_INSPECT") else { return };
        let doc = lopdf::Document::load(&path).unwrap();
        for (n, id) in doc.get_pages() {
            let page = doc.get_dictionary(id).unwrap();
            let list = match page.get(b"Annots") {
                Ok(Object::Reference(r)) => doc.get_object(*r).unwrap().as_array().unwrap().clone(),
                Ok(Object::Array(a)) => a.clone(),
                _ => Vec::new(),
            };
            let kinds: Vec<String> = list
                .iter()
                .filter_map(|o| o.as_reference().ok().and_then(|r| doc.get_dictionary(r).ok()))
                .map(|d| String::from_utf8_lossy(d.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"?")).into_owned())
                .collect();
            println!("page {n}: {kinds:?}");
        }
        let root = doc.trailer.get(b"Root").and_then(Object::as_reference).unwrap();
        println!("AcroForm: {}", doc.get_dictionary(root).unwrap().has(b"AcroForm"));
    }

    #[test]
    fn da_parsing() {
        let (s, c) = parse_da("/Helv 14 Tf 1 0 0 rg");
        assert_eq!(s, 14.0);
        assert_eq!(c, [255, 0, 0, 255]);
    }
}
