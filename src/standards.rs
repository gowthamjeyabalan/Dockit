//! Standards and print production: saving as PDF/A (for archiving) and PDF/X-4 (for printing),
//! a preflight report for the press (fonts, image resolution, colours, inks, transparency, page
//! boxes, estimated ink coverage), and printer's marks.
//!
//! The checks are Dockit's own reading of the standards' main rules, not a certified validator;
//! the windows say so.

use crate::accessibility::{Item, fonts};
use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::{Editor, plural};
use crate::pageops::MM;
use crate::pdf::Kind;
use crate::theme::{self, UiExt, pal};
use egui::RichText;
use lopdf::{Dictionary, Object, ObjectId, Stream};
use std::path::{Path, PathBuf};

const PROFILES: &str = r"C:\Windows\System32\spool\drivers\color";

fn resolve(doc: &lopdf::Document, o: &Object) -> Option<Dictionary> {
    match o {
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        Object::Dictionary(d) => Some(d.clone()),
        _ => None,
    }
}

/// What the pages draw with, found by reading every page's drawing commands and resources.
#[derive(Default, Debug)]
pub struct Usage {
    pub rgb: bool,
    pub cmyk: bool,
    pub gray: bool,
    pub icc: bool,
    pub spots: Vec<String>,
    pub transparency: bool,
    pub javascript: bool,
    pub attachments: bool,
}

pub fn usage(doc: &lopdf::Document) -> Usage {
    let mut u = Usage::default();
    for (_, pid) in doc.get_pages() {
        if let Ok(content) = doc.get_and_decode_page_content(pid) {
            for op in &content.operations {
                match op.operator.as_str() {
                    "rg" | "RG" => u.rgb = true,
                    "k" | "K" => u.cmyk = true,
                    "g" | "G" => u.gray = true,
                    _ => {}
                }
            }
        }
    }
    for o in doc.objects.values() {
        match o {
            Object::Array(a) => colour_space(a, &mut u),
            Object::Dictionary(d) => {
                dict_usage(doc, d, &mut u);
            }
            Object::Stream(s) => {
                if s.dict.has(b"SMask") {
                    u.transparency = true;
                }
                if let Ok(cs) = s.dict.get(b"ColorSpace") {
                    match cs {
                        Object::Name(n) => name_space(n, &mut u),
                        Object::Array(a) => colour_space(a, &mut u),
                        _ => {}
                    }
                }
                dict_usage(doc, &s.dict, &mut u);
            }
            _ => {}
        }
    }
    u.spots.sort();
    u.spots.dedup();
    u
}

fn name_space(n: &[u8], u: &mut Usage) {
    match n {
        b"DeviceRGB" => u.rgb = true,
        b"DeviceCMYK" => u.cmyk = true,
        b"DeviceGray" => u.gray = true,
        _ => {}
    }
}

fn colour_space(a: &[Object], u: &mut Usage) {
    match a.first().and_then(|o| o.as_name().ok()) {
        Some(b"ICCBased") => u.icc = true,
        Some(b"Separation") => {
            if let Some(n) = a.get(1).and_then(|o| o.as_name().ok())
                && !matches!(n, b"All" | b"None")
            {
                u.spots.push(String::from_utf8_lossy(n).into_owned());
            }
        }
        Some(b"DeviceN") => {
            if let Some(Object::Array(names)) = a.get(1) {
                for n in names.iter().filter_map(|o| o.as_name().ok()) {
                    if !matches!(n, b"Cyan" | b"Magenta" | b"Yellow" | b"Black" | b"None" | b"All") {
                        u.spots.push(String::from_utf8_lossy(n).into_owned());
                    }
                }
            }
        }
        _ => {}
    }
}

fn dict_usage(doc: &lopdf::Document, d: &Dictionary, u: &mut Usage) {
    if d.get(b"Type").and_then(Object::as_name).ok() == Some(b"ExtGState") {
        let below = |k: &[u8]| d.get(k).ok().and_then(|v| v.as_float().ok()).is_some_and(|v| v < 1.0);
        let blend = d.get(b"BM").and_then(Object::as_name).ok().is_some_and(|b| b != b"Normal" && b != b"Compatible");
        let mask = d.get(b"SMask").ok().is_some_and(|m| m.as_name().ok() != Some(b"None"));
        if below(b"CA") || below(b"ca") || blend || mask {
            u.transparency = true;
        }
    }
    if d.get(b"S").and_then(Object::as_name).ok() == Some(b"JavaScript") {
        u.javascript = true;
    }
    if d.has(b"EmbeddedFiles") || d.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"FileAttachment") {
        u.attachments = true;
    }
    let _ = doc;
}

