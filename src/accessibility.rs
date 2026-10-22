//! Accessibility: a check against what screen readers and the PDF/UA standard need, with fixes;
//! adding tags (the structure a screen reader follows) to an untagged document; and alternate text
//! for figures.
//!
//! Tagging wraps each piece of page content in marked-content tags (content.rs's Mark edit, so the
//! drawing commands themselves are untouched) and builds the structure tree from them: paragraphs,
//! headings (by text size), figures, and lines or backgrounds marked as decoration (artifacts).

use crate::annots::{Update, text_string};
use crate::content::Edit;
use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::{Editor, plural};
use crate::pdf::Kind;
use crate::theme::{self, pal};
use egui::RichText;
use lopdf::{Dictionary, Object, ObjectId};

/// Windows' language for the user, such as "en-GB".
pub fn user_language() -> String {
    #[cfg(windows)]
    unsafe {
        let mut buf = [0u16; 85];
        let n = windows_sys::Win32::Globalization::GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32);
        if n > 1 {
            return String::from_utf16_lossy(&buf[..n as usize - 1]);
        }
    }
    "en-US".into()
}

fn resolve(doc: &lopdf::Document, o: &Object) -> Option<Dictionary> {
    match o {
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        Object::Dictionary(d) => Some(d.clone()),
        _ => None,
    }
}

/// Every font the document uses: its name and whether its glyphs are inside the file.
pub fn fonts(doc: &lopdf::Document) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for o in doc.objects.values() {
        let Ok(d) = o.as_dict() else { continue };
        if d.get(b"Type").and_then(Object::as_name).ok() != Some(b"Font") {
            continue;
        }
        let sub = d.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"");
        if matches!(sub, b"Type0" | b"Type3") {
            // Type 0 fonts are checked through their descendant; Type 3 glyphs are drawings.
            continue;
        }
        let name = String::from_utf8_lossy(d.get(b"BaseFont").and_then(Object::as_name).unwrap_or(b"?")).into_owned();
        let embedded = d
            .get(b"FontDescriptor")
            .ok()
            .and_then(|fd| resolve(doc, fd))
            .is_some_and(|fd| fd.has(b"FontFile") || fd.has(b"FontFile2") || fd.has(b"FontFile3"));
        if !out.iter().any(|f| f.0 == name) {
            out.push((name, embedded));
        }
    }
    out
}

// ── the check ──

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fix {
    Tag,
    Title,
    Language,
    DisplayTitle,
    Ocr,
    AltText,
    FieldNames,
    TabOrder,
}

pub struct Item {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    pub fix: Option<Fix>,
}

