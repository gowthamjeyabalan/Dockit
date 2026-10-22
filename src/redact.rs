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
    /// Redaction marks on the pages read so far: cheap enough for every frame (marks are made on
    /// pages that are showing, so these are the ones that matter while marking).
    pub fn redaction_marks_seen(&self) -> usize {
        let Some(f) = &self.file else { return 0 };
        (0..f.page_count()).filter_map(|p| f.annots_if_read(p)).map(|a| a.iter().filter(|a| a.subtype == pdf::ANNOT_REDACT).count()).sum()
    }

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

// ── finding what to redact ──

/// Kinds of information that can be found and marked across a document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    Words,
    Emails,
    Phones,
    Cards,
    Ids,
    Dates,
    Custom,
}

impl Pattern {
    pub const ALL: [Pattern; 7] = [Pattern::Words, Pattern::Emails, Pattern::Phones, Pattern::Cards, Pattern::Ids, Pattern::Dates, Pattern::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Pattern::Words => "Words or phrases",
            Pattern::Emails => "Email addresses",
            Pattern::Phones => "Phone numbers",
            Pattern::Cards => "Payment card numbers",
            Pattern::Ids => "ID numbers (like 123-45-6789)",
            Pattern::Dates => "Dates",
            Pattern::Custom => "My own pattern",
        }
    }

    fn regex(self) -> &'static str {
        match self {
            Pattern::Emails => r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
            Pattern::Phones => r"\+?\(?\d[\d ().-]{6,}\d",
            Pattern::Cards => r"\b(?:\d[ -]?){12,18}\d\b",
            Pattern::Ids => r"\b\d{3}-\d{2}-\d{4}\b",
            Pattern::Dates => r"\b(?:\d{1,2}[/.-]\d{1,2}[/.-]\d{2,4}|\d{4}-\d{2}-\d{2}|\d{1,2} (?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]* \d{4})\b",
            Pattern::Words | Pattern::Custom => "",
        }
    }
}

/// The Luhn check digit test, so only real-looking card numbers are taken.
fn luhn(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits.iter().rev().enumerate().map(|(k, &d)| if k % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { d }).sum();
    sum % 10 == 0
}

/// The expression to search with, or why it cannot be built.
fn search_regex(pattern: Pattern, text: &str, match_case: bool) -> Result<regex::Regex, String> {
    let source = match pattern {
        Pattern::Words => {
            let words: Vec<String> = text.lines().map(str::trim).filter(|w| !w.is_empty()).map(regex::escape).collect();
            if words.is_empty() {
                return Err("Type the words to find, one per line.".into());
            }
            format!("(?:{})", words.join("|"))
        }
        Pattern::Custom => {
            if text.trim().is_empty() {
                return Err("Type a pattern (a regular expression).".into());
            }
            text.trim().to_string()
        }
        p => p.regex().to_string(),
    };
    regex::RegexBuilder::new(&source).case_insensitive(!match_case).build().map_err(|e| format!("That pattern does not work: {e}"))
}

impl Editor {
    /// Every place on every page that matches: (page, the boxes it covers).
    pub fn find_for_redaction(&self, pattern: Pattern, text: &str, match_case: bool) -> Result<Vec<(usize, Vec<[f32; 4]>)>, String> {
        let re = search_regex(pattern, text, match_case)?;
        let f = self.file.as_ref().ok_or("No document is open.")?;
        let mut out = Vec::new();
        for page in 0..f.page_count() {
            let Some(p) = f.doc.page(page) else { continue };
            let (s, at) = p.chars_text();
            for m in re.find_iter(&s) {
                if m.as_str().trim().is_empty() || (pattern == Pattern::Cards && !luhn(m.as_str())) {
                    continue;
                }
                let (a, b) = (at[m.start()], at[m.end() - 1]);
                let rects = p.text_rects(a, b - a + 1);
                if !rects.is_empty() {
                    out.push((page, rects));
                }
            }
        }
        Ok(out)
    }

    /// Mark many areas for redaction at once.
    pub fn add_redactions(&mut self, areas: &[(usize, [f32; 4])]) -> usize {
        let mut pages: Vec<usize> = areas.iter().map(|a| a.0).collect();
        pages.sort_unstable();
        pages.dedup();
        self.file_update(&pages, |u| {
            for &(page, r) in areas {
                // A little room around the letters, so nothing peeks out.
                let (d, ap) = mark([r[0] - 1.0, r[1] - 1.0, r[2] + 1.0, r[3] + 1.0]);
                u.push_annot(page, d, Some(ap))?;
            }
            Ok(areas.len())
        })
        .unwrap_or(0)
    }
}