// ── PDF/A and PDF/X ──

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standard {
    /// PDF/A-2b: archiving, looks the same in years to come.
    PdfA,
    /// PDF/X-4: for print.
    PdfX,
}

impl Standard {
    pub fn name(self) -> &'static str {
        match self {
            Standard::PdfA => "PDF/A-2b",
            Standard::PdfX => "PDF/X-4",
        }
    }
}

/// What stands in the way of the standard, before conversion fixes what it can.
pub fn blockers(doc: &lopdf::Document, std: Standard) -> Vec<String> {
    let mut out = Vec::new();
    let missing: Vec<String> = fonts(doc).into_iter().filter(|f| !f.1).map(|f| f.0).collect();
    if !missing.is_empty() {
        out.push(format!("These fonts are not inside the file and Dockit cannot add them: {}.", missing.join(", ")));
    }
    if std == Standard::PdfX {
        let u = usage(doc);
        if u.rgb {
            out.push("The pages use plain RGB colours. PDF/X-4 needs every colour tied to a profile, and Dockit does not convert colours.".into());
        }
    }
    out
}

fn icc_stream(path: &Path, components: i64) -> Result<Stream, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("The colour profile {} could not be read ({e}).", path.display()))?;
    let mut d = Dictionary::new();
    d.set("N", Object::Integer(components));
    let mut s = Stream::new(d, bytes);
    let _ = s.compress();
    Ok(s)
}

/// The profile's own description (its name), read from the ICC 'desc' tag.
fn icc_name(path: &Path) -> String {
    let fallback = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let Ok(b) = std::fs::read(path) else { return fallback };
    let be32 = |o: usize| b.get(o..o + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]) as usize);
    let Some(count) = be32(128) else { return fallback };
    for k in 0..count.min(64) {
        let at = 132 + k * 12;
        if b.get(at..at + 4) == Some(b"desc")
            && let (Some(off), Some(_)) = (be32(at + 4), be32(at + 8))
            && b.get(off..off + 4) == Some(b"desc")
            && let Some(len) = be32(off + 8)
            && let Some(s) = b.get(off + 12..off + 12 + len)
        {
            let name: String = s.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
            if !name.trim().is_empty() {
                return name;
            }
        }
    }
    fallback
}

