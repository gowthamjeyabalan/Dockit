//! True redaction: marked areas are removed from the file, not just covered.
//!
//! Marking adds Redact annotations (shown as red outlines, still reversible). Applying them, page
//! by page: groups under a mark are opened up; each text object loses the characters under a mark
//! (the rest is rewritten in its own font at the same places); image pixels under a mark are
//! painted black in a new copy of the image; shapes wholly inside a mark are deleted; comments and
//! fields that touch a mark are removed; and a black box is drawn over each area. Finally the whole
//! file is rewritten without the objects nothing uses any more, so the old content and images do
//! not survive in earlier revisions of the file.

use crate::annots;
use crate::content::{Edit, Piece};
use crate::editor::{Editor, Sel, plural};
use crate::incremental;
use crate::pdf::{self, Doc, Kind, PathStyle, Seg};
use lopdf::{Dictionary, Object};

fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn inside(p: (f32, f32), r: [f32; 4]) -> bool {
    p.0 >= r[0] && p.0 <= r[2] && p.1 >= r[1] && p.1 <= r[3]
}

fn quad_box(q: &[(f32, f32); 4]) -> [f32; 4] {
    let xs = q.iter().map(|p| p.0);
    let ys = q.iter().map(|p| p.1);
    [xs.clone().fold(f32::MAX, f32::min), ys.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max), ys.fold(f32::MIN, f32::max)]
}

/// A Redact annotation over `rect`: outlined in red until it is applied.
pub fn mark(rect: [f32; 4]) -> (Dictionary, lopdf::Stream) {
    let mut d = Dictionary::new();
    d.set("Subtype", Object::Name(b"Redact".to_vec()));
    d.set("Rect", Object::Array(rect.iter().map(|&v| Object::Real(v)).collect()));
    let [x0, y0, x1, y1] = rect;
    d.set("QuadPoints", Object::Array([x0, y1, x1, y1, x0, y0, x1, y0].iter().map(|&v| Object::Real(v)).collect()));
    d.set("IC", Object::Array(vec![Object::Real(0.0), Object::Real(0.0), Object::Real(0.0)]));
    d.set("C", Object::Array(vec![Object::Real(0.88), Object::Real(0.2), Object::Real(0.2)]));
    d.set("T", annots::text_string(&annots::author()));
    let content = format!(
        "q 0.88 0.2 0.2 RG 0.88 0.2 0.2 rg 1.5 w [4 2] 0 d {} {} {} {} re S Q",
        x0 + 0.75,
        y0 + 0.75,
        (x1 - x0 - 1.5).max(0.0),
        (y1 - y0 - 1.5).max(0.0)
    );
    let mut sd = Dictionary::new();
    sd.set("Type", Object::Name(b"XObject".to_vec()));
    sd.set("Subtype", Object::Name(b"Form".to_vec()));
    sd.set("BBox", Object::Array(rect.iter().map(|&v| Object::Real(v)).collect()));
    (d, lopdf::Stream::new(sd, content.into_bytes()))
}

#[derive(Default)]
struct Report {
    chars: usize,
    images: usize,
    shapes: usize,
    annots: usize,
    /// Text objects removed whole because the rest of their text could not be rewritten.
    whole: usize,
}

/// Whether a mark covers enough of a glyph (l, b, r, t) to take it: a fifth of its width, so a
/// mark that only grazes a neighbour leaves it alone.
fn covers(r: [f32; 4], g: [f32; 4]) -> bool {
    if !overlaps(r, g) {
        return false;
    }
    let w = (g[2] - g[0]).max(1e-3);
    (r[2].min(g[2]) - r[0].max(g[0])) / w >= 0.2
}

impl Editor {
    pub fn add_redaction(&mut self, page: usize, rect: [f32; 4]) {
        let (d, ap) = mark(rect);
        if let Some(i) = self.file_update(&[page], |u| u.push_annot(page, d, Some(ap)).map(|(_, i)| i)) {
            self.sel = Sel::Annot { page, index: i };
        }
    }

    /// Redaction marks on each page, as (page, rect, annotation index).
    pub fn redaction_marks(&self) -> Vec<(usize, [f32; 4], usize)> {
        let Some(f) = &self.file else { return Vec::new() };
        (0..f.page_count()).flat_map(|p| f.annots(p).iter().filter(|a| a.subtype == pdf::ANNOT_REDACT).map(|a| (p, a.rect, a.index)).collect::<Vec<_>>()).collect()
    }