impl Editor {
    pub fn accessibility_check(&self) -> Result<Vec<Item>, String> {
        let f = self.file.as_ref().ok_or("No document is open.")?;
        let doc = lopdf::Document::load_mem(&f.doc.save_plain()?).map_err(|e| format!("The file could not be read for checking ({e})."))?;
        let root = doc.trailer.get(b"Root").ok().and_then(|o| resolve(&doc, o)).ok_or("The document catalog is missing.")?;
        let mut items = Vec::new();
        let tagged = root.has(b"StructTreeRoot") && root.get(b"MarkInfo").ok().and_then(|m| resolve(&doc, m)).is_some_and(|m| matches!(m.get(b"Marked"), Ok(Object::Boolean(true))));
        items.push(Item {
            name: "Tags",
            ok: tagged,
            detail: if tagged { "The document has tags for screen readers to follow.".into() } else { "Without tags, screen readers guess the reading order.".into() },
            fix: (!tagged).then_some(Fix::Tag),
        });
        let has_title = !f.doc.meta("Title").trim().is_empty();
        items.push(Item { name: "Title", ok: has_title, detail: if has_title { f.doc.meta("Title") } else { "The document has no title.".into() }, fix: (!has_title).then_some(Fix::Title) });
        let shows_title = root
            .get(b"ViewerPreferences")
            .ok()
            .and_then(|v| resolve(&doc, v))
            .is_some_and(|v| matches!(v.get(b"DisplayDocTitle"), Ok(Object::Boolean(true))));
        items.push(Item {
            name: "Title shown in the window",
            ok: shows_title,
            detail: if shows_title { "Readers show the title rather than the file name.".into() } else { "Readers show the file name instead of the title.".into() },
            fix: (!shows_title).then_some(Fix::DisplayTitle),
        });
        let lang = root.get(b"Lang").map(crate::annots::read_text).unwrap_or_default();
        items.push(Item {
            name: "Language",
            ok: !lang.is_empty(),
            detail: if lang.is_empty() { "No language is set, so screen readers may pronounce it wrongly.".into() } else { lang },
            fix: root.get(b"Lang").is_err().then_some(Fix::Language),
        });
        let scanned: Vec<usize> =
            (0..f.page_count()).filter(|&p| f.glyphs(p).is_empty() && f.objects(p).iter().any(|o| o.kind == Kind::Image)).collect();
        items.push(Item {
            name: "Readable text",
            ok: scanned.is_empty(),
            detail: if scanned.is_empty() {
                "Every page's text can be read out.".into()
            } else {
                format!("{} only pictures of text (scans).", plural(scanned.len(), "page has", "pages have"))
            },
            fix: (!scanned.is_empty()).then_some(Fix::Ocr),
        });
        if tagged {
            let figs = figures(&doc);
            let missing = figs.iter().filter(|g| g.alt.trim().is_empty()).count();
            items.push(Item {
                name: "Alternate text for figures",
                ok: missing == 0,
                detail: if figs.is_empty() {
                    "There are no figures.".into()
                } else if missing == 0 {
                    format!("All {} have a description.", plural(figs.len(), "figure", "figures"))
                } else {
                    format!("{} of {} have no description.", missing, plural(figs.len(), "figure", "figures"))
                },
                fix: (missing > 0).then_some(Fix::AltText),
            });
        }
        let mut fields = 0;
        let mut no_tip = 0;
        let mut tabs_missing = 0;
        for (_, pid) in doc.get_pages() {
            let Ok(page) = doc.get_dictionary(pid) else { continue };
            let annots = match page.get(b"Annots") {
                Ok(Object::Reference(r)) => doc.get_object(*r).and_then(Object::as_array).cloned().unwrap_or_default(),
                Ok(Object::Array(a)) => a.clone(),
                _ => Vec::new(),
            };
            if !annots.is_empty() && page.get(b"Tabs").and_then(Object::as_name).ok() != Some(b"S") {
                tabs_missing += 1;
            }
            for a in annots.iter().filter_map(|a| resolve(&doc, a)) {
                if a.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Widget") {
                    fields += 1;
                    let parent = a.get(b"Parent").ok().and_then(|p| resolve(&doc, p));
                    if !a.has(b"TU") && !parent.is_some_and(|p| p.has(b"TU")) {
                        no_tip += 1;
                    }
                }
            }
        }
        if fields > 0 {
            items.push(Item {
                name: "Form field descriptions",
                ok: no_tip == 0,
                detail: if no_tip == 0 { "Every field says what it is for.".into() } else { format!("{} of {} have no description for screen readers.", no_tip, plural(fields, "field", "fields")) },
                fix: (no_tip > 0).then_some(Fix::FieldNames),
            });
        }
        items.push(Item {
            name: "Tab order",
            ok: tabs_missing == 0,
            detail: if tabs_missing == 0 { "Links and fields are visited in reading order.".into() } else { format!("{} no set tab order.", plural(tabs_missing, "page has", "pages have")) },
            fix: (tabs_missing > 0).then_some(Fix::TabOrder),
        });
        let missing_fonts: Vec<String> = fonts(&doc).into_iter().filter(|f| !f.1).map(|f| f.0).collect();
        items.push(Item {
            name: "Fonts inside the file",
            ok: missing_fonts.is_empty(),
            detail: if missing_fonts.is_empty() { "Every font is embedded.".into() } else { format!("Not embedded: {}. Dockit cannot add them.", missing_fonts.join(", ")) },
            fix: None,
        });
        if f.doc.encrypted() {
            let allowed = f.doc.permissions() & (1 << 9) != 0;
            items.push(Item {
                name: "Assistive technology allowed",
                ok: allowed,
                detail: if allowed { "The protection lets screen readers read it.".into() } else { "The protection stops screen readers. Change it in Password and permissions.".into() },
                fix: None,
            });
        }
        Ok(items)
    }

    /// Apply one of the simple fixes (the others open their own windows).
    pub fn accessibility_fix(&mut self, fix: Fix) -> Result<String, String> {
        match fix {
            Fix::Tag => self.auto_tag(),
            Fix::Language | Fix::DisplayTitle | Fix::FieldNames | Fix::TabOrder => {
                let lang = user_language();
                self.file_update(&[], |u| {
                    let root_id = u.doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|_| "The document catalog is missing.".to_string())?;
                    let mut cat = u.dict(root_id)?;
                    match fix {
                        Fix::Language => cat.set("Lang", text_string(&lang)),
                        Fix::DisplayTitle => {
                            let mut vp = cat.get(b"ViewerPreferences").ok().and_then(|v| resolve(&u.doc, v)).unwrap_or_default();
                            vp.set("DisplayDocTitle", Object::Boolean(true));
                            cat.set("ViewerPreferences", Object::Dictionary(vp));
                        }
                        _ => {}
                    }
                    u.put(root_id, Object::Dictionary(cat));
                    let pages: Vec<(u32, ObjectId)> = u.doc.get_pages().into_iter().collect();
                    for (_, pid) in pages {
                        let mut page = u.dict(pid)?;
                        let annots = match page.get(b"Annots") {
                            Ok(Object::Reference(r)) => u.doc.get_object(*r).and_then(Object::as_array).cloned().unwrap_or_default(),
                            Ok(Object::Array(a)) => a.clone(),
                            _ => Vec::new(),
                        };
                        if fix == Fix::TabOrder && !annots.is_empty() {
                            page.set("Tabs", Object::Name(b"S".to_vec()));
                            u.put(pid, Object::Dictionary(page));
                        }
                        if fix == Fix::FieldNames {
                            for a in annots {
                                let Ok(aid) = a.as_reference() else { continue };
                                let Ok(mut d) = u.dict(aid) else { continue };
                                if d.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Widget") || d.has(b"TU") {
                                    continue;
                                }
                                // The field's name, made readable, describes it.
                                let target = match d.get(b"Parent") {
                                    Ok(Object::Reference(p)) if !d.has(b"T") => *p,
                                    _ => aid,
                                };
                                if target != aid {
                                    d = u.dict(target)?;
                                }
                                let name = d.get(b"T").map(crate::annots::read_text).unwrap_or_default().replace(['_', '.'], " ");
                                if !name.trim().is_empty() && !d.has(b"TU") {
                                    d.set("TU", text_string(name.trim()));
                                    u.put(target, Object::Dictionary(d));
                                }
                            }
                        }
                    }
                    Ok(())
                })
                .ok_or_else(|| self.error.take().unwrap_or_default())?;
                Ok(match fix {
                    Fix::Language => format!("Language set to {lang}"),
                    Fix::DisplayTitle => "Readers will show the title".into(),
                    Fix::FieldNames => "Fields described by their names".into(),
                    _ => "Tab order follows the reading order".into(),
                })
            }
            _ => Ok(String::new()),
        }
    }
}

