//! What a document says about itself and what it carries besides its pages: its properties (title,
//! author and so on), hidden information that can be removed before sharing, and attached files.

use crate::annots::{self, Update, text_string};
use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::{Editor, plural};
use crate::theme::{self, pal};
use egui::RichText;
use lopdf::{Dictionary, Object};

pub const FIELDS: [(&str, &str); 4] = [("Title", "Title"), ("Author", "Author"), ("Subject", "Subject"), ("Keywords", "Keywords")];

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// "D:20261022143000" to "2026-10-22T14:30:00".
fn iso_date(pdf: &str) -> String {
    let d = pdf.trim_start_matches("D:");
    let part = |a: usize, b: usize, def: &str| d.get(a..b).filter(|x| x.chars().all(|c| c.is_ascii_digit())).unwrap_or(def).to_string();
    format!("{}-{}-{}T{}:{}:{}", part(0, 4, "2026"), part(4, 6, "01"), part(6, 8, "01"), part(8, 10, "00"), part(10, 12, "00"), part(12, 14, "00"))
}

/// An XMP metadata packet with the document's properties (and a PDF/A claim, when given).
pub fn xmp_packet(fields: &[(&str, String)], created: &str, modified: &str, pdfa: Option<(u8, &str)>) -> String {
    let get = |k: &str| fields.iter().find(|f| f.0 == k).map(|f| xml_escape(&f.1)).unwrap_or_default();
    let mut s = String::from("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    s += "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n";
    s += "<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"";
    if pdfa.is_some() {
        s += " xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\"";
    }
    s += ">\n";
    s += "<dc:format>application/pdf</dc:format>\n";
    s += &format!("<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n", get("Title"));
    s += &format!("<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>\n", get("Author"));
    s += &format!("<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>\n", get("Subject"));
    s += &format!("<pdf:Keywords>{}</pdf:Keywords>\n", get("Keywords"));
    s += &format!("<pdf:Producer>{}</pdf:Producer>\n", get("Producer"));
    s += &format!("<xmp:CreatorTool>{}</xmp:CreatorTool>\n", get("Creator"));
    s += &format!("<xmp:CreateDate>{}</xmp:CreateDate>\n<xmp:ModifyDate>{}</xmp:ModifyDate>\n", iso_date(created), iso_date(modified));
    if let Some((part, conf)) = pdfa {
        s += &format!("<pdfaid:part>{part}</pdfaid:part>\n<pdfaid:conformance>{conf}</pdfaid:conformance>\n");
    }
    s += "</rdf:Description>\n</rdf:RDF></x:xmpmeta>\n";
    // Room to grow in place, as the specification suggests.
    s += &" ".repeat(1024);
    s += "\n<?xpacket end=\"w\"?>";
    s
}

fn root(u: &Update) -> Result<(lopdf::ObjectId, Dictionary), String> {
    let id = u.doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|_| "The document catalog is missing.".to_string())?;
    Ok((id, u.dict(id)?))
}