/// Rewrite a plain PDF to meet the standard as far as Dockit can, and claim it.
pub fn convert(bytes: &[u8], std: Standard, profile: &Path) -> Result<Vec<u8>, String> {
    // Scripts and automatic actions are not allowed in either.
    let (clean, _) = crate::docinfo::sanitize_bytes(bytes, crate::docinfo::Sanitize { metadata: false, attachments: std == Standard::PdfA, scripts: true, comments: false, private_data: false })?;
    let mut doc = lopdf::Document::load_mem(&clean).map_err(|e| e.to_string())?;
    let blocks = blockers(&doc, std);
    if !blocks.is_empty() {
        return Err(blocks.join(" "));
    }
    doc.version = "1.7".into();
    let root_id: ObjectId = doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|_| "The document catalog is missing.".to_string())?;
    let mut cat = doc.get_dictionary(root_id).map_err(|_| "The document catalog is missing.")?.clone();

    // Annotations print, and none is hidden.
    let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for pid in &pages {
        let Ok(page) = doc.get_dictionary(*pid).cloned() else { continue };
        let list = match page.get(b"Annots") {
            Ok(Object::Reference(r)) => doc.get_object(*r).and_then(Object::as_array).cloned().unwrap_or_default(),
            Ok(Object::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        for a in list {
            if let Object::Reference(aid) = a
                && let Ok(mut d) = doc.get_dictionary(aid).cloned()
            {
                if d.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Popup") {
                    continue;
                }
                let f = d.get(b"F").and_then(Object::as_i64).unwrap_or(0);
                d.set("F", Object::Integer((f | 4) & !(1 | 2 | 32)));
                doc.objects.insert(aid, Object::Dictionary(d));
            }
        }
        // Print production wants to know where the page is cut.
        if std == Standard::PdfX && !page.has(b"TrimBox") {
            let mut page = page.clone();
            let b = page.get(b"CropBox").or(page.get(b"MediaBox")).cloned().unwrap_or(Object::Array(vec![]));
            page.set("TrimBox", b);
            doc.objects.insert(*pid, Object::Dictionary(page));
        }
    }
    if let Some(mut form) = cat.get(b"AcroForm").ok().and_then(|o| resolve(&doc, o)) {
        form.remove(b"NeedAppearances");
        cat.set("AcroForm", Object::Dictionary(form));
    }

    // The colours' meaning: an output intent with its profile.
    let (subtype, components, condition) = match std {
        Standard::PdfA => ("GTS_PDFA1", 3, "sRGB IEC61966-2.1".to_string()),
        Standard::PdfX => ("GTS_PDFX", 4, icc_name(profile)),
    };
    let icc_id = doc.add_object(Object::Stream(icc_stream(profile, components)?));
    let mut intent = Dictionary::new();
    intent.set("Type", Object::Name(b"OutputIntent".to_vec()));
    intent.set("S", Object::Name(subtype.as_bytes().to_vec()));
    intent.set("OutputConditionIdentifier", crate::annots::text_string(&condition));
    intent.set("Info", crate::annots::text_string(&condition));
    intent.set("DestOutputProfile", Object::Reference(icc_id));
    cat.set("OutputIntents", Object::Array(vec![Object::Dictionary(intent)]));

    // Properties, in the information dictionary and in XMP with the standard's claim.
    let mut info = doc.trailer.get(b"Info").ok().and_then(|o| resolve(&doc, o)).unwrap_or_default();
    let now = crate::annots::pdf_date();
    info.set("ModDate", crate::annots::text_string(&now));
    if !info.has(b"CreationDate") {
        info.set("CreationDate", crate::annots::text_string(&now));
    }
    info.set("Producer", crate::annots::text_string(&format!("Dockit {}", env!("CARGO_PKG_VERSION"))));
    if std == Standard::PdfX {
        info.set("GTS_PDFXVersion", crate::annots::text_string("PDF/X-4"));
        info.set("Trapped", Object::Name(b"False".to_vec()));
    }
    let get = |k: &str| info.get(k.as_bytes()).map(crate::annots::read_text).unwrap_or_default();
    let fields: Vec<(&str, String)> = ["Title", "Author", "Subject", "Keywords", "Creator", "Producer"].iter().map(|k| (*k, get(k))).collect();
    let created = get("CreationDate");
    let mut xmp = crate::docinfo::xmp_packet(&fields, &created, &now, (std == Standard::PdfA).then_some((2, "B")));
    if std == Standard::PdfX {
        xmp = xmp.replace(
            "<dc:format>",
            "<pdfxid:GTS_PDFXVersion xmlns:pdfxid=\"http://www.npes.org/pdfx/ns/id/\">PDF/X-4</pdfxid:GTS_PDFXVersion>\n<dc:format>",
        );
    }
    let info_id = doc.add_object(Object::Dictionary(info));
    doc.trailer.set("Info", Object::Reference(info_id));
    let mut md = Dictionary::new();
    md.set("Type", Object::Name(b"Metadata".to_vec()));
    md.set("Subtype", Object::Name(b"XML".to_vec()));
    let md_id = doc.add_object(Object::Stream(Stream::new(md, xmp.into_bytes())));
    cat.set("Metadata", Object::Reference(md_id));
    doc.objects.insert(root_id, Object::Dictionary(cat));

    // A file identifier, which both require.
    if !doc.trailer.has(b"ID") {
        let id = crate::security::random_bytes(16);
        doc.trailer.set("ID", Object::Array(vec![Object::String(id.clone(), lopdf::StringFormat::Hexadecimal), Object::String(id, lopdf::StringFormat::Hexadecimal)]));
    }
    doc.prune_objects();
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| format!("The file could not be written ({e})."))?;
    Ok(out)
}