// ── tags ──

#[derive(Clone)]
struct Elem {
    s: &'static str,
    page: usize,
    mcids: Vec<u32>,
    /// For figures: where it is (l, b, r, t).
    bbox: Option<[f32; 4]>,
    size: f32,
    bottom: f32,
}

impl Editor {
    /// Add tags to an untagged document. Pages whose content cannot be matched safely are left
    /// untagged (and said so).
    pub fn auto_tag(&mut self) -> Result<String, String> {
        self.commit_edits();
        let f = self.file.as_ref().ok_or("No document is open.")?;
        {
            let doc = lopdf::Document::load_mem(&f.doc.save_plain()?).map_err(|e| e.to_string())?;
            if doc.trailer.get(b"Root").ok().and_then(|o| resolve(&doc, o)).is_some_and(|r| r.has(b"StructTreeRoot")) {
                return Err("This document has tags already.".into());
            }
        }
        // The body text size: the size most characters are set in.
        let n = f.page_count();
        let mut sizes: Vec<(f32, usize)> = Vec::new();
        let mut per_page: Vec<Vec<(usize, Kind, String, f32, [f32; 4])>> = Vec::new();
        for page in 0..n {
            let Some(p) = f.doc.page(page) else {
                per_page.push(Vec::new());
                continue;
            };
            let texts = p.texts();
            let mut v = Vec::new();
            for o in f.objects(page).iter() {
                let q = o.quad;
                let bbox = [
                    q.iter().map(|c| c.0).fold(f32::MAX, f32::min),
                    q.iter().map(|c| c.1).fold(f32::MAX, f32::min),
                    q.iter().map(|c| c.0).fold(f32::MIN, f32::max),
                    q.iter().map(|c| c.1).fold(f32::MIN, f32::max),
                ];
                let (text, size) = if o.kind == Kind::Text {
                    let m = p.matrix(o.index);
                    let scale = (m[2] * m[2] + m[3] * m[3]).sqrt();
                    (texts.get(o.index).cloned().flatten().unwrap_or_default(), (p.font_size(o.index) * scale).round())
                } else {
                    (String::new(), 0.0)
                };
                if !text.trim().is_empty() {
                    match sizes.iter_mut().find(|s| (s.0 - size).abs() < 0.5) {
                        Some(s) => s.1 += text.len(),
                        None => sizes.push((size, text.len())),
                    }
                }
                v.push((o.index, o.kind, text, size, bbox));
            }
            per_page.push(v);
        }
        let body = sizes.iter().max_by_key(|s| s.1).map_or(11.0, |s| s.0);
        // Bigger sizes, largest first, become heading levels.
        let mut heads: Vec<f32> = sizes.iter().map(|s| s.0).filter(|&s| s >= body * 1.2).collect();
        heads.sort_by(|a, b| b.total_cmp(a));
        let level = |size: f32| -> &'static str {
            match heads.iter().position(|&h| (h - size).abs() < 0.5) {
                Some(0) => "H1",
                Some(1) => "H2",
                Some(_) => "H3",
                None => "P",
            }
        };

