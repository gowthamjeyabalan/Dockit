//! Whole-page tools: crop and resize, page numbers / headers / footers / watermarks, compress,
//! export pages as images and make a PDF from images. Each has its window here.
//!
//! None of these rewrite existing page content: crop and size change the page's boxes, resizing
//! wraps the content in a matrix (PDFium's page transform), stamps are new text objects in a
//! separate stream, and compressing re-encodes image streams and drops unused objects.

use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::{Editor, parse_ranges, plural};
use crate::pdf::{Doc, ImageData, Kind};
use crate::theme::{self, UiExt, pal};
use egui::{RichText, Ui, vec2};
use std::path::PathBuf;

const MM: f32 = 72.0 / 25.4;

/// Which pages a tool works on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Selected,
    Range,
}

fn scope_ui(ui: &mut Ui, scope: &mut Scope, range: &mut String, ed: &Editor) {
    let n = ed.selected_pages().len();
    ui.segmented(scope, &[(Scope::All, "All pages"), (Scope::Selected, if n > 1 { "Selected pages" } else { "This page" }), (Scope::Range, "Pages…")]);
    if *scope == Scope::Range {
        ui.add(egui::TextEdit::singleline(range).desired_width(f32::INFINITY).hint_text("For example 1-3, 8"));
    }
}

fn scope_pages(ed: &Editor, scope: Scope, range: &str) -> Result<Vec<usize>, String> {
    let n = ed.file.as_ref().map_or(0, |f| f.page_count());
    match scope {
        Scope::All => Ok((0..n).collect()),
        Scope::Selected => Ok(ed.selected_pages()),
        Scope::Range => {
            let mut v: Vec<usize> = parse_ranges(range, n)?.into_iter().flatten().collect();
            v.sort_unstable();
            v.dedup();
            Ok(v)
        }
    }
}

// ── crop and resize ──

#[derive(Clone, Copy, PartialEq, Eq)]
enum CropMode {
    Margins,
    Auto,
    Size,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Paper {
    A3,
    A4,
    A5,
    Letter,
    Legal,
}

impl Paper {
    /// Portrait size in points.
    fn size(self) -> (f32, f32) {
        match self {
            Paper::A3 => (297.0 * MM, 420.0 * MM),
            Paper::A4 => (210.0 * MM, 297.0 * MM),
            Paper::A5 => (148.0 * MM, 210.0 * MM),
            Paper::Letter => (612.0, 792.0),
            Paper::Legal => (612.0, 1008.0),
        }
    }
}

pub struct CropDialog {
    mode: CropMode,
    /// Top, right, bottom, left in millimetres, as the page is seen.
    margins: [f32; 4],
    paper: Paper,
    landscape: bool,
    scope: Scope,
    range: String,
    error: Option<String>,
}

impl CropDialog {
    pub fn new() -> Self {
        CropDialog { mode: CropMode::Margins, margins: [10.0; 4], paper: Paper::A4, landscape: false, scope: Scope::All, range: String::new(), error: None }
    }
}

impl Editor {
    /// Trim `margins` (top, right, bottom, left in points, as seen) from the visible area.
    pub fn crop_pages(&mut self, pages: &[usize], margins: [f32; 4]) {
        for &i in pages {
            let Some(g) = self.file.as_ref().and_then(|f| f.geom(i)) else { continue };
            let h = self.file.as_ref().map_or(0.0, |f| f.sizes[i].1);
            let [t, r, b, l] = margins;
            if l + r >= g.w - 10.0 || t + b >= h - 10.0 {
                self.error = Some("The margins are larger than the page.".into());
                return;
            }
            let a = g.to_user(l, t);
            let c = g.to_user(g.w - r, h - b);
            let rect = [a.0.min(c.0), a.1.min(c.1), a.0.max(c.0), a.1.max(c.1)];
            self.apply(i, |p| p.set_crop_box(rect));
        }
        self.relayout_pages();
        self.say(format!("Cropped {}", plural(pages.len(), "page", "pages")));
    }