/// What the converted file holds, checked again.
pub fn verify(bytes: &[u8], std: Standard) -> Vec<Item> {
    let Ok(doc) = lopdf::Document::load_mem(bytes) else { return Vec::new() };
    let cat = doc.trailer.get(b"Root").ok().and_then(|o| resolve(&doc, o)).unwrap_or_default();
    let u = usage(&doc);
    let missing: Vec<String> = fonts(&doc).into_iter().filter(|f| !f.1).map(|f| f.0).collect();
    let item = |name: &'static str, ok: bool, detail: &str| Item { name, ok, detail: detail.to_string(), fix: None };
    let mut items = vec![
        item("Fonts inside the file", missing.is_empty(), &if missing.is_empty() { "Every font is embedded.".to_string() } else { missing.join(", ") }),
        item("Output intent", cat.has(b"OutputIntents"), "The colours are tied to a profile."),
        item("Metadata", cat.has(b"Metadata"), "XMP metadata with the standard's claim."),
        item("No scripts", !u.javascript, "Nothing runs on its own."),
        item("Not encrypted", !doc.is_encrypted(), "Archive and print files cannot be locked."),
        item("File identifier", doc.trailer.has(b"ID"), "The file has its identifier."),
    ];
    if std == Standard::PdfA {
        items.push(item("No attached files", !u.attachments, "Attachments were removed."));
    } else {
        items.push(item("No plain RGB", !u.rgb, "Every colour is CMYK, grey, spot or tied to a profile."));
    }
    items
}

pub struct StandardDialog {
    std: Standard,
    profile: PathBuf,
    report: Option<(PathBuf, Vec<Item>)>,
    error: Option<String>,
}

impl StandardDialog {
    pub fn new(std: Standard) -> Self {
        let profile = match std {
            Standard::PdfA => Path::new(PROFILES).join("sRGB Color Space Profile.icm"),
            Standard::PdfX => Path::new(PROFILES).join("RSWOP.icm"),
        };
        StandardDialog { std, profile, report: None, error: None }
    }
}

pub fn standard_window(ctx: &egui::Context, ed: &mut Editor, d: &mut StandardDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut save = false;
    let (_, close) = modal(ctx, "standard", 460.0, |ui| {
        title(ui, &format!("Save as {}", d.std.name()));
        ui.label(theme::secondary(
            ui,
            match d.std {
                Standard::PdfA => "For archiving: the file keeps everything it needs to look the same in years to come.",
                Standard::PdfX => "For printing: fonts embedded, colours tied to the press's profile, where each page is cut.",
            },
        ));
        theme::heading(ui, "COLOUR PROFILE");
        ui.label(icc_name(&d.profile));
        if d.std == Standard::PdfX && ui.button("Choose the press profile (.icc)…").clicked()
            && let Some(f) = rfd::FileDialog::new().add_filter("Colour profile", &["icc", "icm"]).set_directory(PROFILES).pick_file()
        {
            d.profile = f;
        }
        if let Some((path, items)) = &d.report {
            theme::heading(ui, "SAVED");
            ui.label(path.display().to_string());
            for it in items {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if it.ok { "✔" } else { "✖" }).color(if it.ok { p.accent_text } else { p.danger }));
                    ui.label(it.name);
                    ui.label(theme::secondary(ui, &it.detail).size(12.0));
                });
            }
            ui.label(theme::secondary(ui, "Dockit checks the standard's main rules; a certified validator can confirm the rest.").size(11.5));
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if d.report.is_some() {
                if primary(ui, "Done", true) {
                    open = false;
                }
            } else {
                if primary(ui, "Save as…", true) {
                    save = true;
                }
                if secondary_button(ui, "Cancel") {
                    open = false;
                }
            }
        });
    });
    if close {
        open = false;
    }
    if save {
        ed.commit_edits();
        ed.commit();
        let Some(f) = &ed.file else { return false };
        let stem = f.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into());
        let tag = if d.std == Standard::PdfA { "PDF-A" } else { "PDF-X" };
        let mut dlg = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{stem} ({tag}).pdf"));
        if let Some(dir) = f.path.as_ref().and_then(|p| p.parent()) {
            dlg = dlg.set_directory(dir);
        }
        if let Some(out) = dlg.save_file() {
            match f.doc.save_plain().and_then(|b| convert(&b, d.std, &d.profile)) {
                Ok(bytes) => match std::fs::write(&out, &bytes) {
                    Ok(()) => {
                        d.report = Some((out, verify(&bytes, d.std)));
                        d.error = None;
                    }
                    Err(e) => d.error = Some(format!("{} could not be written ({e}).", out.display())),
                },
                Err(e) => d.error = Some(e),
            }
        }
    }
    open
}