        let mut elems: Vec<Elem> = Vec::new();
        let mut skipped = Vec::new();
        for (page, objs) in per_page.into_iter().enumerate() {
            let mut marks: Vec<(usize, Edit)> = Vec::new();
            let mut page_elems: Vec<Elem> = Vec::new();
            let mut mcid = 0u32;
            for (index, kind, text, size, bbox) in objs {
                match kind {
                    Kind::Text if !text.trim().is_empty() => {
                        let s = level(size);
                        // Joins the paragraph before it when it carries straight on.
                        let joins = page_elems.last().is_some_and(|e| e.s == s && e.bbox.is_none() && (e.size - size).abs() <= size * 0.15 && e.bottom - bbox[3] < size * 1.8 && e.bottom - bbox[3] > -size * 1.2);
                        let tag = if s == "P" { "P" } else { s };
                        marks.push((index, Edit::Mark(format!("/{tag} <</MCID {mcid}>> BDC"), "EMC".into())));
                        if joins {
                            let e = page_elems.last_mut().unwrap();
                            e.mcids.push(mcid);
                            e.bottom = e.bottom.min(bbox[1]);
                        } else {
                            page_elems.push(Elem { s, page, mcids: vec![mcid], bbox: None, size, bottom: bbox[1] });
                        }
                        mcid += 1;
                    }
                    Kind::Image | Kind::Group => {
                        marks.push((index, Edit::Mark(format!("/Figure <</MCID {mcid}>> BDC"), "EMC".into())));
                        page_elems.push(Elem { s: "Figure", page, mcids: vec![mcid], bbox: Some(bbox), size: 0.0, bottom: bbox[1] });
                        mcid += 1;
                    }
                    // Lines, boxes, backgrounds and empty text are decoration.
                    _ => marks.push((index, Edit::Mark("/Artifact BMC".into(), "EMC".into()))),
                }
            }
            if marks.is_empty() {
                continue;
            }
            if self.surgery(page, marks, Vec::new()) {
                elems.extend(page_elems);
            } else {
                self.error = None;
                skipped.push(page + 1);
            }
        }
        let lang = user_language();
        let count = elems.len();
        self.file_update(&(0..n).collect::<Vec<_>>(), |u| build_tree(u, &elems, &lang)).ok_or_else(|| self.error.take().unwrap_or_default())?;
        let mut msg = format!("Added tags: {}", plural(count, "element", "elements"));
        let figs = elems.iter().filter(|e| e.s == "Figure").count();
        if figs > 0 {
            msg += &format!(", including {} to describe", plural(figs, "figure", "figures"));
        }
        if !skipped.is_empty() {
            msg += &format!(". Pages left untagged (their content could not be matched safely): {}", skipped.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "));
        }
        Ok(msg)
    }
}