    /// Permanently remove everything under the redaction marks.
    pub fn apply_redactions(&mut self) {
        self.commit_edits();
        self.clear_selection();
        let marks = self.redaction_marks();
        if marks.is_empty() {
            self.say("There are no redaction marks. Use the Redact tool to mark areas first.");
            return;
        }
        // The final clean-up needs to read the whole file: check that first, before changing anything.
        if let Some(f) = &self.file {
            match f.doc.save_plain().and_then(|b| lopdf::Document::load_mem(&b).map_err(|e| e.to_string())) {
                Ok(_) => {}
                Err(e) => {
                    self.error = Some(format!("Redaction needs to rewrite the whole file, and this file could not be read for that ({e}). Nothing was changed."));
                    return;
                }
            }
        }
        let mut pages: Vec<usize> = marks.iter().map(|m| m.0).collect();
        pages.dedup();
        let mut report = Report::default();
        for page in pages {
            let rects: Vec<[f32; 4]> = marks.iter().filter(|m| m.0 == page).map(|m| m.1).collect();
            if let Err(e) = self.redact_page(page, &rects, &mut report) {
                self.error = Some(format!("Page {} could not be redacted completely: {e}", page + 1));
                return;
            }
        }
        if let Err(e) = self.scrub() {
            self.error = Some(format!("The redacted content was removed from the pages, but the file could not be cleaned of older copies ({e}). Do not share it yet."));
            return;
        }
        let mut parts = Vec::new();
        if report.chars > 0 {
            parts.push(plural(report.chars, "character", "characters"));
        }
        if report.images > 0 {
            parts.push(plural(report.images, "image", "images"));
        }
        if report.shapes > 0 {
            parts.push(plural(report.shapes, "shape", "shapes"));
        }
        if report.annots > 0 {
            parts.push(plural(report.annots, "comment or field", "comments or fields"));
        }
        let what = if parts.is_empty() { "nothing under the marks".to_string() } else { parts.join(", ") };
        let extra = if report.whole > 0 { format!(" {} also removed whole, as the rest of their text could not be kept.", plural(report.whole, "line was", "lines were")) } else { String::new() };
        self.say(format!("Redacted {}: removed {what}.{extra}", plural(marks.len(), "area", "areas")));
    }