    /// Crop each page to what is drawn on it, with a small border.
    pub fn crop_to_content(&mut self, pages: &[usize]) {
        let mut done = 0;
        for &i in pages {
            let Some(f) = &self.file else { return };
            let objs = f.objects(i);
            let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            let Some(p) = f.doc.page(i) else { continue };
            let page_box = p.crop_box();
            drop(p);
            let page_area = (page_box[2] - page_box[0]) * (page_box[3] - page_box[1]);
            for o in objs.iter() {
                let (mut l, mut bo, mut r, mut t) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for &(x, y) in &o.quad {
                    l = l.min(x);
                    bo = bo.min(y);
                    r = r.max(x);
                    t = t.max(y);
                }
                // Page-sized backgrounds say nothing about where the content is.
                if (r - l) * (t - bo) > page_area * 0.9 && o.kind != Kind::Text {
                    continue;
                }
                b = [b[0].min(l), b[1].min(bo), b[2].max(r), b[3].max(t)];
            }
            if b[0] > b[2] {
                continue;
            }
            let pad = 12.0;
            let rect = [(b[0] - pad).max(page_box[0]), (b[1] - pad).max(page_box[1]), (b[2] + pad).min(page_box[2]), (b[3] + pad).min(page_box[3])];
            self.apply(i, |p| p.set_crop_box(rect));
            done += 1;
        }
        self.relayout_pages();
        self.say(format!("Cropped {} to their content", plural(done, "page", "pages")));
    }

    /// Give pages a new paper size (points, as seen), scaling the content to fit and centring it.
    pub fn resize_pages(&mut self, pages: &[usize], size: (f32, f32)) {
        for &i in pages {
            let Some(f) = &self.file else { return };
            let Some(p) = f.doc.page(i) else { continue };
            let media = p.media_box();
            let crop = p.crop_box();
            let g = p.geom();
            drop(p);
            // The page's own axes: swap the target for pages turned a quarter.
            let turned = {
                let o = g.to_view(0.0, 0.0);
                let e = g.to_view(1.0, 0.0);
                (e.1 - o.1).abs() > (e.0 - o.0).abs()
            };
            let (tw, th) = if turned { (size.1, size.0) } else { size };
            let (cw, ch) = (crop[2] - crop[0], crop[3] - crop[1]);
            let s = (tw / cw).min(th / ch);
            let dx = (tw - cw * s) / 2.0 - crop[0] * s;
            let dy = (th - ch * s) / 2.0 - crop[1] * s;
            let _ = media;
            self.apply(i, |p| {
                p.transform_content([s, 0.0, 0.0, s, dx, dy]);
                p.set_media_box([0.0, 0.0, tw, th]);
                p.set_crop_box([0.0, 0.0, tw, th]);
            });
        }
        self.relayout_pages();
        self.say(format!("Resized {}", plural(pages.len(), "page", "pages")));
    }