fn build_tree(u: &mut Update, elems: &[Elem], lang: &str) -> Result<(), String> {
    let pages: Vec<ObjectId> = u.doc.get_pages().values().copied().collect();
    let root_id = u.doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|_| "The document catalog is missing.".to_string())?;
    // Ids first, so parents and children can point at each other.
    u.doc.max_id += 2;
    let tree_id = (u.doc.max_id - 1, 0);
    let document_id = (u.doc.max_id, 0);
    let mut kids = Vec::new();
    let mut parents: Vec<Vec<Object>> = vec![Vec::new(); pages.len()];
    for e in elems {
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"StructElem".to_vec()));
        d.set("S", Object::Name(e.s.as_bytes().to_vec()));
        d.set("P", Object::Reference(document_id));
        d.set("Pg", Object::Reference(pages[e.page]));
        d.set("K", if e.mcids.len() == 1 { Object::Integer(e.mcids[0] as i64) } else { Object::Array(e.mcids.iter().map(|&m| Object::Integer(m as i64)).collect()) });
        if let Some(b) = e.bbox {
            let mut a = Dictionary::new();
            a.set("O", Object::Name(b"Layout".to_vec()));
            a.set("BBox", Object::Array(b.iter().map(|&v| Object::Real(v)).collect()));
            d.set("A", Object::Dictionary(a));
        }
        let id = u.add(Object::Dictionary(d));
        kids.push(Object::Reference(id));
        let slot = &mut parents[e.page];
        for &m in &e.mcids {
            if slot.len() <= m as usize {
                slot.resize(m as usize + 1, Object::Null);
            }
            slot[m as usize] = Object::Reference(id);
        }
    }
    let mut document = Dictionary::new();
    document.set("Type", Object::Name(b"StructElem".to_vec()));
    document.set("S", Object::Name(b"Document".to_vec()));
    document.set("P", Object::Reference(tree_id));
    document.set("K", Object::Array(kids));
    u.put(document_id, Object::Dictionary(document));
    let mut nums = Vec::new();
    for (k, list) in parents.into_iter().enumerate() {
        nums.push(Object::Integer(k as i64));
        nums.push(Object::Array(list));
    }
    let mut parent_tree = Dictionary::new();
    parent_tree.set("Nums", Object::Array(nums));
    let mut tree = Dictionary::new();
    tree.set("Type", Object::Name(b"StructTreeRoot".to_vec()));
    tree.set("K", Object::Reference(document_id));
    tree.set("ParentTree", Object::Dictionary(parent_tree));
    tree.set("ParentTreeNextKey", Object::Integer(pages.len() as i64));
    u.put(tree_id, Object::Dictionary(tree));
    for (k, pid) in pages.iter().enumerate() {
        let mut page = u.dict(*pid)?;
        page.set("StructParents", Object::Integer(k as i64));
        page.set("Tabs", Object::Name(b"S".to_vec()));
        u.put(*pid, Object::Dictionary(page));
    }
    let mut cat = u.dict(root_id)?;
    cat.set("StructTreeRoot", Object::Reference(tree_id));
    let mut mark = Dictionary::new();
    mark.set("Marked", Object::Boolean(true));
    cat.set("MarkInfo", Object::Dictionary(mark));
    if !cat.has(b"Lang") {
        cat.set("Lang", text_string(lang));
    }
    u.put(root_id, Object::Dictionary(cat));
    Ok(())
}