impl Update {
    /// Set the document's properties, in its information dictionary and (when it has one, or `pdfa`
    /// asks for one) its XMP metadata, which most readers prefer.
    pub fn set_properties(&mut self, fields: &[(&str, String)], pdfa: Option<(u8, &str)>) -> Result<(), String> {
        let now = annots::pdf_date();
        let mut info = match self.doc.trailer.get(b"Info") {
            Ok(Object::Reference(id)) => self.dict(*id).unwrap_or_default(),
            Ok(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        };
        for (k, v) in fields {
            if v.trim().is_empty() {
                info.remove(k.as_bytes());
            } else {
                info.set(*k, text_string(v.trim()));
            }
        }
        info.set("ModDate", text_string(&now));
        if !info.has(b"CreationDate") {
            info.set("CreationDate", text_string(&now));
        }
        let created = info.get(b"CreationDate").map(annots::read_text).unwrap_or_else(|_| now.clone());
        let mut all: Vec<(&str, String)> = fields.to_vec();
        for k in ["Producer", "Creator"] {
            if !all.iter().any(|f| f.0 == k) {
                all.push((k, info.get(k.as_bytes()).map(annots::read_text).unwrap_or_default()));
            }
        }
        let info_id = self.add(Object::Dictionary(info));
        self.doc.trailer.set("Info", Object::Reference(info_id));
        let (root_id, mut cat) = root(self)?;
        if cat.has(b"Metadata") || pdfa.is_some() {
            let xmp = xmp_packet(&all, &created, &now, pdfa);
            let mut d = Dictionary::new();
            d.set("Type", Object::Name(b"Metadata".to_vec()));
            d.set("Subtype", Object::Name(b"XML".to_vec()));
            // Left uncompressed so any tool can read it.
            let id = self.add(Object::Stream(lopdf::Stream::new(d, xmp.into_bytes())));
            cat.set("Metadata", Object::Reference(id));
            self.put(root_id, Object::Dictionary(cat));
        }
        Ok(())
    }
}

// ── properties ──

pub struct PropertiesDialog {
    values: Vec<String>,
    created: String,
    modified: String,
    producer: String,
    creator: String,
    error: Option<String>,
}

impl PropertiesDialog {
    pub fn new(ed: &Editor) -> Self {
        let doc = ed.file.as_ref().map(|f| &f.doc);
        let get = |k: &str| doc.map(|d| d.meta(k)).unwrap_or_default();
        PropertiesDialog {
            values: FIELDS.iter().map(|(k, _)| get(k)).collect(),
            created: annots::show_date(&get("CreationDate")),
            modified: annots::show_date(&get("ModDate")),
            producer: get("Producer"),
            creator: get("Creator"),
            error: None,
        }
    }
}

pub fn properties_window(ctx: &egui::Context, ed: &mut Editor, d: &mut PropertiesDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut save = false;
    let (_, close) = modal(ctx, "doc-properties", 440.0, |ui| {
        title(ui, "Document properties");
        ui.label(theme::secondary(ui, "What the document says about itself. Search tools and file browsers show these."));
        ui.add_space(6.0);
        for (k, (_, label)) in FIELDS.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add_sized([80.0, 20.0], egui::Label::new(*label));
                ui.add(egui::TextEdit::singleline(&mut d.values[k]).desired_width(f32::INFINITY));
            });
        }
        ui.add_space(6.0);
        for (label, v) in [("Created", &d.created), ("Changed", &d.modified), ("Made with", &d.creator), ("PDF from", &d.producer)] {
            if !v.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_sized([80.0, 18.0], egui::Label::new(theme::secondary(ui, label)));
                    ui.label(theme::secondary(ui, v.as_str()));
                });
            }
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Save", true) {
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
        let fields: Vec<(&str, String)> = FIELDS.iter().zip(&d.values).map(|((k, _), v)| (*k, v.clone())).collect();
        if ed.file_update(&[], |u| u.set_properties(&fields, None)).is_some() {
            ed.say("Document properties updated");
            open = false;
        }
    }
    open
}

// ── removing hidden information ──

#[derive(Clone, Copy)]
pub struct Sanitize {
    pub metadata: bool,
    pub attachments: bool,
    pub scripts: bool,
    pub comments: bool,
    pub private_data: bool,
}

impl Default for Sanitize {
    fn default() -> Self {
        Sanitize { metadata: true, attachments: true, scripts: true, comments: false, private_data: true }
    }
}

/// What was removed, counted.
#[derive(Default)]
pub struct Removed {
    pub items: Vec<String>,
}

fn resolve_dict(doc: &lopdf::Document, o: &Object) -> Option<Dictionary> {
    match o {
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        Object::Dictionary(d) => Some(d.clone()),
        _ => None,
    }
}