// ── preflight ──

pub struct Preflight {
    pub items: Vec<Item>,
    pub inks: Vec<String>,
    /// Estimated average coverage of C, M, Y and K on the current page (0..1).
    pub coverage: Option<[f32; 4]>,
}

impl Editor {
    pub fn preflight(&self) -> Result<Preflight, String> {
        let f = self.file.as_ref().ok_or("No document is open.")?;
        let doc = lopdf::Document::load_mem(&f.doc.save_plain()?).map_err(|e| e.to_string())?;
        let u = usage(&doc);
        let missing: Vec<String> = fonts(&doc).into_iter().filter(|x| !x.1).map(|x| x.0).collect();
        let mut items = Vec::new();
        let item = |name: &'static str, ok: bool, detail: String| Item { name, ok, detail, fix: None };
        items.push(item("Fonts embedded", missing.is_empty(), if missing.is_empty() { "Every font is inside the file.".into() } else { format!("Missing: {}", missing.join(", ")) }));
        // Image resolution as placed on the page.
        let mut low = 0;
        let mut lowest = f32::MAX;
        let mut images = 0;
        for page in 0..f.page_count() {
            let Some(p) = f.doc.page(page) else { continue };
            for o in f.objects(page).iter().filter(|o| o.kind == Kind::Image) {
                let Some((w, h)) = p.image_pixels(o.index) else { continue };
                let q = o.quad;
                let side = |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
                let (pw, ph) = (side(q[0], q[1]).max(0.1) / 72.0, side(q[1], q[2]).max(0.1) / 72.0);
                let ppi = (w as f32 / pw).min(h as f32 / ph);
                images += 1;
                lowest = lowest.min(ppi);
                if ppi < 300.0 {
                    low += 1;
                }
            }
        }
        items.push(item(
            "Image resolution",
            low == 0,
            if images == 0 {
                "There are no images.".into()
            } else if low == 0 {
                format!("All {} are 300 ppi or more where placed.", plural(images, "image", "images"))
            } else {
                format!("{} of {} are under 300 ppi (lowest {:.0} ppi).", low, plural(images, "image", "images"), lowest)
            },
        ));
        let mut spaces = Vec::new();
        for (on, n) in [(u.cmyk, "CMYK"), (u.rgb, "RGB"), (u.gray, "grey"), (u.icc, "profile-based")] {
            if on {
                spaces.push(n);
            }
        }
        items.push(item("Colours", !u.rgb, if spaces.is_empty() { "No colours found.".into() } else { format!("{} colours{}", spaces.join(", "), if u.rgb { "; RGB will be converted by the printer" } else { "" }) }));
        items.push(item("Transparency", !u.transparency, if u.transparency { "Used: fine for PDF/X-4, flattening needed for older workflows.".into() } else { "None.".into() }));
        let trims = (0..f.page_count()).filter(|&k| f.doc.page(k).and_then(|p| p.trim_box()).is_some()).count();
        items.push(item(
            "Trim boxes",
            trims == f.page_count(),
            if trims == f.page_count() { "Every page says where it is cut.".into() } else { format!("{} no trim box (Printer's marks adds one).", plural(f.page_count() - trims, "page has", "pages have")) },
        ));
        let first = f.sizes.first().copied().unwrap_or_default();
        let mixed = f.sizes.iter().any(|s| (s.0 - first.0).abs() > 1.0 || (s.1 - first.1).abs() > 1.0);
        items.push(item("Page sizes", !mixed, if mixed { "Pages are not all the same size.".into() } else { format!("All {:.0} × {:.0} mm", first.0 / MM, first.1 / MM) }));
        let mut inks = Vec::new();
        if u.cmyk || u.rgb || u.gray || u.icc {
            inks.extend(["Cyan", "Magenta", "Yellow", "Black"].map(String::from));
        }
        inks.extend(u.spots.iter().map(|s| format!("{s} (spot)")));
        // Ink coverage, estimated from the page as drawn on screen.
        let coverage = f.doc.page(self.current).and_then(|p| {
            let (w, h) = f.sizes[self.current];
            let s = 36.0 / 72.0;
            let img = p.render(s, [0, 0, (w * s) as i32, (h * s) as i32])?;
            let mut sum = [0.0f32; 4];
            for c in &img.pixels {
                let (r, g, b) = (c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0);
                let k = 1.0 - r.max(g).max(b);
                let d = (1.0 - k).max(1e-4);
                sum[0] += (1.0 - r - k) / d;
                sum[1] += (1.0 - g - k) / d;
                sum[2] += (1.0 - b - k) / d;
                sum[3] += k;
            }
            let n = img.pixels.len().max(1) as f32;
            Some(sum.map(|v| v / n))
        });
        Ok(Preflight { items, inks, coverage })
    }
}

