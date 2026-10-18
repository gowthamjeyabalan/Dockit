//! Reading a page's content and resources, and writing changes as a PDF incremental update:
//! the original bytes stay as they are and the changed objects are appended after them.

use crate::content::{FontMeta, XKind};
use lopdf::{Dictionary, Object, ObjectId, Stream};
use std::collections::HashMap;
use std::io::Write;

pub struct PageData {
    pub doc: lopdf::Document,
    pub page_id: ObjectId,
    pub content: Vec<u8>,
    pub xobjects: HashMap<Vec<u8>, XKind>,
    pub fonts: HashMap<Vec<u8>, FontMeta>,
}

/// The effective resource dictionary of a page (its own, or the nearest inherited one).
fn resources(doc: &lopdf::Document, page_id: ObjectId) -> Dictionary {
    let Ok((direct, ids)) = doc.get_page_resources(page_id) else { return Dictionary::new() };
    if let Some(d) = direct {
        return d.clone();
    }
    ids.first().and_then(|id| doc.get_dictionary(*id).ok()).cloned().unwrap_or_default()
}

fn sub_dict(doc: &lopdf::Document, res: &Dictionary, key: &[u8]) -> Dictionary {
    match res.get(key) {
        Ok(Object::Dictionary(d)) => d.clone(),
        Ok(Object::Reference(id)) => doc.get_dictionary(*id).cloned().unwrap_or_default(),
        _ => Dictionary::new(),
    }
}

fn resolve<'a>(doc: &'a lopdf::Document, o: &'a Object) -> &'a Object {
    doc.dereference(o).map(|(_, o)| o).unwrap_or(o)
}

pub fn page_data(bytes: &[u8], page_index: usize) -> Result<PageData, String> {
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| format!("The file structure could not be read ({e})."))?;
    if doc.is_encrypted() {
        return Err("The file is encrypted.".into());
    }
    let page_id = *doc.get_pages().get(&(page_index as u32 + 1)).ok_or("The page was not found.")?;
    let mut content = Vec::new();
    for id in doc.get_page_contents(page_id) {
        let s = doc.get_object(id).and_then(Object::as_stream).map_err(|_| "A content stream is missing.")?;
        let data = if s.dict.get(b"Filter").is_ok() {
            s.decompressed_content().map_err(|_| "A content stream uses compression Dockit cannot read.")?
        } else {
            s.content.clone()
        };
        content.extend_from_slice(&data);
        content.push(b'\n');
    }
    let res = resources(&doc, page_id);
    let mut xobjects = HashMap::new();
    for (name, v) in sub_dict(&doc, &res, b"XObject").iter() {
        let kind = match resolve(&doc, v).as_stream().ok().and_then(|s| s.dict.get(b"Subtype").ok()).and_then(|s| s.as_name().ok()) {
            Some(b"Image") => XKind::Image,
            Some(b"Form") => XKind::Form,
            _ => XKind::Other,
        };
        xobjects.insert(name.clone(), kind);
    }
    let mut fonts = HashMap::new();
    for (name, v) in sub_dict(&doc, &res, b"Font").iter() {
        let Ok(d) = resolve(&doc, v).as_dict() else { continue };
        let subtype = d.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"");
        let base = d.get(b"BaseFont").and_then(Object::as_name).unwrap_or(b"");
        let enc = d.get(b"Encoding").ok().map(|e| resolve(&doc, e));
        let standard = [
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-Oblique",
            "Helvetica-BoldOblique",
            "Times-Roman",
            "Times-Bold",
            "Times-Italic",
            "Times-BoldItalic",
            "Courier",
            "Courier-Bold",
            "Courier-Oblique",
            "Courier-BoldOblique",
            "Arial",
            "Arial,Bold",
            "Arial,Italic",
            "Arial,BoldItalic",
        ]
        .iter()
        .any(|s| s.as_bytes() == base);
        let embedded = d
            .get(b"FontDescriptor")
            .ok()
            .map(|fd| resolve(&doc, fd))
            .and_then(|fd| fd.as_dict().ok())
            .is_some_and(|fd| fd.has(b"FontFile") || fd.has(b"FontFile2") || fd.has(b"FontFile3"));
        let winansi = matches!(enc, Some(Object::Name(n)) if n == b"WinAnsiEncoding");
        fonts.insert(name.clone(), FontMeta { two_byte: subtype == b"Type0", winansi: standard && winansi && !embedded });
    }
    Ok(PageData { doc, page_id, content, xobjects, fonts })
}

// ── writing ──