    /// Page sizes changed: lay the pages out again.
    fn relayout_pages(&mut self) {
        let epoch_bump = self.file.is_some();
        if let Some(f) = &mut self.file {
            f.refresh_sizes();
        }
        if epoch_bump {
            self.epoch += 1;
        }
    }
}

pub fn crop_window(ctx: &egui::Context, ed: &mut Editor, d: &mut CropDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut run = false;
    let (_, close) = modal(ctx, "crop", 420.0, |ui| {
        title(ui, "Crop or resize pages");
        ui.add_space(6.0);
        ui.segmented(&mut d.mode, &[(CropMode::Margins, "Trim margins"), (CropMode::Auto, "Fit to content"), (CropMode::Size, "Paper size")]);
        ui.add_space(8.0);
        match d.mode {
            CropMode::Margins => {
                egui::Grid::new("crop-margins").num_columns(4).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
                    for (k, label) in ["Top", "Right", "Bottom", "Left"].iter().enumerate() {
                        ui.label(theme::secondary(ui, *label));
                        ui.add(egui::DragValue::new(&mut d.margins[k]).range(0.0..=500.0).speed(0.5).suffix(" mm"));
                        if k % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
                ui.label(theme::secondary(ui, "Only the visible area changes; nothing on the page is removed."));
            }
            CropMode::Auto => {
                ui.label(theme::secondary(ui, "Each page is cropped to what is drawn on it, with a small border."));
            }
            CropMode::Size => {
                egui::ComboBox::from_id_salt("paper").selected_text(match d.paper {
                    Paper::A3 => "A3",
                    Paper::A4 => "A4",
                    Paper::A5 => "A5",
                    Paper::Letter => "Letter",
                    Paper::Legal => "Legal",
                })
                .show_ui(ui, |ui| {
                    for (v, n) in [(Paper::A3, "A3"), (Paper::A4, "A4"), (Paper::A5, "A5"), (Paper::Letter, "Letter"), (Paper::Legal, "Legal")] {
                        ui.selectable_value(&mut d.paper, v, n);
                    }
                });
                ui.switch(&mut d.landscape, "Landscape");
                ui.label(theme::secondary(ui, "The content is scaled to fit the new size and centred. Comments and form fields keep their place."));
            }
        }
        theme::heading(ui, "PAGES");
        scope_ui(ui, &mut d.scope, &mut d.range, ed);
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Apply", true) {
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
        match scope_pages(ed, d.scope, &d.range) {
            Ok(pages) => {
                match d.mode {
                    CropMode::Margins => ed.crop_pages(&pages, d.margins.map(|v| v * MM)),
                    CropMode::Auto => ed.crop_to_content(&pages),
                    CropMode::Size => {
                        let (w, h) = d.paper.size();
                        ed.resize_pages(&pages, if d.landscape { (h, w) } else { (w, h) });
                    }
                }
                open = false;
            }
            Err(e) => d.error = Some(e),
        }
    }
    open
}

// ── headers, footers, page numbers and watermarks ──

#[derive(Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

pub struct StampDialog {
    header: String,
    header_align: Align,
    footer: String,
    footer_align: Align,
    size: f32,
    margin_mm: f32,
    color: crate::pdf::Rgba,
    start: usize,
    watermark: String,
    watermark_size: f32,
    opacity: f32,
    diagonal: bool,
    scope: Scope,
    range: String,
    error: Option<String>,
}

impl StampDialog {
    pub fn new() -> Self {
        StampDialog {
            header: String::new(),
            header_align: Align::Right,
            footer: "Page {page} of {pages}".into(),
            footer_align: Align::Center,
            size: 9.0,
            margin_mm: 10.0,
            color: [60, 60, 66, 255],
            start: 1,
            watermark: String::new(),
            watermark_size: 72.0,
            opacity: 0.15,
            diagonal: true,
            scope: Scope::All,
            range: String::new(),
            error: None,
        }
    }
}

/// Fill in {page}, {pages}, {date} and {file}.
fn expand(template: &str, page: usize, pages: usize, file: &str) -> String {
    let date = {
        // Days since 1970 to a calendar date (UTC), avoiding a date crate.
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
        let z = secs / 86400 + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}")
    };
    template.replace("{page}", &page.to_string()).replace("{pages}", &pages.to_string()).replace("{date}", &date).replace("{file}", file)
}

impl Editor {
    /// Put a line of text on page `i` with its baseline starting at view position `at`, turned by
    /// `degrees` (counter-clockwise as seen). Returns false if no font could be loaded.
    fn put_text(&mut self, i: usize, at: (f32, f32), degrees: f32, text: &str, size: f32, color: crate::pdf::Rgba) -> bool {
        let Some(f) = &self.file else { return false };
        let Some(g) = f.geom(i) else { return false };
        let latin = text.chars().all(|c| (c as u32) < 0x100);
        let font = if latin {
            Some(f.doc.standard_font("Helvetica"))
        } else {
            crate::fonts::generic("sans", false, false).and_then(|p| f.doc.truetype_font(p))
        };
        let Some(font) = font.filter(|f| !f.is_null()) else { return false };
        let o = g.to_user(0.0, 0.0);
        let rx = g.to_user(1.0, 0.0);
        let dy = g.to_user(0.0, 1.0);
        let unit = |v: (f32, f32)| {
            let l = (v.0 * v.0 + v.1 * v.1).sqrt().max(1e-6);
            (v.0 / l, v.1 / l)
        };
        let right = unit((rx.0 - o.0, rx.1 - o.1));
        let up = unit((o.0 - dy.0, o.1 - dy.1));
        let (s, c) = degrees.to_radians().sin_cos();
        let r2 = (right.0 * c + up.0 * s, right.1 * c + up.1 * s);
        let u2 = (up.0 * c - right.0 * s, up.1 * c - right.1 * s);
        let pos = g.to_user(at.0, at.1);
        let size_user = size / g.unit().max(1e-3);
        self.apply(i, |p| p.add_text(font, size_user, text, [r2.0, r2.1, u2.0, u2.1, pos.0, pos.1], color));
        true
    }

    fn line_width(&self, text: &str, size: f32) -> f32 {
        let Some(f) = &self.file else { return 0.0 };
        let latin = text.chars().all(|c| (c as u32) < 0x100);
        let font = if latin { f.doc.standard_font("Helvetica") } else { crate::fonts::generic("sans", false, false).and_then(|p| f.doc.truetype_font(p)).unwrap_or(std::ptr::null_mut()) };
        Doc::font_text_width(font, text, size)
    }

    fn stamp_pages(&mut self, d: &StampDialog, pages: &[usize]) {
        let Some(f) = &self.file else { return };
        let total = f.page_count();
        let name = f.name();
        let margin = d.margin_mm * MM;
        for (k, &i) in pages.iter().enumerate() {
            let Some(f) = &self.file else { return };
            let (w, h) = f.sizes[i];
            let number = d.start + k;
            for (template, align, top) in [(&d.header, d.header_align, true), (&d.footer, d.footer_align, false)] {
                if template.trim().is_empty() {
                    continue;
                }
                let text = expand(template, number, d.start + pages.len() - 1, &name);
                let tw = self.line_width(&text, d.size);
                let x = match align {
                    Align::Left => margin,
                    Align::Center => (w - tw) / 2.0,
                    Align::Right => w - margin - tw,
                };
                let y = if top { margin + d.size * 0.8 } else { h - margin };
                if !self.put_text(i, (x, y), 0.0, &text, d.size, d.color) {
                    self.error = Some("No font was found for the header or footer.".into());
                    return;
                }
            }
            if !d.watermark.trim().is_empty() {
                let text = expand(&d.watermark, number, total, &name);
                let tw = self.line_width(&text, d.watermark_size);
                let deg = if d.diagonal { (h / w).atan().to_degrees() } else { 0.0 };
                let (s, c) = deg.to_radians().sin_cos();
                // Centre the text's middle on the page's middle.
                let (cx, cy) = (w / 2.0, h / 2.0);
                let half = (tw / 2.0, d.watermark_size * 0.35);
                let at = (cx - half.0 * c - half.1 * s, cy + half.0 * s - half.1 * c);
                let alpha = (d.opacity.clamp(0.02, 1.0) * 255.0) as u8;
                let color = [d.color[0], d.color[1], d.color[2], alpha];
                self.put_text(i, at, deg, &text, d.watermark_size, color);
            }
        }
        self.say(format!("Added to {}", plural(pages.len(), "page", "pages")));
    }
}

fn align_ui(ui: &mut Ui, a: &mut Align) {
    ui.segmented(a, &[(Align::Left, "Left"), (Align::Center, "Centre"), (Align::Right, "Right")]);
}

pub fn stamp_window(ctx: &egui::Context, ed: &mut Editor, d: &mut StampDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut run = false;
    let (_, close) = modal(ctx, "stamp-pages", 440.0, |ui| {
        title(ui, "Page numbers, headers and watermark");
        ui.label(theme::secondary(ui, "Use {page}, {pages}, {date} and {file} in the text."));
        theme::heading(ui, "HEADER");
        ui.add(egui::TextEdit::singleline(&mut d.header).desired_width(f32::INFINITY).hint_text("None"));
        align_ui(ui, &mut d.header_align);
        theme::heading(ui, "FOOTER");
        ui.add(egui::TextEdit::singleline(&mut d.footer).desired_width(f32::INFINITY).hint_text("None"));
        align_ui(ui, &mut d.footer_align);
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add(egui::DragValue::new(&mut d.size).range(5.0..=40.0).speed(0.2).suffix(" pt"));
            ui.label("Margin");
            ui.add(egui::DragValue::new(&mut d.margin_mm).range(2.0..=60.0).speed(0.2).suffix(" mm"));
            ui.label("First number");
            ui.add(egui::DragValue::new(&mut d.start).range(0..=100000).speed(0.2));
        });
        theme::heading(ui, "WATERMARK");
        ui.add(egui::TextEdit::singleline(&mut d.watermark).desired_width(f32::INFINITY).hint_text("For example DRAFT"));
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add(egui::DragValue::new(&mut d.watermark_size).range(12.0..=300.0).speed(0.5).suffix(" pt"));
            ui.label("Opacity");
            let mut pct = d.opacity * 100.0;
            if ui.add(egui::DragValue::new(&mut pct).range(2.0..=100.0).speed(0.5).suffix("%")).changed() {
                d.opacity = pct / 100.0;
            }
        });
        ui.switch(&mut d.diagonal, "Across the page diagonally");
        theme::heading(ui, "COLOUR");
        crate::canvas::color_row(ui, &mut d.color);
        theme::heading(ui, "PAGES");
        scope_ui(ui, &mut d.scope, &mut d.range, ed);
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Add", !(d.header.trim().is_empty() && d.footer.trim().is_empty() && d.watermark.trim().is_empty())) {
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
        match scope_pages(ed, d.scope, &d.range) {
            Ok(pages) => {
                let spec = std::mem::replace(d, StampDialog::new());
                ed.stamp_pages(&spec, &pages);
                *d = spec;
                open = false;
            }
            Err(e) => d.error = Some(e),
        }
    }
    open
}