/// Remove hidden information from a plain (unencrypted) PDF and rewrite it without anything left
/// over from earlier versions. Returns the new file and what went.
pub fn sanitize_bytes(bytes: &[u8], s: Sanitize) -> Result<(Vec<u8>, Removed), String> {
    let mut doc = lopdf::Document::load_mem(bytes).map_err(|e| format!("The file could not be read for cleaning ({e})."))?;
    let mut removed = Removed::default();
    let root_id = doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|_| "The document catalog is missing.".to_string())?;
    let mut cat = doc.get_dictionary(root_id).map_err(|_| "The document catalog is missing.")?.clone();
    if s.metadata {
        let had = doc.trailer.remove(b"Info").is_some() | cat.remove(b"Metadata").is_some();
        if had {
            removed.items.push("document properties and metadata".into());
        }
    }
    let mut names = cat.get(b"Names").ok().and_then(|o| resolve_dict(&doc, o));
    if s.attachments
        && let Some(n) = &mut names
        && n.remove(b"EmbeddedFiles").is_some()
    {
        removed.items.push("attached files".into());
    }
    if s.scripts {
        let mut any = false;
        if let Some(n) = &mut names {
            any |= n.remove(b"JavaScript").is_some();
        }
        any |= cat.remove(b"AA").is_some();
        if let Ok(oa) = cat.get(b"OpenAction") {
            // An opening action that runs a script or another program goes; a plain "go to page" stays.
            let kind = resolve_dict(&doc, oa).and_then(|d| d.get(b"S").ok().and_then(|x| x.as_name().ok()).map(|n| n.to_vec()));
            if matches!(kind.as_deref(), Some(b"JavaScript" | b"Launch" | b"ImportData" | b"SubmitForm")) {
                cat.remove(b"OpenAction");
                any = true;
            }
        }
        if any {
            removed.items.push("scripts and automatic actions".into());
        }
    }
    if let Some(n) = names {
        cat.set("Names", Object::Dictionary(n));
    }
    if s.private_data {
        let mut any = cat.remove(b"PieceInfo").is_some();
        for (_, id) in doc.get_pages() {
            if let Ok(page) = doc.get_dictionary_mut(id) {
                any |= page.remove(b"PieceInfo").is_some();
                any |= page.remove(b"Thumb").is_some();
            }
        }
        if any {
            removed.items.push("private application data and page thumbnails".into());
        }
    }
    // Page by page: actions on pages and annotations, and (if asked) comments.
    let mut comments = 0;
    let mut actions = 0;
    let pages: Vec<lopdf::ObjectId> = doc.get_pages().values().copied().collect();
    for pid in pages {
        let Ok(page) = doc.get_dictionary(pid).cloned() else { continue };
        let mut page = page;
        if s.scripts && page.remove(b"AA").is_some() {
            actions += 1;
        }
        let list = match page.get(b"Annots") {
            Ok(Object::Reference(r)) => doc.get_object(*r).and_then(Object::as_array).cloned().unwrap_or_default(),
            Ok(Object::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let mut keep = Vec::new();
        for entry in list {
            let Some(mut d) = resolve_dict(&doc, &entry) else { continue };
            let sub = d.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"").to_vec();
            if s.comments && !matches!(sub.as_slice(), b"Link" | b"Widget") {
                comments += 1;
                continue;
            }
            if s.attachments && sub == b"FileAttachment" {
                comments += 1;
                continue;
            }
            if s.scripts {
                let mut changed = d.remove(b"AA").is_some();
                if let Some(a) = d.get(b"A").ok().and_then(|o| resolve_dict(&doc, o))
                    && matches!(a.get(b"S").and_then(Object::as_name).ok(), Some(b"JavaScript" | b"Launch" | b"ImportData" | b"SubmitForm"))
                {
                    d.remove(b"A");
                    changed = true;
                }
                if changed {
                    actions += 1;
                    match entry {
                        Object::Reference(id) => {
                            doc.objects.insert(id, Object::Dictionary(d));
                            keep.push(entry);
                        }
                        _ => keep.push(Object::Dictionary(d)),
                    }
                    continue;
                }
            }
            keep.push(entry);
        }
        if keep.is_empty() {
            page.remove(b"Annots");
        } else {
            page.set("Annots", Object::Array(keep));
        }
        doc.objects.insert(pid, Object::Dictionary(page));
    }
    if comments > 0 {
        removed.items.push(plural(comments, "comment", "comments"));
    }
    if actions > 0 && !removed.items.iter().any(|i| i.starts_with("scripts")) {
        removed.items.push("scripts and automatic actions".into());
    }
    doc.objects.insert(root_id, Object::Dictionary(cat));
    doc.prune_objects();
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| format!("The cleaned file could not be written ({e})."))?;
    Ok((out, removed))
}

impl Editor {
    pub fn sanitize(&mut self, s: Sanitize) -> Result<String, String> {
        self.commit_edits();
        self.commit();
        let f = self.file.as_ref().ok_or("No document is open.")?;
        let (bytes, removed) = sanitize_bytes(&f.doc.save_plain()?, s)?;
        let doc = crate::pdf::Doc::open(bytes, None).map_err(|_| "The cleaned file could not be read back.".to_string())?;
        let f = self.file.as_mut().unwrap();
        f.replace_doc(doc);
        self.clear_selection();
        self.epoch += 1;
        self.needs_commit = true;
        let what = if removed.items.is_empty() { "nothing hidden was found".to_string() } else { format!("removed {}", removed.items.join(", ")) };
        Ok(format!("Cleaned: {what}. Earlier versions inside the file are gone too."))
    }
}