fn write_obj(out: &mut Vec<u8>, o: &Object) {
    match o {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => out.extend_from_slice(crate::content::num(*r as f64).as_bytes()),
        Object::Name(n) => write_name(out, n),
        Object::String(s, _) => {
            out.push(b'<');
            for b in s {
                out.extend_from_slice(format!("{b:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Object::Array(a) => {
            out.push(b'[');
            for (k, v) in a.iter().enumerate() {
                if k > 0 {
                    out.push(b' ');
                }
                write_obj(out, v);
            }
            out.push(b']');
        }
        Object::Dictionary(d) => write_dict(out, d),
        Object::Stream(s) => {
            let mut d = s.dict.clone();
            d.set("Length", Object::Integer(s.content.len() as i64));
            write_dict(out, &d);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&s.content);
            out.extend_from_slice(b"\nendstream");
        }
        Object::Reference((n, g)) => out.extend_from_slice(format!("{n} {g} R").as_bytes()),
    }
}

fn write_name(out: &mut Vec<u8>, n: &[u8]) {
    out.push(b'/');
    for &b in n {
        if b" \t\n\r\x0C()<>[]{}/%#".contains(&b) || !(33..=126).contains(&b) {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        } else {
            out.push(b);
        }
    }
}

fn write_dict(out: &mut Vec<u8>, d: &Dictionary) {
    out.extend_from_slice(b"<<");
    for (k, v) in d.iter() {
        write_name(out, k);
        out.push(b' ');
        write_obj(out, v);
        out.push(b' ');
    }
    out.extend_from_slice(b">>");
}

fn last_startxref(bytes: &[u8]) -> Option<usize> {
    let tail_start = bytes.len().saturating_sub(4096);
    let tail = &bytes[tail_start..];
    let at = tail.windows(9).rposition(|w| w == b"startxref")?;
    let rest = &tail[at + 9..];
    let digits: String = rest.iter().skip_while(|c| c.is_ascii_whitespace()).take_while(|c| c.is_ascii_digit()).map(|&c| c as char).collect();
    digits.parse().ok()
}

/// Append an update that gives the page `new_content`, plus any new image XObjects
/// (resource name → stream) added to its resources.
pub fn write_update(bytes: &[u8], data: &PageData, new_content: &[u8], new_xobjects: Vec<(Vec<u8>, Stream)>) -> Result<Vec<u8>, String> {
    let prev = last_startxref(bytes).ok_or("The file has no cross-reference table.")?;
    let doc = &data.doc;
    let mut next_id = doc.max_id + 1;
    let mut objects: Vec<(ObjectId, Object)> = Vec::new();

    let mut stream = Stream::new(Dictionary::new(), new_content.to_vec());
    let _ = stream.compress();
    let content_id = (next_id, 0);
    next_id += 1;
    objects.push((content_id, Object::Stream(stream)));

    let mut page = doc.get_dictionary(data.page_id).map_err(|_| "The page could not be read.")?.clone();
    page.set("Contents", Object::Reference(content_id));
    if !new_xobjects.is_empty() {
        let mut res = resources(doc, data.page_id);
        let mut xo = sub_dict(doc, &res, b"XObject");
        for (name, mut s) in new_xobjects {
            // Streams must be indirect objects, so a soft mask gets its own number.
            if let Ok(Object::Stream(mask)) = s.dict.get(b"SMask").cloned() {
                let mid = (next_id, 0);
                next_id += 1;
                objects.push((mid, Object::Stream(mask)));
                s.dict.set("SMask", Object::Reference(mid));
            }
            let id = (next_id, 0);
            next_id += 1;
            objects.push((id, Object::Stream(s)));
            xo.set(name, Object::Reference(id));
        }
        res.set("XObject", Object::Dictionary(xo));
        page.set("Resources", Object::Dictionary(res));
    }
    objects.push((data.page_id, Object::Dictionary(page)));

    let mut out = bytes.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let mut offsets = Vec::new();
    for (id, o) in &objects {
        offsets.push((*id, out.len()));
        let _ = writeln!(out, "{} {} obj", id.0, id.1);
        write_obj(&mut out, o);
        out.extend_from_slice(b"\nendobj\n");
    }
    offsets.sort_by_key(|(id, _)| id.0);
    let xref_at = out.len();
    out.extend_from_slice(b"xref\n");
    for ((n, g), off) in &offsets {
        let _ = write!(out, "{n} 1\n{off:010} {g:05} n\r\n");
    }
    let mut trailer = Dictionary::new();
    for (k, v) in doc.trailer.iter() {
        if !matches!(k.as_slice(), b"Prev" | b"XRefStm" | b"Size" | b"Type" | b"W" | b"Index" | b"Filter" | b"DecodeParms" | b"Length") {
            trailer.set(k.clone(), v.clone());
        }
    }
    trailer.set("Size", Object::Integer(next_id as i64));
    trailer.set("Prev", Object::Integer(prev as i64));
    out.extend_from_slice(b"trailer\n");
    write_dict(&mut out, &trailer);
    let _ = write!(out, "\nstartxref\n{xref_at}\n%%EOF\n");
    Ok(out)
}

/// An image XObject for a picture: JPEG data as is, other pixels compressed with a soft mask for
/// transparency.
pub fn image_xobject(jpeg: Option<&[u8]>, w: u32, h: u32, rgba: Option<&[u8]>) -> Option<Stream> {
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"XObject".to_vec()));
    d.set("Subtype", Object::Name(b"Image".to_vec()));
    d.set("Width", Object::Integer(w as i64));
    d.set("Height", Object::Integer(h as i64));
    d.set("BitsPerComponent", Object::Integer(8));
    if let Some(j) = jpeg {
        let channels = image::load_from_memory_with_format(j, image::ImageFormat::Jpeg).ok()?.color().channel_count();
        d.set("ColorSpace", Object::Name(if channels == 1 { b"DeviceGray".to_vec() } else { b"DeviceRGB".to_vec() }));
        d.set("Filter", Object::Name(b"DCTDecode".to_vec()));
        return Some(Stream::new(d, j.to_vec()));
    }
    let px = rgba?;
    d.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    let rgb: Vec<u8> = px.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut s = Stream::new(d, rgb);
    let _ = s.compress();
    if px.as_chunks::<4>().0.iter().any(|p| p[3] != 255) {
        let mut md = Dictionary::new();
        md.set("Type", Object::Name(b"XObject".to_vec()));
        md.set("Subtype", Object::Name(b"Image".to_vec()));
        md.set("Width", Object::Integer(w as i64));
        md.set("Height", Object::Integer(h as i64));
        md.set("BitsPerComponent", Object::Integer(8));
        md.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
        let mut mask = Stream::new(md, px.as_chunks::<4>().0.iter().map(|p| p[3]).collect());
        let _ = mask.compress();
        // Held here for now; write_update gives it its own object number.
        s.dict.set("SMask", Object::Stream(mask));
    }
    Some(s)
}