// ── alternate text ──

#[derive(Clone)]
pub struct Figure {
    pub id: ObjectId,
    pub page: usize,
    pub bbox: Option<[f32; 4]>,
    pub alt: String,
}

/// Every Figure element in the structure tree.
pub fn figures(doc: &lopdf::Document) -> Vec<Figure> {
    let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let mut out = Vec::new();
    let Some(root) = doc.trailer.get(b"Root").ok().and_then(|o| resolve(doc, o)) else { return out };
    let Ok(tree) = root.get(b"StructTreeRoot") else { return out };
    let mut stack: Vec<Object> = vec![tree.clone()];
    let mut seen = 0;
    while let Some(o) = stack.pop() {
        seen += 1;
        if seen > 200_000 {
            break;
        }
        let id = o.as_reference().ok();
        let Some(d) = resolve(doc, &o) else {
            if let Object::Array(a) = o {
                stack.extend(a);
            }
            continue;
        };
        if d.get(b"S").and_then(Object::as_name).ok() == Some(b"Figure")
            && let Some(id) = id
        {
            let page = d.get(b"Pg").and_then(Object::as_reference).ok().and_then(|pg| pages.iter().position(|p| *p == pg)).unwrap_or(0);
            let bbox = d
                .get(b"A")
                .ok()
                .and_then(|a| resolve(doc, a))
                .and_then(|a| a.get(b"BBox").ok().and_then(|b| b.as_array().ok().cloned()))
                .and_then(|b| (b.len() == 4).then(|| [0, 1, 2, 3].map(|k| b[k].as_float().unwrap_or(0.0))));
            out.push(Figure { id, page, bbox, alt: d.get(b"Alt").map(crate::annots::read_text).unwrap_or_default() });
        }
        match d.get(b"K") {
            Ok(Object::Array(a)) => stack.extend(a.iter().cloned()),
            Ok(k @ Object::Reference(_)) | Ok(k @ Object::Dictionary(_)) => stack.push(k.clone()),
            _ => {}
        }
    }
    out.sort_by_key(|f| (f.page, f.bbox.map(|b| -(b[3] as i64)).unwrap_or(0)));
    out
}

pub struct AltDialog {
    figures: Vec<Figure>,
    texts: Vec<String>,
    thumbs: Vec<Option<egui::TextureHandle>>,
    error: Option<String>,
}

impl AltDialog {
    pub fn new(ed: &Editor) -> Self {
        let figs = ed.file.as_ref().and_then(|f| f.doc.save_plain().ok()).and_then(|b| lopdf::Document::load_mem(&b).ok()).map(|d| figures(&d)).unwrap_or_default();
        AltDialog { texts: figs.iter().map(|f| f.alt.clone()).collect(), thumbs: vec![None; figs.len()], figures: figs, error: None }
    }
}