pub struct FindRedactDialog {
    pattern: Pattern,
    text: String,
    match_case: bool,
    found: Option<Vec<(usize, Vec<[f32; 4]>)>>,
    error: Option<String>,
}

impl FindRedactDialog {
    pub fn new() -> Self {
        FindRedactDialog { pattern: Pattern::Words, text: String::new(), match_case: false, found: None, error: None }
    }
}

/// Returns false when it closes.
pub fn find_redact_window(ctx: &egui::Context, ed: &mut Editor, d: &mut FindRedactDialog) -> bool {
    use crate::dialogs::{modal, primary, secondary_button, title};
    use crate::theme::{self, pal};
    let p = pal(ctx);
    let mut open = true;
    let mut search = false;
    let mut mark_all = false;
    let (_, close) = modal(ctx, "find-redact", 440.0, |ui| {
        title(ui, "Find and redact");
        ui.label(theme::secondary(ui, "Finds words or kinds of information on every page and marks them for redaction. Check the marks, then apply them."));
        theme::heading(ui, "FIND");
        let before = (d.pattern, d.text.clone(), d.match_case);
        egui::ComboBox::from_id_salt("redact-pattern").width(ui.available_width() - 8.0).selected_text(d.pattern.name()).show_ui(ui, |ui| {
            for pt in Pattern::ALL {
                ui.selectable_value(&mut d.pattern, pt, pt.name());
            }
        });
        match d.pattern {
            Pattern::Words => {
                ui.add(egui::TextEdit::multiline(&mut d.text).desired_rows(3).desired_width(f32::INFINITY).hint_text("One word or phrase per line"));
                ui.checkbox(&mut d.match_case, "Match case");
            }
            Pattern::Custom => {
                ui.add(egui::TextEdit::singleline(&mut d.text).desired_width(f32::INFINITY).hint_text(r"A regular expression, such as INV-\d{6}"));
                ui.checkbox(&mut d.match_case, "Match case");
            }
            _ => {}
        }
        if before != (d.pattern, d.text.clone(), d.match_case) {
            d.found = None;
        }
        if let Some(found) = &d.found {
            let pages = {
                let mut v: Vec<usize> = found.iter().map(|f| f.0).collect();
                v.dedup();
                v.len()
            };
            ui.add_space(4.0);
            ui.label(if found.is_empty() { "Nothing found.".to_string() } else { format!("Found {} on {}.", plural(found.len(), "match", "matches"), plural(pages, "page", "pages")) });
        }
        if let Some(e) = &d.error {
            ui.label(egui::RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match &d.found {
                Some(f) if !f.is_empty() => {
                    if primary(ui, "Mark for redaction", true) {
                        mark_all = true;
                    }
                }
                _ => {
                    if primary(ui, "Find", true) {
                        search = true;
                    }
                }
            }
            if secondary_button(ui, "Cancel") {
                open = false;
            }
        });
    });
    if close {
        open = false;
    }
    if search {
        match ed.find_for_redaction(d.pattern, &d.text, d.match_case) {
            Ok(found) => {
                d.error = None;
                d.found = Some(found);
            }
            Err(e) => d.error = Some(e),
        }
    }
    if mark_all && let Some(found) = d.found.take() {
        let areas: Vec<(usize, [f32; 4])> = found.iter().flat_map(|(page, rs)| rs.iter().map(|r| (*page, *r))).collect();
        let n = ed.add_redactions(&areas);
        if n > 0 {
            ed.tool = crate::editor::Tool::Redact;
            ed.say(format!("Marked {} for redaction. Check them, then use Apply redactions.", plural(found.len(), "match", "matches")));
            if let Some(&(page, _)) = found.first() {
                ed.goto = Some(page);
            }
        }
        open = false;
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_find_what_they_should() {
        let find = |p: Pattern, s: &str| search_regex(p, "", false).unwrap().find(s).map(|m| m.as_str().to_string());
        assert_eq!(find(Pattern::Emails, "write to ana.li@example.co.uk today").as_deref(), Some("ana.li@example.co.uk"));
        assert_eq!(find(Pattern::Ids, "SSN 123-45-6789.").as_deref(), Some("123-45-6789"));
        assert_eq!(find(Pattern::Dates, "due 2026-10-22 or 3 March 2027").as_deref(), Some("2026-10-22"));
        assert!(luhn("4111 1111 1111 1111"));
        assert!(!luhn("4111 1111 1111 1112"));
        let words = search_regex(Pattern::Words, "secret\nAcme Corp", false).unwrap();
        assert_eq!(words.find_iter("The ACME corp secret").count(), 2);
    }

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