// ── compress ──

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    High,
    Medium,
    Small,
}

impl Quality {
    /// Largest image resolution kept (pixels per inch of the largest page) and JPEG quality.
    fn limits(self) -> (f32, u8) {
        match self {
            Quality::High => (220.0, 85),
            Quality::Medium => (150.0, 72),
            Quality::Small => (96.0, 55),
        }
    }
}

pub struct CompressDialog {
    quality: Quality,
    result: Option<String>,
}

impl CompressDialog {
    pub fn new() -> Self {
        CompressDialog { quality: Quality::Medium, result: None }
    }
}

/// Re-encode large images and drop unused objects. Returns the new file bytes.
fn compress_bytes(bytes: &[u8], quality: Quality, page_inches: f32) -> Result<Vec<u8>, String> {
    use lopdf::Object;
    let mut doc = lopdf::Document::load_mem(bytes).map_err(|e| format!("The file structure could not be read ({e})."))?;
    if doc.is_encrypted() {
        return Err("Password-protected files cannot be compressed.".into());
    }
    let (ppi, jpeg_q) = quality.limits();
    let max_px = (page_inches * ppi) as u32;
    let ids: Vec<lopdf::ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        let Some(Object::Stream(s)) = doc.objects.get(&id) else { continue };
        let d = &s.dict;
        let is_image = d.get(b"Subtype").and_then(Object::as_name).is_ok_and(|n| n == b"Image");
        if !is_image || d.has(b"ImageMask") || d.has(b"SMask") || d.has(b"Mask") || d.has(b"Decode") {
            continue;
        }
        let bpc = d.get(b"BitsPerComponent").and_then(Object::as_i64).unwrap_or(0);
        let w = d.get(b"Width").and_then(Object::as_i64).unwrap_or(0) as u32;
        let h = d.get(b"Height").and_then(Object::as_i64).unwrap_or(0) as u32;
        let cs = d.get(b"ColorSpace").ok().and_then(|o| o.as_name().ok()).map(|n| n.to_vec());
        let channels = match cs.as_deref() {
            Some(b"DeviceRGB") => 3,
            Some(b"DeviceGray") => 1,
            _ => continue,
        };
        if bpc != 8 || w == 0 || h == 0 {
            continue;
        }
        let filter = d.get(b"Filter").ok().and_then(|o| o.as_name().ok()).map(|n| n.to_vec());
        let img: Option<image::DynamicImage> = match filter.as_deref() {
            Some(b"DCTDecode") => image::load_from_memory_with_format(&s.content, image::ImageFormat::Jpeg).ok(),
            Some(b"FlateDecode") | None => {
                let raw = if filter.is_some() { s.decompressed_content().ok() } else { Some(s.content.clone()) };
                raw.and_then(|raw| {
                    if raw.len() < (w * h * channels) as usize {
                        return None;
                    }
                    if channels == 3 {
                        image::RgbImage::from_raw(w, h, raw[..(w * h * 3) as usize].to_vec()).map(image::DynamicImage::ImageRgb8)
                    } else {
                        image::GrayImage::from_raw(w, h, raw[..(w * h) as usize].to_vec()).map(image::DynamicImage::ImageLuma8)
                    }
                })
            }
            _ => None,
        };
        let Some(mut img) = img else { continue };
        if w.max(h) > max_px {
            img = img.resize(max_px, max_px, image::imageops::FilterType::Lanczos3);
        }
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, jpeg_q);
        let ok = if channels == 3 { img.to_rgb8().write_with_encoder(enc).is_ok() } else { img.to_luma8().write_with_encoder(enc).is_ok() };
        if !ok || out.len() >= s.content.len() {
            continue;
        }
        let (nw, nh) = (img.width(), img.height());
        if let Some(Object::Stream(s)) = doc.objects.get_mut(&id) {
            s.dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
            s.dict.remove(b"DecodeParms");
            s.dict.set("Width", Object::Integer(nw as i64));
            s.dict.set("Height", Object::Integer(nh as i64));
            s.set_content(out);
            s.allows_compression = false;
        }
    }
    doc.prune_objects();
    doc.compress();
    let mut out = Vec::new();
    let opts = lopdf::SaveOptions::builder().use_object_streams(true).use_xref_streams(true).build();
    doc.save_with_options(&mut out, opts).map_err(|e| format!("The file could not be written ({e})."))?;
    Ok(out)
}