pub fn alt_window(ctx: &egui::Context, ed: &mut Editor, d: &mut AltDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut save = false;
    // Small pictures of each figure, made once.
    for (k, fig) in d.figures.iter().enumerate() {
        if d.thumbs[k].is_some() {
            continue;
        }
        let (Some(f), Some(b)) = (&ed.file, fig.bbox) else { continue };
        let Some(g) = f.geom(fig.page) else { continue };
        let (x0, y0) = g.to_view(b[0], b[3]);
        let (x1, y1) = g.to_view(b[2], b[1]);
        let w = (x1 - x0).abs().max(1.0);
        let scale = (120.0 / w).min(2.0);
        let r = [(x0.min(x1) * scale) as i32, (y0.min(y1) * scale) as i32, (x0.max(x1) * scale) as i32 + 1, (y0.max(y1) * scale) as i32 + 1];
        if let Some(img) = f.doc.page(fig.page).and_then(|pg| pg.render(scale, r)) {
            d.thumbs[k] = Some(ctx.load_texture(format!("alt-{k}"), img, egui::TextureOptions::LINEAR));
        }
    }
    let (_, close) = modal(ctx, "alt-text", 520.0, |ui| {
        title(ui, "Describe figures");
        ui.label(theme::secondary(ui, "Screen readers read these descriptions in place of the pictures. Leave a description empty only for pure decoration."));
        ui.add_space(6.0);
        if d.figures.is_empty() {
            ui.label(theme::secondary(ui, "There are no figures. Add tags first (Accessibility check) if the document has pictures."));
        }
        egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
            for (k, fig) in d.figures.iter().enumerate() {
                ui.horizontal(|ui| {
                    match &d.thumbs[k] {
                        Some(t) => {
                            let s = t.size_vec2() / ctx.pixels_per_point();
                            let s = s * (90.0 / s.y.max(90.0)).min(1.0);
                            ui.image((t.id(), s));
                        }
                        None => {
                            ui.label(theme::secondary(ui, "Figure"));
                        }
                    }
                    ui.vertical(|ui| {
                        ui.label(theme::secondary(ui, format!("Page {}", fig.page + 1)));
                        ui.add(egui::TextEdit::multiline(&mut d.texts[k]).desired_rows(2).desired_width(f32::INFINITY).hint_text("What the picture shows"));
                    });
                });
                ui.add_space(4.0);
            }
        });
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Save", !d.figures.is_empty()) {
                save = true;
            }
            if secondary_button(ui, "Cancel") {
                open = false;
            }
        });
    });
    if close {
        open = false;
    }
    if save {
        let changes: Vec<(ObjectId, String)> = d.figures.iter().zip(&d.texts).filter(|(f, t)| f.alt != **t).map(|(f, t)| (f.id, t.trim().to_string())).collect();
        let n = changes.len();
        if n == 0 {
            return false;
        }
        let ok = ed.file_update(&[], |u| {
            for (id, text) in &changes {
                let mut el = u.dict(*id)?;
                if text.is_empty() {
                    el.remove(b"Alt");
                } else {
                    el.set("Alt", text_string(text));
                }
                u.put(*id, Object::Dictionary(el));
            }
            Ok(())
        });
        if ok.is_some() {
            ed.say(format!("Saved {}", plural(n, "description", "descriptions")));
            open = false;
        }
    }
    open
}

// ── the check window ──

pub struct CheckDialog {
    items: Vec<Item>,
    note: Option<String>,
    error: Option<String>,
}

/// What the check window asks the app to open.
pub enum CheckAsk {
    Properties,
    Ocr,
    AltText,
}

impl CheckDialog {
    pub fn new(ed: &Editor) -> Self {
        match ed.accessibility_check() {
            Ok(items) => CheckDialog { items, note: None, error: None },
            Err(e) => CheckDialog { items: Vec::new(), note: None, error: Some(e) },
        }
    }
}