pub struct SanitizeDialog {
    opts: Sanitize,
    error: Option<String>,
}

impl SanitizeDialog {
    pub fn new() -> Self {
        SanitizeDialog { opts: Sanitize::default(), error: None }
    }
}

pub fn sanitize_window(ctx: &egui::Context, ed: &mut Editor, d: &mut SanitizeDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut run = false;
    let (_, close) = modal(ctx, "sanitize", 440.0, |ui| {
        title(ui, "Remove hidden information");
        ui.label(theme::secondary(ui, "Before sharing a document, take out what readers do not see. Earlier versions kept inside the file are always removed."));
        ui.add_space(6.0);
        let o = &mut d.opts;
        ui.checkbox(&mut o.metadata, "Document properties and metadata (author, titles, the program that made it)");
        ui.checkbox(&mut o.attachments, "Attached files");
        ui.checkbox(&mut o.scripts, "Scripts and actions that run on their own");
        ui.checkbox(&mut o.comments, "Comments and marks (links and form fields stay)");
        ui.checkbox(&mut o.private_data, "Private data other programs left, and page thumbnails");
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Remove", true) {
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
        match ed.sanitize(d.opts) {
            Ok(msg) => {
                ed.say(msg);
                open = false;
            }
            Err(e) => d.error = Some(e),
        }
    }
    open
}

// ── attachments ──

pub struct AttachmentsDialog {
    error: Option<String>,
}

impl AttachmentsDialog {
    pub fn new() -> Self {
        AttachmentsDialog { error: None }
    }
}

fn size_text(n: usize) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

pub fn attachments_window(ctx: &egui::Context, ed: &mut Editor, d: &mut AttachmentsDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let list = ed.file.as_ref().map(|f| f.doc.attachments()).unwrap_or_default();
    let mut save = None;
    let mut delete = None;
    let mut add = false;
    let (_, close) = modal(ctx, "attachments", 440.0, |ui| {
        title(ui, "Attached files");
        ui.label(theme::secondary(ui, "Files carried inside the PDF. They travel with it and can be saved out again."));
        ui.add_space(6.0);
        if list.is_empty() {
            ui.label(theme::secondary(ui, "No files are attached."));
        }
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            for (k, (name, size)) in list.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(name);
                    ui.label(theme::secondary(ui, size_text(*size)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(RichText::new("Remove").color(p.danger)).clicked() {
                            delete = Some(k);
                        }
                        if ui.button("Save as…").clicked() {
                            save = Some(k);
                        }
                    });
                });
            }
        });
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Done", true) {
                open = false;
            }
            if secondary_button(ui, "Attach files…") {
                add = true;
            }
        });
    });
    if close {
        open = false;
    }
    if let Some(k) = save
        && let Some((name, _)) = list.get(k)
        && let Some(to) = rfd::FileDialog::new().set_file_name(name).save_file()
    {
        match ed.file.as_ref().and_then(|f| f.doc.attachment_data(k)) {
            Some(bytes) => match std::fs::write(&to, bytes) {
                Ok(()) => ed.say(format!("Saved {}", to.display())),
                Err(e) => d.error = Some(format!("{} could not be written ({e}).", to.display())),
            },
            None => d.error = Some("That file could not be read from the document.".into()),
        }
    }
    if let Some(k) = delete {
        ed.apply_doc(|f| f.doc.delete_attachment(k));
        ed.say("Attachment removed");
    }
    if add && let Some(files) = rfd::FileDialog::new().pick_files() {
        let mut n = 0;
        for path in files {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            match std::fs::read(&path) {
                Ok(bytes) if ed.apply_doc(|f| f.doc.add_attachment(&name, &bytes)) == Some(true) => n += 1,
                _ => d.error = Some(format!("{} could not be attached.", path.display())),
            }
        }
        if n > 0 {
            ed.say(format!("Attached {}", plural(n, "file", "files")));
        }
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_has_the_fields() {
        let x = xmp_packet(&[("Title", "A & B".into()), ("Author", "Me".into())], "D:20260101000000", "D:20261022101500", Some((2, "B")));
        assert!(x.contains("A &amp; B") && x.contains("<rdf:li>Me</rdf:li>"));
        assert!(x.contains("2026-10-22T10:15:00") && x.contains("<pdfaid:part>2</pdfaid:part>"));
    }
}