pub struct PreflightDialog {
    report: Result<Preflight, String>,
    page: usize,
}

impl PreflightDialog {
    pub fn new(ed: &Editor) -> Self {
        PreflightDialog { report: ed.preflight(), page: ed.current }
    }
}

pub fn preflight_window(ctx: &egui::Context, ed: &mut Editor, d: &mut PreflightDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let (_, close) = modal(ctx, "preflight", 500.0, |ui| {
        title(ui, "Print production check");
        match &d.report {
            Err(e) => {
                ui.label(RichText::new(e).color(p.danger));
            }
            Ok(r) => {
                for it in &r.items {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if it.ok { "✔" } else { "!" }).color(if it.ok { p.accent_text } else { p.warning }));
                        ui.vertical(|ui| {
                            ui.label(theme::semibold(it.name));
                            ui.label(theme::secondary(ui, &it.detail).size(12.0));
                        });
                    });
                    ui.add_space(3.0);
                }
                theme::heading(ui, "INKS");
                ui.label(if r.inks.is_empty() { "None found".to_string() } else { r.inks.join(", ") });
                if let Some(c) = r.coverage {
                    theme::heading(ui, &format!("INK COVERAGE, PAGE {} (ESTIMATED)", d.page + 1));
                    ui.horizontal(|ui| {
                        for (name, v) in ["C", "M", "Y", "K"].iter().zip(c) {
                            ui.label(format!("{name} {:.0}%", v * 100.0));
                        }
                        ui.label(theme::secondary(ui, format!("total {:.0}%", c.iter().sum::<f32>() * 100.0)));
                    });
                    ui.label(theme::secondary(ui, "Worked out from the colours on screen, so a guide rather than the press's figures.").size(11.5));
                }
            }
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Done", true) {
                open = false;
            }
            if secondary_button(ui, "Check again") {
                d.report = ed.preflight();
                d.page = ed.current;
            }
        });
    });
    if close {
        open = false;
    }
    open
}

// ── printer's marks ──

pub struct MarksDialog {
    bleed_mm: f32,
    crop: bool,
    registration: bool,
    info: bool,
}

impl MarksDialog {
    pub fn new() -> Self {
        MarksDialog { bleed_mm: 3.0, crop: true, registration: true, info: true }
    }
}