/// Returns (still open, a window to open).
pub fn check_window(ctx: &egui::Context, ed: &mut Editor, d: &mut CheckDialog) -> (bool, Option<CheckAsk>) {
    let p = pal(ctx);
    let mut open = true;
    let mut fix = None;
    let (_, close) = modal(ctx, "accessibility", 520.0, |ui| {
        title(ui, "Accessibility check");
        let bad = d.items.iter().filter(|i| !i.ok).count();
        ui.label(theme::secondary(
            ui,
            if bad == 0 && d.error.is_none() { "Everything checked is in order for screen readers.".to_string() } else { format!("{} to look at.", plural(bad, "thing", "things")) },
        ));
        ui.add_space(6.0);
        for it in &d.items {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if it.ok { "✔" } else { "✖" }).color(if it.ok { p.accent_text } else { p.danger }));
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - 90.0);
                    ui.label(theme::semibold(it.name));
                    ui.label(theme::secondary(ui, &it.detail).size(12.0));
                });
                if let Some(f) = it.fix {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Fix").clicked() {
                            fix = Some(f);
                        }
                    });
                }
            });
            ui.add_space(4.0);
        }
        if let Some(n) = &d.note {
            ui.label(RichText::new(n).color(p.accent_text).size(12.0));
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Done", true) {
                open = false;
            }
        });
    });
    if close {
        open = false;
    }
    let mut ask = None;
    if let Some(f) = fix {
        match f {
            Fix::Title => ask = Some(CheckAsk::Properties),
            Fix::Ocr => ask = Some(CheckAsk::Ocr),
            Fix::AltText => ask = Some(CheckAsk::AltText),
            other => match ed.accessibility_fix(other) {
                Ok(msg) => {
                    d.note = Some(msg);
                    d.error = None;
                    if let Ok(items) = ed.accessibility_check() {
                        d.items = items;
                    }
                }
                Err(e) => d.error = Some(e),
            },
        }
    }
    (open && ask.is_none(), ask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Tagging a real document (run with --ignored; needs pdfium.dll in target/debug).
    #[test]
    #[ignore]
    fn tags_a_document_without_changing_its_look() {
        crate::pdf::init(Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug").as_path()).unwrap();
        let dir = std::env::temp_dir().join("dockit-phase-e");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("tag.pdf");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("target/devtools/out/p1.pdf"), &path).unwrap();
        let mut ed = Editor::open_headless(&path).unwrap();
        let render = |ed: &Editor, k: usize| {
            let f = ed.file.as_ref().unwrap();
            let (w, h) = f.sizes[k];
            f.doc.page(k).unwrap().render(1.0, [0, 0, w as i32, h as i32]).unwrap().pixels
        };
        let before: Vec<_> = (0..4).map(|k| render(&ed, k)).collect();
        let before_check = ed.accessibility_check().unwrap();
        assert!(!before_check.iter().find(|i| i.name == "Tags").unwrap().ok);
        let msg = ed.auto_tag().unwrap();
        println!("{msg}");
        for k in 0..4 {
            assert!(render(&ed, k) == before[k], "page {} looks the same", k + 1);
        }
        let items = ed.accessibility_check().unwrap();
        for it in &items {
            println!("{} {}: {}", if it.ok { "ok" } else { "NO" }, it.name, it.detail);
        }
        assert!(items.iter().find(|i| i.name == "Tags").unwrap().ok);
        assert!(items.iter().find(|i| i.name == "Language").unwrap().ok);
        // The structure tree is sound: every marked piece has its element in the parent tree.
        let doc = lopdf::Document::load_mem(&ed.file.as_ref().unwrap().doc.save_plain().unwrap()).unwrap();
        let root = doc.trailer.get(b"Root").ok().and_then(|o| resolve(&doc, o)).unwrap();
        let tree = resolve(&doc, root.get(b"StructTreeRoot").unwrap()).unwrap();
        let nums = tree.get(b"ParentTree").ok().and_then(|p| resolve(&doc, p)).unwrap();
        let nums = nums.get(b"Nums").unwrap().as_array().unwrap();
        assert_eq!(nums.len(), 8, "one entry per page");
        assert!(nums[1].as_array().unwrap().iter().all(|o| o.as_reference().is_ok()));
        assert!(ed.accessibility_fix(Fix::DisplayTitle).is_ok());
        assert!(ed.auto_tag().is_err(), "tagging twice is refused");
    }
}