impl Editor {
    pub fn compress(&mut self, quality: Quality) -> Result<String, String> {
        self.commit_edits();
        self.commit();
        let f = self.file.as_ref().ok_or("No document is open.")?;
        let before = f.doc.save()?;
        let inches = f.sizes.iter().map(|s| s.0.max(s.1)).fold(0.0, f32::max) / 72.0;
        let after = compress_bytes(&before, quality, inches.max(1.0))?;
        if after.len() >= before.len() {
            return Ok("The file is already as small as these settings make it.".into());
        }
        let doc = Doc::open(after.clone(), None).map_err(|_| "The compressed file could not be read back, so nothing was changed.".to_string())?;
        let f = self.file.as_mut().unwrap();
        f.replace_doc(doc);
        self.epoch += 1;
        self.needs_commit = true;
        let kb = |n: usize| if n > 1 << 20 { format!("{:.1} MB", n as f64 / 1048576.0) } else { format!("{} KB", n / 1024) };
        Ok(format!("{} → {} ({}% smaller)", kb(before.len()), kb(after.len()), 100 - after.len() * 100 / before.len()))
    }
}

pub fn compress_window(ctx: &egui::Context, ed: &mut Editor, d: &mut CompressDialog) -> bool {
    let mut open = true;
    let mut run = false;
    let (_, close) = modal(ctx, "compress", 400.0, |ui| {
        title(ui, "Compress");
        ui.label(theme::secondary(ui, "Large pictures are stored at a lower resolution and unused parts of the file are removed. Text and drawings are not changed."));
        ui.add_space(8.0);
        ui.segmented(&mut d.quality, &[(Quality::High, "High quality"), (Quality::Medium, "Balanced"), (Quality::Small, "Smallest")]);
        if let Some(r) = &d.result {
            ui.add_space(6.0);
            ui.label(r);
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if d.result.is_some() {
                if primary(ui, "Done", true) {
                    open = false;
                }
            } else {
                if primary(ui, "Compress", true) {
                    run = true;
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
    if run {
        d.result = Some(match ed.compress(d.quality) {
            Ok(r) => r,
            Err(e) => e,
        });
    }
    open
}

// ── export pages as images, and images to a PDF ──

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
}

pub struct ExportDialog {
    format: Format,
    dpi: f32,
    scope: Scope,
    range: String,
    folder: Option<PathBuf>,
    error: Option<String>,
}

impl ExportDialog {
    pub fn new(ed: &Editor) -> Self {
        let folder = ed.file.as_ref().and_then(|f| f.path.as_ref()).and_then(|p| p.parent()).map(|p| p.to_path_buf());
        ExportDialog { format: Format::Png, dpi: 150.0, scope: Scope::All, range: String::new(), folder, error: None }
    }
}

impl Editor {
    fn export_images(&mut self, pages: &[usize], format: Format, dpi: f32, folder: &std::path::Path) -> Result<usize, String> {
        let f = self.file.as_ref().ok_or("No document is open.")?;
        let stem = f.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into());
        let scale = dpi / 72.0;
        let mut n = 0;
        for &i in pages {
            let (w, h) = f.sizes[i];
            let Some(p) = f.doc.page(i) else { continue };
            let Some(img) = p.render(scale, [0, 0, (w * scale).round() as i32, (h * scale).round() as i32]) else { continue };
            drop(p);
            let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            let Some(rgba) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, raw) else { continue };
            let digits = f.page_count().to_string().len();
            let ext = if format == Format::Png { "png" } else { "jpg" };
            let path = folder.join(format!("{stem} page {:0digits$}.{ext}", i + 1));
            let res = match format {
                Format::Png => rgba.save_with_format(&path, image::ImageFormat::Png),
                Format::Jpeg => image::DynamicImage::ImageRgba8(rgba).to_rgb8().save_with_format(&path, image::ImageFormat::Jpeg),
            };
            res.map_err(|e| format!("Could not write {}: {e}", path.display()))?;
            n += 1;
        }
        Ok(n)
    }
}

pub fn export_window(ctx: &egui::Context, ed: &mut Editor, d: &mut ExportDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut run = false;
    let (_, close) = modal(ctx, "export-images", 400.0, |ui| {
        title(ui, "Export pages as images");
        ui.add_space(6.0);
        ui.segmented(&mut d.format, &[(Format::Png, "PNG"), (Format::Jpeg, "JPEG")]);
        ui.horizontal(|ui| {
            ui.label("Resolution");
            ui.add(egui::DragValue::new(&mut d.dpi).range(36.0..=600.0).speed(1.0).suffix(" dpi"));
        });
        theme::heading(ui, "PAGES");
        scope_ui(ui, &mut d.scope, &mut d.range, ed);
        theme::heading(ui, "SAVE TO");
        let label = d.folder.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "No folder chosen".into());
        ui.add(egui::Label::new(theme::secondary(ui, label.clone())).truncate()).on_hover_text(label);
        if ui.button("Choose folder…").clicked()
            && let Some(f) = rfd::FileDialog::new().pick_folder()
        {
            d.folder = Some(f);
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Export", d.folder.is_some()) {
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
    if run && let Some(folder) = d.folder.clone() {
        match scope_pages(ed, d.scope, &d.range).and_then(|pages| ed.export_images(&pages, d.format, d.dpi, &folder)) {
            Ok(n) => {
                ed.say(format!("Saved {} in {}", plural(n, "image", "images"), folder.display()));
                open = false;
            }
            Err(e) => d.error = Some(e),
        }
    }
    open
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImagePage {
    Fit,
    A4,
    Letter,
}

pub struct ImagesDialog {
    files: Vec<PathBuf>,
    page: ImagePage,
    error: Option<String>,
}

impl ImagesDialog {
    pub fn new(files: Vec<PathBuf>) -> Self {
        ImagesDialog { files, page: ImagePage::Fit, error: None }
    }
}

/// A new PDF with one image per page.
fn images_to_pdf(files: &[PathBuf], page: ImagePage) -> Result<Doc, String> {
    let doc = Doc::new_empty();
    for (k, path) in files.iter().enumerate() {
        let img = crate::editor::PendingImage::load(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (iw, ih) = (img.w as f32 * 0.75, img.h as f32 * 0.75);
        let (pw, ph) = match page {
            ImagePage::Fit => (iw, ih),
            ImagePage::A4 => (210.0 * MM, 297.0 * MM),
            ImagePage::Letter => (612.0, 792.0),
        };
        // Turn the paper to suit the picture.
        let (pw, ph) = if page != ImagePage::Fit && (iw > ih) != (pw > ph) { (ph, pw) } else { (pw, ph) };
        if !doc.insert_blank(k, pw, ph) {
            return Err("A page could not be added.".into());
        }
        let margin = if page == ImagePage::Fit { 0.0 } else { 36.0 };
        let s = ((pw - 2.0 * margin) / iw).min((ph - 2.0 * margin) / ih).min(if page == ImagePage::Fit { 1.0 } else { f32::MAX });
        let (w, h) = (iw * s, ih * s);
        let p = doc.page(k).ok_or("A page could not be loaded.")?;
        let data: ImageData = img.as_data();
        p.add_image(&data, [(pw - w) / 2.0, (ph - h) / 2.0, w, h]).ok_or("An image could not be placed.")?;
    }
    Ok(doc)
}

/// Returns Some(doc) once the PDF is made (the app opens it in a new tab), None while open, and
/// Some(None)-like closing via `open = false`.
pub fn images_window(ctx: &egui::Context, d: &mut ImagesDialog) -> (bool, Option<Doc>) {
    let p = pal(ctx);
    let mut open = true;
    let mut made = None;
    let mut run = false;
    let (_, close) = modal(ctx, "images-to-pdf", 400.0, |ui| {
        title(ui, "Create a PDF from images");
        ui.label(theme::secondary(ui, format!("{} in this order:", plural(d.files.len(), "image", "images"))));
        egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
            let mut swap = None;
            let n = d.files.len();
            for (k, f) in d.files.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(f.file_name().unwrap_or_default().to_string_lossy());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(k + 1 < n, egui::Button::new("↓").small()).clicked() {
                            swap = Some((k, k + 1));
                        }
                        if ui.add_enabled(k > 0, egui::Button::new("↑").small()).clicked() {
                            swap = Some((k - 1, k));
                        }
                    });
                });
            }
            if let Some((a, b)) = swap {
                d.files.swap(a, b);
            }
        });
        theme::heading(ui, "PAGE SIZE");
        ui.segmented(&mut d.page, &[(ImagePage::Fit, "Fit each image"), (ImagePage::A4, "A4"), (ImagePage::Letter, "Letter")]);
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Create", !d.files.is_empty()) {
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
        match images_to_pdf(&d.files, d.page) {
            Ok(doc) => {
                made = Some(doc);
                open = false;
            }
            Err(e) => d.error = Some(e),
        }
    }
    (open, made)
}