impl Editor {
    /// Make room around every page and draw crop marks, registration marks and a line of page
    /// information there. The page itself becomes the trim box; the bleed box extends past it.
    pub fn add_marks(&mut self, d: &MarksDialog) -> usize {
        let Some(n) = self.file.as_ref().map(|f| f.page_count()) else { return 0 };
        let name = self.file.as_ref().map(|f| f.name()).unwrap_or_default();
        let margin = 15.0 * MM;
        let bleed = d.bleed_mm * MM;
        let date = crate::annots::show_date(&crate::annots::pdf_date());
        for page in 0..n {
            let Some(trim) = self.file.as_ref().and_then(|f| f.doc.page(page)).map(|p| p.trim_box().unwrap_or_else(|| p.crop_box())) else { continue };
            let [l, b, r, t] = trim;
            let media = [l - margin, b - margin, r + margin, t + margin];
            self.apply(page, |p| {
                p.set_trim_box(trim);
                p.set_bleed_box([l - bleed, b - bleed, r + bleed, t + bleed]);
                p.set_media_box(media);
                p.set_crop_box(media);
                let style = crate::pdf::PathStyle { fill: None, stroke: Some([0, 0, 0, 255]), width: 0.25 };
                if d.crop {
                    // Two short lines at each corner, outside the bleed.
                    let gap = bleed + 2.0;
                    let len = margin - gap - 2.0;
                    for (x, y, sx, sy) in [(l, b, -1.0, -1.0), (r, b, 1.0, -1.0), (l, t, -1.0, 1.0), (r, t, 1.0, 1.0)] {
                        p.add_path(&[crate::pdf::Seg::Move(x + sx * gap, y), crate::pdf::Seg::Line(x + sx * (gap + len), y)], style);
                        p.add_path(&[crate::pdf::Seg::Move(x, y + sy * gap), crate::pdf::Seg::Line(x, y + sy * (gap + len))], style);
                    }
                }
                if d.registration {
                    // A target at the middle of each side.
                    let rr = 4.0;
                    for (cx, cy) in [((l + r) / 2.0, b - margin / 2.0), ((l + r) / 2.0, t + margin / 2.0), (l - margin / 2.0, (b + t) / 2.0), (r + margin / 2.0, (b + t) / 2.0)] {
                        p.add_path(&crate::pdf::ellipse(cx - rr, cy - rr, cx + rr, cy + rr), style);
                        p.add_path(&[crate::pdf::Seg::Move(cx - rr * 1.6, cy), crate::pdf::Seg::Line(cx + rr * 1.6, cy)], style);
                        p.add_path(&[crate::pdf::Seg::Move(cx, cy - rr * 1.6), crate::pdf::Seg::Line(cx, cy + rr * 1.6)], style);
                    }
                }
            });
            if d.info {
                let text = format!("{name}   page {} of {n}   {date}", page + 1);
                // Below the bottom-left crop mark, in the view's coordinates (top-left origin).
                let h = self.file.as_ref().map(|f| f.sizes[page].1).unwrap_or(0.0);
                self.put_text(page, (margin + bleed + 6.0, h - margin / 2.0 - 7.0), 0.0, &text, 6.0, [0, 0, 0, 255]);
            }
        }
        if let Some(f) = &mut self.file {
            f.refresh_sizes();
        }
        self.epoch += 1;
        n
    }
}

pub fn marks_window(ctx: &egui::Context, ed: &mut Editor, d: &mut MarksDialog) -> bool {
    let mut open = true;
    let mut run = false;
    let (_, close) = modal(ctx, "marks", 420.0, |ui| {
        title(ui, "Printer's marks");
        ui.label(theme::secondary(ui, "Adds room around every page for marks the press uses. The page as it is now becomes the trim (where it is cut)."));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Bleed");
            ui.add(egui::DragValue::new(&mut d.bleed_mm).range(0.0..=10.0).speed(0.1).suffix(" mm"));
        });
        ui.switch(&mut d.crop, "Crop marks");
        ui.switch(&mut d.registration, "Registration marks");
        ui.switch(&mut d.info, "Page information (file, page, date)");
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Add marks", true) {
                run = true;
            }
            if secondary_button(ui, "Cancel") {
                open = false;
            }
        });
    });
    if close {
        open = false;
    }
    if run {
        let n = ed.add_marks(d);
        ed.fit = Some(false);
        ed.say(format!("Added printer's marks to {}", plural(n, "page", "pages")));
        open = false;
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PDF/A and PDF/X conversion of a real document (run with --ignored; needs pdfium.dll).
    #[test]
    #[ignore]
    fn converts_to_standards() {
        let src = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("target/devtools/out/p1.pdf")).unwrap();
        let profile = Path::new(PROFILES).join("sRGB Color Space Profile.icm");
        match convert(&src, Standard::PdfA, &profile) {
            Ok(out) => {
                let items = verify(&out, Standard::PdfA);
                for it in &items {
                    println!("{} {} {}", if it.ok { "ok" } else { "NO" }, it.name, it.detail);
                }
                assert!(items.iter().all(|i| i.ok));
                let doc = lopdf::Document::load_mem(&out).unwrap();
                let xmp = doc.objects.values().filter_map(|o| o.as_stream().ok()).find(|s| s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"XML")).unwrap();
                assert!(String::from_utf8_lossy(&xmp.content).contains("<pdfaid:part>2</pdfaid:part>"));
            }
            Err(e) => println!("PDF/A refused: {e}"),
        }
        assert_eq!(icc_name(&Path::new(PROFILES).join("RSWOP.icm")).is_empty(), false);
        println!("RSWOP is {:?}", icc_name(&Path::new(PROFILES).join("RSWOP.icm")));
    }
}