    fn redact_page(&mut self, page: usize, rects: &[[f32; 4]], report: &mut Report) -> Result<(), String> {
        let hit = |b: [f32; 4]| rects.iter().any(|r| overlaps(*r, b));

        // Open up groups under a mark so their parts can be dealt with one by one.
        for _ in 0..16 {
            let Some(f) = &self.file else { return Ok(()) };
            let group = f.objects(page).iter().find(|o| o.kind == Kind::Group && hit(quad_box(&o.quad))).map(|o| o.index);
            let Some(g) = group else { break };
            if self.ungroup(page, g).is_none() {
                // It could not be opened: remove it whole rather than leave it there.
                self.error = None;
                if !self.surgery(page, vec![(g, Edit::Delete)], Vec::new()) {
                    return Err("a group of objects could not be removed".into());
                }
                report.shapes += 1;
            }
        }

        let f = self.file.as_ref().ok_or("no document")?;
        let p = f.doc.page(page).ok_or("the page could not be loaded")?;
        let objs = f.objects(page);
        let chars = p.chars();
        let model = crate::editor::PageModel::load(f, page)?;
        let mut edits: Vec<(usize, Edit)> = Vec::new();
        let mut images: Vec<(Vec<u8>, lopdf::Stream)> = Vec::new();
        let mut taken: std::collections::HashSet<Vec<u8>> = model.data.xobjects.keys().cloned().collect();

        for o in objs.iter() {
            let bounds = quad_box(&o.quad);
            if !hit(bounds) {
                continue;
            }
            match o.kind {
                Kind::Text => {
                    let mine: Vec<&pdf::Char> = chars.iter().filter(|c| c.obj == o.index).collect();
                    let taken = |c: &pdf::Char| rects.iter().any(|r| covers(*r, c.rect));
                    let gone = mine.iter().filter(|c| taken(c)).count();
                    if gone == 0 && !mine.is_empty() {
                        continue;
                    }
                    report.chars += gone.max(1);
                    // The characters that stay, in runs, each run starting where its first character was.
                    let m = p.matrix(o.index);
                    let mut runs: Vec<((f32, f32), String)> = Vec::new();
                    let mut open = false;
                    for c in &mine {
                        if taken(c) {
                            open = false;
                            continue;
                        }
                        if !open {
                            // A run starts at its first real character.
                            if c.ch.is_whitespace() {
                                continue;
                            }
                            runs.push((c.origin, String::new()));
                            open = true;
                        }
                        runs.last_mut().unwrap().1.push(c.ch);
                    }
                    runs.retain(|r| !r.1.trim().is_empty());
                    let mut lines: Vec<([f64; 6], Vec<Piece>)> = Vec::new();
                    let mut ok = true;
                    for (origin, text) in &runs {
                        let text = text.trim_end();
                        match model.encode(f, page, o.index, text) {
                            Some(pieces) => lines.push(([m[0] as f64, m[1] as f64, m[2] as f64, m[3] as f64, origin.0 as f64, origin.1 as f64], pieces)),
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if lines.is_empty() || !ok {
                        if !runs.is_empty() {
                            report.whole += 1;
                        }
                        edits.push((o.index, Edit::Delete));
                    } else {
                        edits.push((o.index, Edit::Lines(lines)));
                    }
                }
                Kind::Image => {
                    let Some((w, h, mut px)) = p.image_rgba(o.index) else {
                        edits.push((o.index, Edit::Delete));
                        report.images += 1;
                        continue;
                    };
                    let m = p.matrix(o.index);
                    blacken(&mut px, w, h, m, rects);
                    let Some(stream) = incremental::image_xobject(None, w, h, Some(&px)) else { return Err("an image could not be rewritten".into()) };
                    let name = (1..).map(|n| format!("DkRedact{n}").into_bytes()).find(|n| !taken.contains(n)).unwrap();
                    taken.insert(name.clone());
                    let is_xobject = model.parsed.objs.get(o.index).is_some_and(|po| model.parsed.ops[po.first].name == b"Do");
                    if is_xobject {
                        edits.push((o.index, Edit::XObject(name.clone())));
                    } else {
                        // A picture written inline in the page becomes a separate image drawn the same way.
                        edits.push((o.index, Edit::Inline(format!("/{} Do", String::from_utf8_lossy(&name)).into_bytes())));
                    }
                    images.push((name, stream));
                    report.images += 1;
                }
                Kind::Path | Kind::Shading => {
                    if rects.iter().any(|r| o.quad.iter().all(|&c| inside(c, *r))) {
                        edits.push((o.index, Edit::Delete));
                        report.shapes += 1;
                    }
                }
                Kind::Group => {}
            }
        }
        drop(p);
        drop(model);
        if !edits.is_empty() && !self.surgery(page, edits, images) {
            return Err(self.error.take().unwrap_or_else(|| "the page could not be rewritten".into()));
        }

        // Comments, marks and fields touching an area go, and so do the marks themselves.
        let annots = self.file.as_ref().map(|f| f.annots(page)).unwrap_or_default();
        let mut gone: Vec<usize> = annots
            .iter()
            .filter(|a| a.subtype == pdf::ANNOT_REDACT || (a.subtype != pdf::ANNOT_POPUP && hit(a.rect)))
            .map(|a| a.index)
            .collect();
        // Replies to anything removed go too.
        let parents = gone.clone();
        gone.extend(annots.iter().filter(|a| a.reply_to.is_some_and(|r| parents.contains(&r))).map(|a| a.index));
        gone.sort_unstable_by(|a, b| b.cmp(a));
        gone.dedup();
        report.annots += annots.iter().filter(|a| gone.contains(&a.index) && a.subtype != pdf::ANNOT_REDACT).count();
        self.apply(page, |p| {
            for i in gone {
                p.remove_annot(i);
            }
        });

        // A black box over each area.
        for r in rects {
            let segs = vec![Seg::Move(r[0], r[1]), Seg::Line(r[2], r[1]), Seg::Line(r[2], r[3]), Seg::Line(r[0], r[3]), Seg::Close];
            self.apply(page, |p| p.add_path(&segs, PathStyle { fill: Some([0, 0, 0, 255]), stroke: None, width: 0.0 }));
        }
        Ok(())
    }

    /// Rewrite the whole file keeping only what is in use, so nothing redacted survives in it.
    fn scrub(&mut self) -> Result<(), String> {
        self.commit();
        let f = self.file.as_ref().ok_or("no document")?;
        let bytes = f.doc.save_plain()?;
        let mut doc = lopdf::Document::load_mem(&bytes).map_err(|e| e.to_string())?;
        doc.prune_objects();
        let mut out = Vec::new();
        doc.save_to(&mut out).map_err(|e| e.to_string())?;
        let fresh = Doc::open(out, None).map_err(|_| "the cleaned file could not be read back".to_string())?;
        let f = self.file.as_mut().unwrap();
        f.replace_doc(fresh);
        self.epoch += 1;
        self.needs_commit = true;
        Ok(())
    }
}

/// Paint the pixels of an image (drawn with matrix `m`, which maps the unit square onto the page)
/// that fall inside any of `rects` black.
fn blacken(px: &mut [u8], w: u32, h: u32, m: [f32; 6], rects: &[[f32; 4]]) {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-9 {
        return;
    }
    // Page point to image unit square.
    let to_unit = |x: f32, y: f32| {
        let (dx, dy) = (x - m[4], y - m[5]);
        ((m[3] * dx - m[2] * dy) / det, (-m[1] * dx + m[0] * dy) / det)
    };
    for r in rects {
        // Pixel range covering the rect, then an exact test per pixel (rotated images included).
        let corners = [to_unit(r[0], r[1]), to_unit(r[2], r[1]), to_unit(r[0], r[3]), to_unit(r[2], r[3])];
        let (u0, u1) = (corners.iter().map(|c| c.0).fold(f32::MAX, f32::min), corners.iter().map(|c| c.0).fold(f32::MIN, f32::max));
        let (v0, v1) = (corners.iter().map(|c| c.1).fold(f32::MAX, f32::min), corners.iter().map(|c| c.1).fold(f32::MIN, f32::max));
        let x0 = ((u0 * w as f32).floor().max(0.0)) as u32;
        let x1 = ((u1 * w as f32).ceil().min(w as f32)) as u32;
        // Row 0 is the top of the image, at v = 1.
        let y0 = (((1.0 - v1) * h as f32).floor().max(0.0)) as u32;
        let y1 = (((1.0 - v0) * h as f32).ceil().min(h as f32)) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let (u, v) = ((x as f32 + 0.5) / w as f32, 1.0 - (y as f32 + 0.5) / h as f32);
                let page = (m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5]);
                // Pixels touching the edge count too.
                let pad = (m[0].abs() + m[2].abs()).max(m[1].abs() + m[3].abs()) / w.min(h).max(1) as f32 / 2.0;
                if page.0 >= r[0] - pad && page.0 <= r[2] + pad && page.1 >= r[1] - pad && page.1 <= r[3] + pad {
                    let k = ((y * w + x) * 4) as usize;
                    px[k..k + 4].copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reports whether DOCKIT_WORD appears anywhere in DOCKIT_INSPECT, in any stream or string,
    /// decompressed (run with --ignored).
    #[test]
    #[ignore]
    fn find_word() {
        let (Ok(path), Ok(word)) = (std::env::var("DOCKIT_INSPECT"), std::env::var("DOCKIT_WORD")) else { return };
        let doc = lopdf::Document::load(&path).unwrap();
        let w = word.as_bytes();
        let mut found = Vec::new();
        for (id, o) in &doc.objects {
            let data = match o {
                Object::Stream(s) => s.decompressed_content().unwrap_or_else(|_| s.content.clone()),
                other => format!("{other:?}").into_bytes(),
            };
            if data.windows(w.len()).any(|x| x == w) {
                found.push(*id);
            }
        }
        let text = doc.extract_text(&[1]).unwrap_or_default();
        println!("objects containing {word:?}: {found:?}");
        println!("page 1 text contains it {} times", text.matches(&word).count());
        println!("raw file contains it: {}", std::fs::read(&path).unwrap().windows(w.len()).any(|x| x == w));
    }

    /// Needs pdfium.dll in target/debug (run with --ignored).
    #[test]
    #[ignore]
    fn redacts_image_pixels() {
        pdf::init(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug").as_path()).unwrap();
        let doc = Doc::new_empty();
        assert!(doc.insert_blank(0, 200.0, 200.0));
        let px = vec![200u8; 20 * 20 * 4];
        doc.page(0).unwrap().add_image(&pdf::ImageData::Rgba { w: 20, h: 20, px: &px }, [0.0, 0.0, 200.0, 200.0]).unwrap();
        let mut ed = Editor::default();
        ed.open_untitled(doc);
        ed.add_redaction(0, [0.0, 0.0, 100.0, 200.0]);
        ed.apply_redactions();
        assert!(ed.error.is_none(), "{:?}", ed.error);
        let f = ed.file.as_ref().unwrap();
        let p = f.doc.page(0).unwrap();
        let img = f.objects(0).iter().find(|o| o.kind == Kind::Image).map(|o| o.index).unwrap();
        let (w, _, px) = p.image_rgba(img).unwrap();
        let at = |x: u32, y: u32| px[((y * w + x) * 4) as usize];
        assert_eq!(at(2, 10), 0, "left half is black");
        assert_eq!(at(17, 10), 200, "right half is untouched");
        assert!(f.annots(0).is_empty(), "the mark is gone");
    }

    #[test]
    fn blackens_only_inside() {
        // A 10 x 10 image drawn over (0,0)-(100,100); redact the left half.
        let mut px = vec![255u8; 10 * 10 * 4];
        blacken(&mut px, 10, 10, [100.0, 0.0, 0.0, 100.0, 0.0, 0.0], &[[0.0, 0.0, 50.0, 100.0]]);
        let black = |x: usize, y: usize| px[(y * 10 + x) * 4] == 0;
        assert!(black(0, 0) && black(4, 9));
        assert!(!black(6, 0) && !black(9, 5));
    }
}
