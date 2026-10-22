//! The open document and everything done to it: history, caches, selection and edit operations.
//!
//! Every change goes through PDFium on the live document. Once a change has settled (no mouse
//! button held), the whole document is saved to memory as a history state, so undo and redo
//! simply reload an earlier state.

use crate::canvas::View;
use crate::content::{self, Edit};
use crate::fonts;
use crate::incremental;
use crate::annots::{self, NewField};
use crate::commands::{Keymap, PageLayout};
use crate::pdf::{self, Annot, Doc, Geom, Glyph, ImageData, Obj, OpenError, Page, PathStyle, Rgba};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

/// History is trimmed from the oldest end once the states take more memory than this.
const HISTORY_BUDGET: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Tool {
    Select,
    Text,
    Image,
    Rect,
    Ellipse,
    Line,
    Highlight,
    Note,
    Ink,
    Stamp,
    Arrow,
    TextBox,
    Callout,
    Measure,
    Field,
    Redact,
    Sign,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Text => "Text",
            Tool::Image => "Image",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Highlight => "Highlight",
            Tool::Note => "Note",
            Tool::Ink => "Draw",
            Tool::Stamp => "Stamp",
            Tool::Arrow => "Arrow",
            Tool::TextBox => "Text box",
            Tool::Callout => "Callout",
            Tool::Measure => "Measure",
            Tool::Field => "Form field",
            Tool::Redact => "Redact",
            Tool::Sign => "Sign",
        }
    }

    pub fn key(self) -> egui::Key {
        use egui::Key;
        match self {
            Tool::Select => Key::V,
            Tool::Text => Key::T,
            Tool::Image => Key::I,
            Tool::Rect => Key::R,
            Tool::Ellipse => Key::O,
            Tool::Line => Key::L,
            Tool::Highlight => Key::H,
            Tool::Note => Key::N,
            Tool::Ink => Key::P,
            Tool::Stamp => Key::K,
            Tool::Arrow => Key::A,
            Tool::TextBox => Key::X,
            Tool::Callout => Key::C,
            Tool::Measure => Key::M,
            Tool::Field => Key::F,
            Tool::Redact => Key::B,
            Tool::Sign => Key::G,
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Tool::Select => "Click to select text, images and shapes. Drag to move, double-click text to edit it.",
            Tool::Text => "Click existing text to edit it, or click anywhere else to add new text.",
            Tool::Image => "Click to place the picked image, or drag to set its size.",
            Tool::Rect | Tool::Ellipse => "Drag to draw. Hold Shift for equal sides.",
            Tool::Line => "Drag to draw. Hold Shift for steps of 45°.",
            Tool::Highlight => "Drag across text to mark it.",
            Tool::Note => "Click to add a comment.",
            Tool::Ink => "Drag to draw freehand.",
            Tool::Stamp => "Click to place the stamp.",
            Tool::Arrow => "Drag from the tail to the point. Hold Shift for steps of 45°.",
            Tool::TextBox => "Drag to draw the box, then type. The box grows to fit the text.",
            Tool::Callout => "Drag from the spot to point at to where the text should go, then type.",
            Tool::Measure => "Drag between two points to measure the distance. Hold Shift for steps of 45°.",
            Tool::Field => "Drag to draw a new field, or click an existing one to select, move or rename it.",
            Tool::Redact => "Drag over what should be removed. Marks can be checked and changed; Apply removes everything under them for good.",
            Tool::Sign => "Click to place your signature.",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FontKind {
    Sans,
    Serif,
    Mono,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Markup {
    Highlight,
    Underline,
    Strikeout,
}

impl Markup {
    pub fn subtype(self) -> i32 {
        match self {
            Markup::Highlight => pdf::ANNOT_HIGHLIGHT,
            Markup::Underline => pdf::ANNOT_UNDERLINE,
            Markup::Strikeout => pdf::ANNOT_STRIKEOUT,
        }
    }
}

/// Settings for new content, remembered between sessions.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub text_color: Rgba,
    pub text_size: f32,
    pub font: FontKind,
    pub bold: bool,
    pub italic: bool,
    pub stroke: Rgba,
    pub fill: Option<Rgba>,
    pub width: f32,
    pub markup: Markup,
    pub highlight: Rgba,
    pub ink: Rgba,
    pub ink_width: f32,
    pub note: Rgba,
    pub stamp: String,
    pub stamp_color: Rgba,
    /// Arrows, text boxes, callouts and measurements.
    pub review_color: Rgba,
    pub review_width: f32,
    pub box_size: f32,
    pub unit: Unit,
    /// Real distance per page distance (1 for 1:1, 100 for a 1:100 drawing).
    pub scale: f32,
    pub field: NewField,
    /// The certificate last used to sign digitally.
    pub pfx: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Unit {
    Mm,
    Cm,
    In,
    Pt,
}

impl Unit {
    pub fn label(self) -> &'static str {
        match self {
            Unit::Mm => "mm",
            Unit::Cm => "cm",
            Unit::In => "in",
            Unit::Pt => "pt",
        }
    }

    /// This unit per PDF point.
    pub fn per_point(self) -> f32 {
        match self {
            Unit::Mm => 25.4 / 72.0,
            Unit::Cm => 2.54 / 72.0,
            Unit::In => 1.0 / 72.0,
            Unit::Pt => 1.0,
        }
    }
}

/// "12.5 mm" for a distance in points, at the given drawing scale.
pub fn measure_label(points: f32, unit: Unit, scale: f32) -> String {
    let v = points * unit.per_point() * scale;
    let digits = if v >= 100.0 { 0 } else if v >= 10.0 { 1 } else { 2 };
    format!("{v:.digits$} {}", unit.label())
}

impl Default for Style {
    fn default() -> Self {
        Style {
            text_color: [20, 22, 28, 255],
            text_size: 12.0,
            font: FontKind::Sans,
            bold: false,
            italic: false,
            stroke: [10, 132, 255, 255],
            fill: None,
            width: 2.0,
            markup: Markup::Highlight,
            highlight: [255, 214, 10, 255],
            ink: [225, 87, 89, 255],
            ink_width: 2.0,
            note: [255, 204, 0, 255],
            stamp: "Approved".into(),
            stamp_color: [89, 161, 79, 255],
            review_color: [225, 87, 89, 255],
            review_width: 1.5,
            box_size: 12.0,
            unit: Unit::Mm,
            scale: 1.0,
            field: NewField::Text,
            pfx: None,
        }
    }
}

/// How documents are shown, remembered between sessions.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewPrefs {
    /// Layout for documents opened for the first time.
    pub layout: PageLayout,
    pub cover: bool,
    /// Reopen each document with the layout it had.
    pub remember: bool,
    /// Open documents straight into editing.
    pub open_editing: bool,
    /// Layouts of recent documents.
    pub remembered: Vec<(PathBuf, PageLayout, bool)>,
    /// Pages drawn light on dark, for reading at night.
    pub dark_pages: bool,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        ViewPrefs { layout: PageLayout::Continuous, cover: true, remember: true, open_editing: false, remembered: Vec::new(), dark_pages: false }
    }
}

/// A saved signature, placed with the Sign tool.
#[derive(Clone, Serialize, Deserialize)]
pub enum Signature {
    /// Strokes in a box `aspect` wide and 1 tall, y pointing down.
    Drawn {
        strokes: Vec<Vec<(f32, f32)>>,
        aspect: f32,
    },
    Typed {
        text: String,
    },
    Image {
        path: PathBuf,
    },
}

#[derive(Clone, PartialEq, Debug, Default)]
pub enum Sel {
    #[default]
    None,
    Objects {
        page: usize,
        items: Vec<usize>,
    },
    Annot {
        page: usize,
        index: usize,
    },
}

impl Sel {
    pub fn page(&self) -> Option<usize> {
        match self {
            Sel::None => None,
            Sel::Objects { page, .. } | Sel::Annot { page, .. } => Some(*page),
        }
    }
}

/// Text being typed on the page.
pub struct TextEdit {
    pub page: usize,
    /// The text object being edited, or None for new text.
    pub object: Option<usize>,
    pub text: String,
    /// Baseline origin of new text, in user space.
    pub at: (f32, f32),
    /// Font size on screen, in view points.
    pub size: f32,
    pub focus: bool,
    /// Set when a whole paragraph (several lines or pieces) is being edited.
    pub paragraph: Option<crate::arrange::Paragraph>,
}

pub struct NoteDraft {
    pub page: usize,
    pub at: (f32, f32),
    /// Some when an existing note is being changed.
    pub index: Option<usize>,
    pub text: String,
    pub focus: bool,
    pub kind: DraftKind,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DraftKind {
    Note,
    /// A text box (l, b, r, t), pointing at a spot when it is a callout.
    TextBox { rect: [f32; 4], callout: Option<(f32, f32)> },
}

pub struct FieldEdit {
    pub page: usize,
    pub index: usize,
    pub text: String,
    pub multiline: bool,
    pub focus: bool,
}

pub enum ImageBytes {
    Jpeg(Vec<u8>),
    Rgba(Vec<u8>),
}

/// An image picked with the Image tool, waiting for a click on the page.
pub struct PendingImage {
    pub name: String,
    pub w: u32,
    pub h: u32,
    pub data: ImageBytes,
}

impl PendingImage {
    pub fn load(path: &Path) -> Result<PendingImage, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let img = image::load_from_memory(&bytes).map_err(|_| "This image format is not supported.".to_string())?;
        let (w, h) = (img.width(), img.height());
        let jpeg = matches!(image::guess_format(&bytes), Ok(image::ImageFormat::Jpeg));
        // JPEG files go in unchanged (small and exact); everything else as pixels.
        let data = if jpeg { ImageBytes::Jpeg(bytes) } else { ImageBytes::Rgba(img.into_rgba8().into_raw()) };
        Ok(PendingImage { name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), w, h, data })
    }

    pub fn as_data(&self) -> ImageData<'_> {
        match &self.data {
            ImageBytes::Jpeg(b) => ImageData::Jpeg(b),
            ImageBytes::Rgba(px) => ImageData::Rgba { w: self.w, h: self.h, px },
        }
    }
}

/// A document that needs a password before it can be opened.
pub struct PasswordAsk {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub text: String,
    pub wrong: bool,
    /// Asking for the owner password, to edit a document whose author restricted changes.
    pub owner: bool,
}

#[derive(Default)]
struct PageCache {
    geom: Option<Geom>,
    objects: Option<Rc<Vec<Obj>>>,
    annots: Option<Rc<Vec<Annot>>>,
    glyphs: Option<Rc<Vec<Glyph>>>,
}

pub struct File {
    pub doc: Doc,
    pub path: Option<PathBuf>,
    pub password: Option<String>,
    /// How it is protected when saved.
    pub security: crate::security::Security,
    /// Its digital signatures, checked against the file as it was opened.
    pub signatures: Vec<crate::sign::SigInfo>,
    /// Names its autosaved copy.
    pub uid: u64,
    history: Vec<Vec<u8>>,
    at: usize,
    saved_at: Option<usize>,
    /// Displayed size of every page, in points.
    pub sizes: Vec<(f32, f32)>,
    /// Bumped whenever a page changes, so its picture is redrawn.
    pub revs: Vec<u64>,
    cache: RefCell<Vec<PageCache>>,
}

impl File {
    fn new(doc: Doc, bytes: Vec<u8>, path: Option<PathBuf>, password: Option<String>) -> File {
        let saved = path.is_some().then_some(0);
        let mut f =
            File {
                doc,
                path,
                password,
                security: Default::default(),
                signatures: Vec::new(),
                uid: u64::from_le_bytes(crate::security::random_bytes(8).try_into().unwrap_or([7; 8])),
                history: vec![bytes], at: 0, saved_at: saved, sizes: Vec::new(), revs: Vec::new(), cache: RefCell::new(Vec::new()) };
        f.relayout();
        f
    }

    fn relayout(&mut self) {
        let n = self.doc.page_count();
        self.sizes = (0..n).map(|i| self.doc.page_size(i)).collect();
        self.revs = vec![0; n];
        self.cache = RefCell::new((0..n).map(|_| PageCache::default()).collect());
    }

    pub fn name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled.pdf".into())
    }

    pub fn dirty(&self) -> bool {
        self.saved_at != Some(self.at)
    }

    /// The latest recorded state of the document (what autosave writes).
    pub fn current_state(&self) -> Option<&[u8]> {
        self.history.get(self.at).map(Vec::as_slice)
    }

    /// Which history state is current; changes whenever the document does.
    pub fn state(&self) -> usize {
        self.at + self.history.len() * 1_000_003
    }

    /// Something to save that is not in the history (such as protection).
    pub fn mark_unsaved(&mut self) {
        self.saved_at = None;
    }

    /// The document was protected and has just been rewritten without its encryption (to change
    /// it directly): keep it protected the same way when it is saved.
    fn keep_protection(&mut self, permissions: u32) {
        if self.security == crate::security::Security::Keep {
            let pw = self.password.clone().unwrap_or_default();
            self.security = crate::security::Security::Protect(crate::security::Protection {
                open: pw.clone(),
                owner: pw,
                allow: crate::security::Allow::from_bits(permissions),
            });
        }
    }

    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    fn invalidate(&mut self, i: usize) {
        if let Some(c) = self.cache.get_mut().get_mut(i) {
            *c = PageCache::default();
        }
        if let Some(r) = self.revs.get_mut(i) {
            *r += 1;
        }
        if i < self.sizes.len() {
            self.sizes[i] = self.doc.page_size(i);
        }
    }

    fn cached<T>(&self, i: usize, get: impl Fn(&mut PageCache) -> &mut Option<T>, load: impl FnOnce(&Page) -> T) -> Option<T>
    where
        T: Clone,
    {
        let mut cache = self.cache.borrow_mut();
        let entry = cache.get_mut(i)?;
        if let Some(v) = get(entry) {
            return Some(v.clone());
        }
        let page = self.doc.page(i)?;
        let v = load(&page);
        *get(entry) = Some(v.clone());
        Some(v)
    }

    pub fn geom(&self, i: usize) -> Option<Geom> {
        self.cached(i, |c| &mut c.geom, |p| p.geom())
    }

    pub fn objects(&self, i: usize) -> Rc<Vec<Obj>> {
        self.cached(i, |c| &mut c.objects, |p| Rc::new(p.objects())).unwrap_or_default()
    }

    pub fn annots(&self, i: usize) -> Rc<Vec<Annot>> {
        self.cached(i, |c| &mut c.annots, |p| Rc::new(p.annots())).unwrap_or_default()
    }

    pub fn glyphs(&self, i: usize) -> Rc<Vec<Glyph>> {
        self.cached(i, |c| &mut c.glyphs, |p| Rc::new(p.glyphs())).unwrap_or_default()
    }

    /// Page sizes or boxes changed: lay the pages out again.
    pub fn refresh_sizes(&mut self) {
        self.relayout();
    }

    /// Use a rewritten document (compressed) from now on.
    pub fn replace_doc(&mut self, doc: Doc) {
        self.doc = doc;
        self.relayout();
    }

    /// A second, independent copy of the current document (for copying pages within it).
    pub fn snapshot(&self) -> Option<Doc> {
        Doc::open(self.history[self.at].clone(), self.password.as_deref()).ok()
    }
}

/// Edits, new image XObjects and extra resources for one page change.
pub type SurgeryPlan = (Vec<(usize, Edit)>, Vec<(Vec<u8>, lopdf::Stream)>, incremental::ExtraResources);

/// A page's content as the file stores it, matched object for object to PDFium's view of it.
pub struct PageModel {
    pub bytes: Vec<u8>,
    pub data: incremental::PageData,
    pub parsed: content::Parsed,
    pub starts: Vec<Option<content::M>>,
}

impl PageModel {
    pub fn load(f: &File, page: usize) -> Result<PageModel, String> {
        let bytes = f.doc.save_plain()?;
        let data = incremental::page_data(&bytes, page)?;
        let parsed = content::parse(&data.content, &data.xobjects);
        let p = f.doc.page(page).ok_or("The page could not be loaded.")?;
        let kinds = p.kinds();
        let code = |k: pdf::Kind| match k {
            pdf::Kind::Text => 1,
            pdf::Kind::Path => 2,
            pdf::Kind::Image => 3,
            pdf::Kind::Shading => 4,
            pdf::Kind::Group => 5,
        };
        let matches = kinds.len() == parsed.objs.len() && kinds.iter().zip(&parsed.objs).all(|(k, o)| *k == code(o.kind));
        if !matches {
            return Err("Dockit could not match this page's drawing commands to what it shows, so the page was left unchanged.".into());
        }
        let starts = kinds.iter().enumerate().map(|(i, k)| (*k == 1).then(|| p.matrix(i).map(|v| v as f64))).collect();
        Ok(PageModel { bytes, data, parsed, starts })
    }

    /// What object `obj`'s font can show: the learned codes, the font's facts and its word gap.
    pub fn encoding(&self, f: &File, page: usize, obj: usize) -> Option<(std::collections::HashMap<char, Vec<u8>>, content::FontMeta, f64)> {
        let t = self.parsed.objs.get(obj)?.text.as_ref()?;
        let meta = self.data.fonts.get(&t.font).cloned().unwrap_or_default();
        let texts = f.doc.page(page)?.texts();
        Some((content::learn_encoding(&self.parsed, &texts, &t.font, &meta), meta, t.word_kern))
    }

    /// Codes for `text` in object `obj`'s own font, if every character is available.
    pub fn encode(&self, f: &File, page: usize, obj: usize, text: &str) -> Option<Vec<content::Piece>> {
        let t = self.parsed.objs.get(obj)?.text.as_ref()?;
        let meta = self.data.fonts.get(&t.font).cloned().unwrap_or_default();
        let texts = f.doc.page(page)?.texts();
        let learned = content::learn_encoding(&self.parsed, &texts, &t.font, &meta);
        content::encode(text, &learned, &meta, t.word_kern)
    }
}

pub struct Editor {
    pub file: Option<File>,
    pub tool: Tool,
    pub style: Style,
    /// Screen points per PDF point.
    pub zoom: f32,
    pub sel: Sel,
    /// Pages selected in the Pages panel.
    pub page_sel: Vec<usize>,
    /// The page at the middle of the view.
    pub current: usize,
    /// Scroll the view to this page.
    pub goto: Option<usize>,
    /// Fit the page width (true) or the whole page (false) on the next frame.
    pub fit: Option<bool>,
    pub text_edit: Option<TextEdit>,
    pub note: Option<NoteDraft>,
    pub field_edit: Option<FieldEdit>,
    pub image: Option<PendingImage>,
    pub signature: Option<Signature>,
    pub signature_dialog: bool,
    pub status: Option<(String, Instant)>,
    pub error: Option<String>,
    pub password: Option<PasswordAsk>,
    pub recent: Vec<PathBuf>,
    /// Something changed since the last history state.
    pub needs_commit: bool,
    /// Bumped whenever pages are added, removed or reordered, or a state is reloaded.
    pub epoch: u64,
    pub view: View,
    /// A window the app should open (asked for from inside a panel).
    pub request: Option<Request>,
    /// Copied page objects.
    pub clip: Option<crate::arrange::Clip>,
    /// Editing (true) or just reading the document.
    pub editing: bool,
    pub keys: Keymap,
    pub palette: Option<crate::palette::Palette>,
    pub view_prefs: ViewPrefs,
    /// This document's page layout.
    pub layout: PageLayout,
    /// With two pages side by side, the first page stands alone.
    pub cover: bool,
    pub find: Option<crate::find::Find>,
    /// Selected text while reading: page and first and last character.
    pub text_sel: Option<(usize, usize, usize)>,
    /// Scroll so this area of a page (l, b, r, t) is in view.
    pub reveal: Option<(usize, [f32; 4])>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    Combine,
    /// Sign with a certificate, in this signature field (page, annotation) if given.
    SignDigitally(Option<(usize, usize)>),
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            file: None,
            tool: Tool::Select,
            style: Style::default(),
            zoom: 1.0,
            sel: Sel::None,
            page_sel: Vec::new(),
            current: 0,
            goto: None,
            fit: None,
            text_edit: None,
            note: None,
            field_edit: None,
            image: None,
            signature: None,
            signature_dialog: false,
            status: None,
            error: None,
            password: None,
            recent: Vec::new(),
            needs_commit: false,
            epoch: 0,
            view: View::default(),
            request: None,
            clip: None,
            editing: false,
            keys: Keymap::default(),
            palette: None,
            view_prefs: ViewPrefs::default(),
            layout: PageLayout::Continuous,
            cover: true,
            find: None,
            text_sel: None,
            reveal: None,
        }
    }
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Text that only uses characters in the standard PDF fonts' encoding (WinAnsi, roughly Latin-1).
fn latin(s: &str) -> bool {
    s.chars().all(|c| (c as u32) < 0x100 || "€‚ƒ„…†‡ˆ‰Š‹ŒŽ‘’“”•–—˜™š›œžŸ".contains(c))
}

impl Editor {
    pub fn say(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    fn clear_transient(&mut self) {
        self.sel = Sel::None;
        self.text_sel = None;
        self.text_edit = None;
        self.note = None;
        self.field_edit = None;
    }

    // ── files ──

    pub fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file() {
            self.open_path(&p);
        }
    }

    pub fn open_path(&mut self, path: &Path) {
        match std::fs::read(path) {
            Ok(bytes) => self.open_bytes(path, bytes, None),
            Err(e) => self.error = Some(format!("Could not open {}: {e}", path.display())),
        }
    }

    /// Open `bytes` read from `path`; asks for a password if the file needs one.
    pub fn open_bytes(&mut self, path: &Path, bytes: Vec<u8>, password: Option<String>) {
        match Doc::open(bytes.clone(), password.as_deref()) {
            Ok(doc) => {
                self.password = None;
                let signatures = crate::sign::verify_all(&bytes, &doc);
                let (doc, bytes) = settle_appearances(doc, bytes, password.as_deref());
                let mut file = File::new(doc, bytes, Some(path.to_path_buf()), password);
                file.signatures = signatures;
                self.file = Some(file);
                self.after_open();
                self.recent.retain(|p| p != path);
                self.recent.insert(0, path.to_path_buf());
                self.recent.truncate(10);
                let n = self.file.as_ref().map_or(0, |f| f.page_count());
                let mut msg = format!("Opened {}, {}", path.file_name().unwrap_or_default().to_string_lossy(), plural(n, "page", "pages"));
                if let Some(f) = &self.file
                    && !f.signatures.is_empty()
                {
                    let bad = f.signatures.iter().filter(|s| !s.valid).count();
                    let names: Vec<&str> = f.signatures.iter().map(|s| s.signer.as_str()).filter(|n| !n.is_empty()).collect();
                    msg = if bad > 0 {
                        format!("{msg}. Warning: {} not valid", plural(bad, "signature is", "signatures are"))
                    } else {
                        format!("{msg}. Signed by {}, valid", names.join(", "))
                    };
                }
                self.say(msg);
            }
            Err(OpenError::Password) => {
                let wrong = password.is_some();
                self.password = Some(PasswordAsk { path: path.to_path_buf(), bytes, text: String::new(), wrong, owner: false });
            }
            Err(OpenError::Failed(msg)) => self.error = Some(format!("Could not open {}. {msg}", path.display())),
        }
    }

    /// The author restricted changes and the owner password has not been given.
    pub fn restricted(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.doc.encrypted() && !crate::security::Allow::from_bits(f.doc.permissions()).edit)
    }

    /// Switch between reading and editing. Leaving editing finishes anything being typed.
    pub fn set_editing(&mut self, on: bool) {
        if on && !self.editing && self.restricted() && let Some(f) = &self.file {
            // The author restricted changes: the owner password unlocks them.
            if let Some(path) = f.path.clone() {
                self.password = Some(PasswordAsk { path, bytes: f.history[0].clone(), text: String::new(), wrong: false, owner: true });
            } else {
                self.error = Some("The author of this document does not allow changes to it.".into());
            }
            return;
        }
        if !on {
            self.commit_edits();
            self.clear_selection();
            self.tool = Tool::Select;
            self.image = None;
        }
        if self.editing != on {
            // The two layouts have different widths: refit the page.
            self.fit = Some(true);
            if on && self.file.as_ref().is_some_and(|f| !f.signatures.is_empty()) {
                self.say("This document is digitally signed. Any change you save will make its signatures invalid.");
            }
        }
        self.editing = on;
    }

    fn after_open(&mut self) {
        // Documents open for reading (Edit PDF switches to editing), unless Settings says otherwise.
        self.editing = false;
        let path = self.file.as_ref().and_then(|f| f.path.clone());
        let known = path.as_ref().filter(|_| self.view_prefs.remember).and_then(|p| self.view_prefs.remembered.iter().find(|r| &r.0 == p));
        (self.layout, self.cover) = match known {
            Some(r) => (r.1, r.2),
            None => (self.view_prefs.layout, self.view_prefs.cover),
        };
        self.clear_transient();
        self.page_sel.clear();
        self.current = 0;
        self.goto = Some(0);
        self.fit = Some(true);
        self.needs_commit = false;
        self.epoch += 1;
        if self.view_prefs.open_editing && !self.restricted() {
            self.set_editing(true);
        }
    }

    // ── page layout ──

    /// Show this document with `layout` (and remember it for next time).
    pub fn set_layout(&mut self, layout: PageLayout, cover: bool) {
        self.layout = layout;
        self.cover = cover;
        self.goto = Some(self.current);
        if let Some(p) = self.file.as_ref().and_then(|f| f.path.clone()) {
            let r = &mut self.view_prefs.remembered;
            r.retain(|x| x.0 != p);
            r.insert(0, (p, layout, cover));
            r.truncate(200);
        }
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<String> {
        let (page, a, b) = self.text_sel?;
        let t = self.file.as_ref()?.doc.page(page)?.text_range(a.min(b), a.max(b));
        (!t.is_empty()).then_some(t)
    }

    /// The first page of the row (page or spread) that page `i` is in.
    pub fn row_start(&self, i: usize) -> usize {
        if !self.layout.two() {
            return i;
        }
        if self.cover {
            if i == 0 { 0 } else { i - (i - 1) % 2 }
        } else {
            i - i % 2
        }
    }

    /// The pages in the row starting at `start`.
    pub fn row_len(&self, start: usize) -> usize {
        let n = self.file.as_ref().map_or(0, |f| f.page_count());
        let len = if !self.layout.two() || (self.cover && start == 0) { 1 } else { 2 };
        len.min(n.saturating_sub(start)).max(1)
    }

    pub fn next_page(&mut self) {
        let n = self.file.as_ref().map_or(0, |f| f.page_count());
        let s = self.row_start(self.current);
        let next = s + self.row_len(s);
        if next < n {
            self.goto = Some(next);
        }
    }

    pub fn prev_page(&mut self) {
        let s = self.row_start(self.current);
        if s > 0 {
            self.goto = Some(self.row_start(s - 1));
        }
    }

    /// Open a document that exists only in memory (made from images), not yet saved.
    pub fn open_untitled(&mut self, doc: Doc) {
        match doc.save() {
            Ok(bytes) => {
                self.file = Some(File::new(doc, bytes, None, None));
                self.after_open();
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn close(&mut self) {
        self.file = None;
        self.clear_transient();
        self.page_sel.clear();
        self.epoch += 1;
    }

    /// Open a file without showing it, for work on many files at once.
    pub fn open_headless(path: &Path) -> Result<Editor, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{} could not be read ({e}).", path.display()))?;
        match Doc::open(bytes.clone(), None) {
            Ok(doc) => {
                let mut ed = Editor::default();
                ed.file = Some(File::new(doc, bytes, Some(path.to_path_buf()), None));
                ed.editing = true;
                Ok(ed)
            }
            Err(OpenError::Password) => Err("it is password protected".into()),
            Err(OpenError::Failed(m)) => Err(m),
        }
    }

    /// The bytes to write for the document, protected as its settings say.
    fn output_bytes(&self) -> Result<Vec<u8>, String> {
        use crate::security::Security;
        let f = self.file.as_ref().ok_or("No document is open.")?;
        match &f.security {
            Security::Keep => f.doc.save(),
            Security::Remove => f.doc.save_plain(),
            Security::Protect(p) => f.doc.save_plain().and_then(|b| crate::security::encrypt(&b, p)),
        }
    }

    /// Write the document to `target` (safely: a temporary file, then swapped in).
    pub fn write_to(&mut self, target: &Path) -> Result<(), String> {
        self.commit_edits();
        self.commit();
        let bytes = self.output_bytes()?;
        let tmp = target.with_extension("pdf.dockit-tmp");
        std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, target)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("Could not save {}: {e}", target.display())
        })
    }

    /// Write the document to its file, or ask where when `as_new` or untitled. False if cancelled or failed.
    pub fn save(&mut self, as_new: bool) -> bool {
        self.commit_edits();
        self.commit();
        let Some(f) = &self.file else { return false };
        let target = match (&f.path, as_new) {
            (Some(p), false) => p.clone(),
            _ => {
                let mut d = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(f.name());
                if let Some(dir) = f.path.as_ref().and_then(|p| p.parent()) {
                    d = d.set_directory(dir);
                }
                match d.save_file() {
                    Some(p) => p,
                    None => return false,
                }
            }
        };
        let bytes = match self.output_bytes() {
            Ok(b) => b,
            Err(e) => {
                self.error = Some(e);
                return false;
            }
        };
        // Write next to the target, then swap, so a failed write never leaves half a file.
        let tmp = target.with_extension("pdf.dockit-tmp");
        let result = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &target));
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            self.error = Some(format!("Could not save {}: {e}", target.display()));
            return false;
        }
        let f = self.file.as_mut().unwrap();
        f.path = Some(target.clone());
        f.saved_at = Some(f.at);
        self.recent.retain(|p| *p != target);
        self.recent.insert(0, target.clone());
        self.recent.truncate(10);
        self.say(format!("Saved {}", target.file_name().unwrap_or_default().to_string_lossy()));
        true
    }

    pub fn needs_save(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.dirty() || self.needs_commit)
    }

    // ── history ──

    /// Store the current document as a new history state.
    pub fn commit(&mut self) {
        if !self.needs_commit {
            return;
        }
        self.needs_commit = false;
        let Some(f) = &mut self.file else { return };
        match f.doc.save() {
            Ok(bytes) => {
                f.history.truncate(f.at + 1);
                if f.saved_at.is_some_and(|s| s > f.at) {
                    f.saved_at = None;
                }
                f.history.push(bytes);
                f.at += 1;
                let mut total: usize = f.history.iter().map(Vec::len).sum();
                while total > HISTORY_BUDGET && f.history.len() > 2 {
                    total -= f.history.remove(0).len();
                    f.at -= 1;
                    f.saved_at = f.saved_at.and_then(|s| s.checked_sub(1));
                }
            }
            Err(e) => self.error = Some(format!("Could not record this change: {e}")),
        }
    }

    /// Commit once nothing is being dragged or typed.
    pub fn tick(&mut self, ctx: &egui::Context) {
        if self.needs_commit && !ctx.input(|i| i.pointer.any_down()) && !ctx.egui_wants_keyboard_input() {
            self.commit();
        }
    }

    pub fn can_undo(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.at > 0 || self.needs_commit)
    }

    pub fn can_redo(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.at + 1 < f.history.len())
    }

    fn reload(&mut self) {
        let Some(f) = &mut self.file else { return };
        match Doc::open(f.history[f.at].clone(), f.password.as_deref()) {
            Ok(doc) => {
                f.doc = doc;
                f.relayout();
            }
            Err(_) => self.error = Some("Could not restore that state.".into()),
        }
        self.clear_transient();
        let n = self.file.as_ref().map_or(0, |f| f.page_count());
        self.page_sel.retain(|&p| p < n);
        self.epoch += 1;
    }

    pub fn undo(&mut self) {
        self.commit_edits();
        self.commit();
        if let Some(f) = &mut self.file
            && f.at > 0
        {
            f.at -= 1;
            self.reload();
        }
    }

    pub fn redo(&mut self) {
        if let Some(f) = &mut self.file
            && f.at + 1 < f.history.len()
        {
            f.at += 1;
            self.reload();
        }
    }

    // ── applying changes ──

    /// Run `op` on page `i`, then refresh that page. The change is recorded once it settles.
    pub fn apply<R>(&mut self, i: usize, op: impl FnOnce(&Page) -> R) -> Option<R> {
        let f = self.file.as_mut()?;
        let r = {
            let page = f.doc.page(i)?;
            op(&page)
        };
        f.invalidate(i);
        self.needs_commit = true;
        Some(r)
    }

    /// Run `op` on the whole document (adding, removing or moving pages).
    pub fn apply_doc<R>(&mut self, op: impl FnOnce(&File) -> R) -> Option<R> {
        self.commit_edits();
        let f = self.file.as_mut()?;
        let r = op(f);
        f.relayout();
        self.clear_transient();
        self.epoch += 1;
        self.needs_commit = true;
        Some(r)
    }

    /// Finish any text, note or field being typed.
    pub fn commit_edits(&mut self) {
        self.commit_text();
        self.commit_note();
        self.commit_field();
    }

    // ── selection ──

    pub fn clear_selection(&mut self) {
        self.sel = Sel::None;
    }

    pub fn select_all(&mut self) {
        let Some(f) = &self.file else { return };
        let page = self.sel.page().unwrap_or(self.current);
        let items: Vec<usize> = f.objects(page).iter().map(|o| o.index).collect();
        self.sel = if items.is_empty() { Sel::None } else { Sel::Objects { page, items } };
    }

    pub fn delete_selection(&mut self) {
        match std::mem::take(&mut self.sel) {
            Sel::Objects { page, items } => {
                let n = items.len();
                if self.surgery(page, items.iter().map(|&i| (i, Edit::Delete)).collect(), Vec::new()) {
                    self.say(format!("Deleted {}", plural(n, "item", "items")));
                }
            }
            Sel::Annot { page, index } => {
                // Replies and review states go with the comment they belong to.
                let mut gone: Vec<usize> =
                    self.file.as_ref().map(|f| f.annots(page).iter().filter(|a| a.reply_to == Some(index)).map(|a| a.index).collect()).unwrap_or_default();
                gone.push(index);
                gone.sort_unstable_by(|a, b| b.cmp(a));
                self.apply(page, |p| {
                    for i in gone {
                        p.remove_annot(i);
                    }
                });
                self.say("Deleted the annotation");
            }
            Sel::None => {}
        }
    }

    /// Move the selection by (dx, dy) in user space.
    pub fn move_selection(&mut self, dx: f32, dy: f32) {
        match self.sel.clone() {
            Sel::Objects { page, items } => {
                let t = [1.0, 0.0, 0.0, 1.0, dx as f64, dy as f64];
                if self.surgery(page, items.iter().map(|&i| (i, Edit::Transform(t))).collect(), Vec::new()) {
                    self.sel = Sel::Objects { page, items };
                }
            }
            Sel::Annot { page, index } => {
                if self.drawn_here(page, index) {
                    self.file_update(&[page], |u| {
                        let id = u.annot_id(page, index)?;
                        u.modify(id, |d| annots::translate(d, dx, dy))
                    });
                    self.sel = Sel::Annot { page, index };
                } else {
                    self.apply(page, |p| p.move_annot(index, dx, dy));
                }
            }
            Sel::None => {}
        }
    }

    /// Nudge by screen arrows: `dx`, `dy` in view space.
    pub fn nudge(&mut self, dx: f32, dy: f32) {
        let Some(page) = self.sel.page() else { return };
        let Some(g) = self.file.as_ref().and_then(|f| f.geom(page)) else { return };
        let (ax, ay) = g.to_user(0.0, 0.0);
        let (bx, by) = g.to_user(dx, dy);
        self.move_selection(bx - ax, by - ay);
    }

    /// Scale and move the selected objects so `from` (user-space rect l, b, r, t) becomes `to`.
    pub fn reshape_selection(&mut self, from: [f32; 4], to: [f32; 4]) {
        let Sel::Objects { page, items } = self.sel.clone() else { return };
        let sx = if (from[2] - from[0]).abs() > 1e-3 { (to[2] - to[0]) / (from[2] - from[0]) } else { 1.0 };
        let sy = if (from[3] - from[1]).abs() > 1e-3 { (to[3] - to[1]) / (from[3] - from[1]) } else { 1.0 };
        let (sx, sy) = (sx as f64, sy as f64);
        let (ex, ey) = (to[0] as f64 - from[0] as f64 * sx, to[1] as f64 - from[1] as f64 * sy);
        let t = [sx, 0.0, 0.0, sy, ex, ey];
        if self.surgery(page, items.iter().map(|&i| (i, Edit::Transform(t))).collect(), Vec::new()) {
            self.sel = Sel::Objects { page, items };
        }
    }

    // ── changing existing content ──

    /// Change existing page objects by rewriting only their drawing commands (see content.rs),
    /// appended to the file as an incremental update. `images` are new image XObjects to add to
    /// the page's resources. Returns false (and says why) if the page could not be changed safely.
    pub fn surgery(&mut self, page: usize, edits: Vec<(usize, Edit)>, images: Vec<(Vec<u8>, lopdf::Stream)>) -> bool {
        self.surgery_with(page, |_| Ok((edits, images, Vec::new())))
    }

    /// Like `surgery`, but the edits are worked out from the page's content model first (for
    /// edits that need to read the file, such as ungrouping).
    pub fn surgery_with(&mut self, page: usize, plan: impl FnOnce(&PageModel) -> Result<SurgeryPlan, String>) -> bool {
        let Some(f) = &self.file else { return false };
        let was_encrypted = f.doc.encrypted();
        let permissions = f.doc.permissions();
        let result = PageModel::load(f, page).and_then(|m| {
            let (edits, images, extra) = plan(&m)?;
            let new = content::edit(&m.data.content, &m.parsed, &m.starts, &edits)?;
            incremental::write_update(&m.bytes, &m.data, &new, images, extra)
        });
        let bytes = match result {
            Ok(b) => b,
            Err(e) => {
                self.error = Some(format!("This change could not be made. {e}"));
                return false;
            }
        };
        let doc = match Doc::open(bytes, None) {
            Ok(d) => d,
            Err(_) => {
                self.error = Some("This change could not be made: the updated page could not be read back.".into());
                return false;
            }
        };
        let f = self.file.as_mut().unwrap();
        f.doc = doc;
        // The page's object list may have changed; the other pages are as they were.
        f.invalidate(page);
        if was_encrypted {
            f.keep_protection(permissions);
            self.say("This file is password protected. It stays protected with the same password when you save it.");
        }
        self.needs_commit = true;
        true
    }

    /// One edit on one object.
    pub fn edit_object(&mut self, page: usize, obj: usize, e: Edit) {
        let keep = !matches!(e, Edit::Delete);
        if self.surgery(page, vec![(obj, e)], Vec::new()) && keep {
            self.sel = Sel::Objects { page, items: vec![obj] };
        }
    }

    /// Scale a text object about its starting point so its size becomes `size` (in points).
    pub fn set_text_size(&mut self, page: usize, obj: usize, current: f32, size: f32) {
        let Some(f) = &self.file else { return };
        let Some(p) = f.doc.page(page) else { return };
        let m = p.matrix(obj);
        drop(p);
        if current <= 0.0 {
            return;
        }
        let s = (size / current) as f64;
        let (ox, oy) = (m[4] as f64, m[5] as f64);
        self.edit_object(page, obj, Edit::Transform([s, 0.0, 0.0, s, ox - s * ox, oy - s * oy]));
    }

    // ── text ──

    pub fn begin_text_edit(&mut self, page: usize, object: usize) {
        self.commit_edits();
        let Some(f) = &self.file else { return };
        let Some(p) = f.doc.page(page) else { return };
        let text = p.text(object);
        let m = p.matrix(object);
        let scale = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
        let size = p.font_size(object) * scale * f.geom(page).map_or(1.0, |g| g.unit());
        drop(p);
        // Text that is part of a longer paragraph is edited as the whole paragraph.
        let paragraph = self.paragraph_of(page, object).filter(|para| para.objects.len() > 1);
        let (text, items) = match &paragraph {
            Some(para) => (para.text.clone(), para.objects.clone()),
            None => (text, vec![object]),
        };
        self.sel = Sel::Objects { page, items };
        self.text_edit = Some(TextEdit { page, object: Some(object), text, at: (0.0, 0.0), size, focus: true, paragraph });
    }

    pub fn begin_new_text(&mut self, page: usize, at: (f32, f32)) {
        self.commit_edits();
        self.clear_selection();
        self.text_edit = Some(TextEdit { page, object: None, text: String::new(), at, size: self.style.text_size, focus: true, paragraph: None });
    }

    pub fn commit_text(&mut self) {
        let Some(t) = self.text_edit.take() else { return };
        if let Some(para) = &t.paragraph {
            self.commit_paragraph(t.page, para, &t.text);
            return;
        }
        match t.object {
            Some(obj) => self.set_object_text(t.page, obj, &t.text),
            None => {
                if !t.text.trim().is_empty() {
                    self.add_text(t.page, t.at, &t.text);
                }
            }
        }
    }

    fn new_text_font(&self, doc: &Doc, text: &str) -> Option<pdfium_render::prelude::FPDF_FONT> {
        let s = &self.style;
        if latin(text) {
            let name = match (s.font, s.bold, s.italic) {
                (FontKind::Sans, false, false) => "Helvetica",
                (FontKind::Sans, true, false) => "Helvetica-Bold",
                (FontKind::Sans, false, true) => "Helvetica-Oblique",
                (FontKind::Sans, true, true) => "Helvetica-BoldOblique",
                (FontKind::Serif, false, false) => "Times-Roman",
                (FontKind::Serif, true, false) => "Times-Bold",
                (FontKind::Serif, false, true) => "Times-Italic",
                (FontKind::Serif, true, true) => "Times-BoldItalic",
                (FontKind::Mono, false, false) => "Courier",
                (FontKind::Mono, true, false) => "Courier-Bold",
                (FontKind::Mono, false, true) => "Courier-Oblique",
                (FontKind::Mono, true, true) => "Courier-BoldOblique",
            };
            let f = doc.standard_font(name);
            return (!f.is_null()).then_some(f);
        }
        let kind = match s.font {
            FontKind::Sans => "sans",
            FontKind::Serif => "serif",
            FontKind::Mono => "mono",
        };
        fonts::generic(kind, s.bold, s.italic).and_then(|p| doc.truetype_font(p))
    }

    /// Add new text with its first baseline at `at` (user space), one object per line.
    pub fn add_text(&mut self, page: usize, at: (f32, f32), text: &str) {
        let Some(f) = &self.file else { return };
        let Some(font) = self.new_text_font(&f.doc, text) else {
            self.error = Some("No font was found for this text.".into());
            return;
        };
        let (size, color) = (self.style.text_size, self.style.text_color);
        let Some(g) = f.geom(page) else { return };
        let size = size / g.unit().max(1e-3);
        // Text reads left to right on screen even on a rotated page: its x axis follows the view's.
        let o = g.to_user(0.0, 0.0);
        let r = g.to_user(1.0, 0.0);
        let d = g.to_user(0.0, 1.0);
        let norm = |v: (f32, f32)| {
            let l = (v.0 * v.0 + v.1 * v.1).sqrt().max(1e-6);
            (v.0 / l, v.1 / l)
        };
        let right = norm((r.0 - o.0, r.1 - o.1));
        let up = norm((o.0 - d.0, o.1 - d.1));
        let added = self.apply(page, |p| {
            let mut ix = Vec::new();
            for (k, line) in text.lines().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let dy = k as f32 * size * 1.25;
                let m = [right.0, right.1, up.0, up.1, at.0 - up.0 * dy, at.1 - up.1 * dy];
                if let Some(i) = p.add_text(font, size, line, m, color) {
                    ix.push(i);
                }
            }
            ix
        });
        if let Some(items) = added.filter(|v| !v.is_empty()) {
            self.sel = Sel::Objects { page, items };
        }
    }

    /// Replace the text of a text object. A single line made only of characters the page already
    /// shows in that font keeps the original font; anything else is redrawn in the closest
    /// installed font (with extra lines below the first).
    pub fn set_object_text(&mut self, page: usize, obj: usize, text: &str) {
        let Some(f) = &self.file else { return };
        let Some(p) = f.doc.page(page) else { return };
        let original = p.text(obj);
        let (info, size, m, color) = (p.font(obj), p.font_size(obj), p.matrix(obj), p.fill_color(obj).unwrap_or([0, 0, 0, 255]));
        drop(p);
        if original.trim_end() == text.trim_end() {
            return;
        }
        if !text.contains('\n')
            && let Some(codes) = PageModel::load(f, page).ok().and_then(|m| m.encode(f, page, obj, text))
        {
            self.edit_object(page, obj, Edit::Text(codes));
            return;
        }
        let Some(path) = info.as_ref().and_then(fonts::closest).or_else(|| fonts::generic("sans", false, false)) else {
            self.error = Some("No installed font can show this text.".into());
            return;
        };
        if !self.surgery(page, vec![(obj, Edit::Delete)], Vec::new()) {
            return;
        }
        let Some(font) = self.file.as_ref().and_then(|f| f.doc.truetype_font(path)) else {
            self.error = Some(format!("{} could not be loaded.", path.display()));
            return;
        };
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let added = self.apply(page, |p| {
            let mut ix = Vec::new();
            for (k, line) in lines.iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let d = k as f32 * size * 1.25;
                let mm = [m[0], m[1], m[2], m[3], m[4] - m[2] * d, m[5] - m[3] * d];
                if let Some(i) = p.add_text(font, size, line, mm, color) {
                    ix.push(i);
                }
            }
            ix
        });
        if let Some(items) = added.filter(|v| !v.is_empty()) {
            self.sel = Sel::Objects { page, items };
        }
        let from = info.as_ref().map(|i| i.clean_name().to_string()).unwrap_or_else(|| "The original font".into());
        self.say(format!("{from} cannot show this text, so it now uses {}.", path.file_stem().unwrap_or_default().to_string_lossy()));
    }

    // ── shapes and images ──

    pub fn add_path(&mut self, page: usize, segs: Vec<pdf::Seg>, style: PathStyle) {
        if let Some(Some(i)) = self.apply(page, |p| p.add_path(&segs, style)) {
            self.sel = Sel::Objects { page, items: vec![i] };
        }
    }

    pub fn pick_image(&mut self) -> bool {
        let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"]).pick_file() else {
            return false;
        };
        match PendingImage::load(&path) {
            Ok(img) => {
                self.say(format!("Click on a page to place {}", img.name));
                self.image = Some(img);
                true
            }
            Err(e) => {
                self.error = Some(format!("Could not read {}: {e}", path.display()));
                false
            }
        }
    }

    /// Place the pending image in `rect` (user space x, y, w, h).
    pub fn place_image(&mut self, page: usize, rect: [f32; 4]) {
        let Some(img) = self.image.take() else { return };
        if let Some(Some(i)) = self.apply(page, |p| p.add_image(&img.as_data(), rect)) {
            self.sel = Sel::Objects { page, items: vec![i] };
            self.tool = Tool::Select;
        } else {
            self.error = Some("The image could not be added.".into());
        }
    }

    pub fn replace_image(&mut self, page: usize, obj: usize) {
        let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"]).pick_file() else {
            return;
        };
        let img = match PendingImage::load(&path) {
            Ok(img) => img,
            Err(e) => {
                self.error = Some(format!("Could not read {}: {e}", path.display()));
                return;
            }
        };
        let Some(f) = &self.file else { return };
        let model = match PageModel::load(f, page) {
            Ok(m) => m,
            Err(e) => {
                self.error = Some(format!("The image could not be replaced. {e}"));
                return;
            }
        };
        let is_xobject = model.parsed.objs.get(obj).is_some_and(|o| model.parsed.ops[o.first].name == b"Do");
        if !is_xobject {
            self.error =
                Some("This picture is stored inside the page's drawing commands, so it cannot be swapped. Delete it and add a new image instead.".into());
            return;
        }
        let stream = match &img.data {
            ImageBytes::Jpeg(b) => incremental::image_xobject(Some(b), img.w, img.h, None),
            ImageBytes::Rgba(px) => incremental::image_xobject(None, img.w, img.h, Some(px)),
        };
        let Some(stream) = stream else {
            self.error = Some("The image could not be prepared.".into());
            return;
        };
        let name = (1..).map(|n| format!("DkIm{n}").into_bytes()).find(|n| !model.data.xobjects.contains_key(n)).unwrap();
        if self.surgery(page, vec![(obj, Edit::XObject(name.clone()))], vec![(name, stream)]) {
            self.sel = Sel::Objects { page, items: vec![obj] };
        }
    }

    // ── annotations ──

    pub fn add_markup(&mut self, page: usize, boxes: Vec<[f32; 4]>) {
        let (subtype, color) = (self.style.markup.subtype(), self.style.highlight);
        if let Some(Some(i)) = self.apply(page, |p| p.add_markup(subtype, &boxes, color)) {
            self.sel = Sel::Annot { page, index: i };
        }
    }

    pub fn add_ink(&mut self, page: usize, stroke: Vec<(f32, f32)>) {
        let (color, width) = (self.style.ink, self.style.ink_width);
        self.apply(page, |p| p.add_ink(&[stroke], color, width));
    }

    pub fn add_stamp(&mut self, page: usize, center: (f32, f32)) {
        let label = self.style.stamp.trim().to_uppercase();
        if label.is_empty() {
            return;
        }
        let h = 34.0;
        let w = pdf::estimate_width(&label, h * 0.48) + h * 0.9;
        let rect = [center.0 - w / 2.0, center.1 - h / 2.0, center.0 + w / 2.0, center.1 + h / 2.0];
        let color = self.style.stamp_color;
        if let Some(Some(i)) = self.apply(page, |p| p.add_stamp(rect, &label, color)) {
            self.sel = Sel::Annot { page, index: i };
        }
    }

    pub fn begin_note(&mut self, page: usize, at: (f32, f32)) {
        self.commit_edits();
        self.note = Some(NoteDraft { page, at, index: None, text: String::new(), focus: true, kind: DraftKind::Note });
    }

    pub fn edit_note(&mut self, page: usize, index: usize) {
        self.commit_edits();
        let Some(a) = self.file.as_ref().and_then(|f| f.annots(page).get(index).cloned()) else { return };
        let kind = if a.subtype == pdf::ANNOT_FREETEXT { DraftKind::TextBox { rect: a.rect, callout: None } } else { DraftKind::Note };
        self.note = Some(NoteDraft { page, at: (a.rect[0], a.rect[3]), index: Some(index), text: a.contents, focus: true, kind });
    }

    pub fn commit_note(&mut self) {
        let Some(n) = self.note.take() else { return };
        match n.index {
            Some(i) => {
                let old = self.file.as_ref().and_then(|f| f.annots(n.page).get(i).map(|a| a.contents.clone()));
                if old.as_deref() != Some(n.text.as_str()) {
                    self.set_annot_contents(n.page, i, &n.text);
                }
            }
            None if matches!(n.kind, DraftKind::TextBox { .. }) => {
                let DraftKind::TextBox { rect, callout } = n.kind else { return };
                if n.text.trim().is_empty() {
                    return;
                }
                let s = &self.style;
                let mut d = annots::text_box(rect, &n.text, s.box_size, s.text_color, s.review_color, callout);
                let ap = annots::build(&mut d);
                if let Some(i) = self.file_update(&[n.page], |u| u.push_annot(n.page, d, ap).map(|(_, i)| i)) {
                    self.sel = Sel::Annot { page: n.page, index: i };
                    if !annots::latin(&n.text) {
                        self.say("Some characters cannot be shown in a text box and appear as \"?\".");
                    }
                }
            }
            None => {
                if n.text.trim().is_empty() {
                    return;
                }
                let color = self.style.note;
                if let Some(Some(i)) = self.apply(n.page, |p| p.add_note(n.at.0, n.at.1, &n.text, color)) {
                    self.sel = Sel::Annot { page: n.page, index: i };
                }
            }
        }
    }

    pub fn set_annot_contents(&mut self, page: usize, index: usize, text: &str) {
        if self.drawn_here(page, index) {
            self.file_update(&[page], |u| {
                let id = u.annot_id(page, index)?;
                u.modify(id, |d| d.set("Contents", annots::text_string(text)))
            });
            return;
        }
        self.apply(page, |p| p.set_annot_contents(index, text));
    }

    pub fn set_annot_color(&mut self, page: usize, index: usize, c: Rgba) {
        if self.drawn_here(page, index) {
            let col = lopdf::Object::Array([c[0], c[1], c[2]].iter().map(|&v| lopdf::Object::Real(v as f32 / 255.0)).collect());
            self.file_update(&[page], |u| {
                let id = u.annot_id(page, index)?;
                u.modify(id, |d| {
                    d.set("C", col.clone());
                    if d.has(b"IC") {
                        d.set("IC", col);
                    }
                })
            });
            return;
        }
        self.apply(page, |p| p.set_annot_color(index, c));
    }

    /// Arrows, measurements, text boxes and callouts, whose appearance Dockit draws itself.
    fn drawn_here(&self, page: usize, index: usize) -> bool {
        self.file.as_ref().is_some_and(|f| f.annots(page).get(index).is_some_and(|a| matches!(a.subtype, pdf::ANNOT_LINE | pdf::ANNOT_FREETEXT)))
    }

    /// Change the file directly (see annots.rs), as an incremental update, then show the result.
    /// `pages` are the pages whose annotations change.
    pub fn file_update<R>(&mut self, pages: &[usize], change: impl FnOnce(&mut annots::Update) -> Result<R, String>) -> Option<R> {
        let f = self.file.as_ref()?;
        let was_encrypted = f.doc.encrypted();
        let permissions = f.doc.permissions();
        let result = f.doc.save_plain().and_then(annots::Update::load).and_then(|mut u| {
            let r = change(&mut u)?;
            Ok((u.finish()?, r))
        });
        let (bytes, r) = match result {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(format!("This change could not be made. {e}"));
                return None;
            }
        };
        let Ok(doc) = Doc::open(bytes, None) else {
            self.error = Some("This change could not be made: the updated file could not be read back.".into());
            return None;
        };
        let f = self.file.as_mut().unwrap();
        f.doc = doc;
        for &p in pages {
            f.invalidate(p);
        }
        if was_encrypted {
            f.keep_protection(permissions);
            self.say("This file is password protected. It stays protected with the same password when you save it.");
        }
        self.needs_commit = true;
        Some(r)
    }

    pub fn add_arrow(&mut self, page: usize, a: (f32, f32), b: (f32, f32)) {
        let s = &self.style;
        let mut d = annots::arrow(a, b, s.review_color, s.review_width);
        let ap = annots::build(&mut d);
        if let Some(i) = self.file_update(&[page], |u| u.push_annot(page, d, ap).map(|(_, i)| i)) {
            self.sel = Sel::Annot { page, index: i };
        }
    }

    pub fn add_measure(&mut self, page: usize, a: (f32, f32), b: (f32, f32)) {
        let s = &self.style;
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        let label = measure_label(len, s.unit, s.scale);
        let ratio = if (s.scale - 1.0).abs() < 1e-6 { "1:1".to_string() } else { format!("1:{}", s.scale) };
        let mut d = annots::dimension(a, b, s.review_color, s.review_width, &label, s.unit.label(), s.unit.per_point() * s.scale, &ratio);
        let ap = annots::build(&mut d);
        if let Some(i) = self.file_update(&[page], |u| u.push_annot(page, d, ap).map(|(_, i)| i)) {
            self.sel = Sel::Annot { page, index: i };
            self.say(format!("Measured {label}"));
        }
    }

    /// Start typing a new text box (or callout pointing at `callout`).
    pub fn begin_text_box(&mut self, page: usize, rect: [f32; 4], callout: Option<(f32, f32)>) {
        self.commit_edits();
        self.note = Some(NoteDraft { page, at: (rect[0], rect[3]), index: None, text: String::new(), focus: true, kind: DraftKind::TextBox { rect, callout } });
    }

    /// Reply to the annotation `parent` on `page`.
    pub fn add_reply(&mut self, page: usize, parent: usize, text: &str) {
        let Some(rect) = self.file.as_ref().and_then(|f| f.annots(page).get(parent).map(|a| a.rect)) else { return };
        if self
            .file_update(&[page], |u| {
                let pid = u.annot_id(page, parent)?;
                let (d, ap) = annots::reply(pid, rect, text, None);
                u.push_annot(page, d, Some(ap))
            })
            .is_some()
        {
            self.say("Reply added");
        }
    }

    /// Mark a comment resolved (or open again).
    pub fn set_resolved(&mut self, page: usize, parent: usize, done: bool) {
        let Some(rect) = self.file.as_ref().and_then(|f| f.annots(page).get(parent).map(|a| a.rect)) else { return };
        let state = if done { "Completed" } else { "None" };
        if self
            .file_update(&[page], |u| {
                let pid = u.annot_id(page, parent)?;
                let (d, ap) = annots::reply(pid, rect, "", Some(state));
                u.push_annot(page, d, Some(ap))
            })
            .is_some()
        {
            self.say(if done { "Marked as resolved" } else { "Opened again" });
        }
    }

    /// A new form field of the chosen kind in `rect` (l, b, r, t).
    pub fn add_field(&mut self, page: usize, rect: [f32; 4]) {
        let kind = self.style.field;
        let r = self.file_update(&[page], |u| {
            let names = u.field_names();
            let base = match kind {
                NewField::Text => "Text",
                NewField::Checkbox => "Checkbox",
                NewField::Dropdown => "Choice",
                NewField::Signature => "Signature",
            };
            let field_name = (1..).map(|k| format!("{base} {k}")).find(|nm| !names.contains(nm)).unwrap();
            u.add_field(page, kind, rect, &field_name).map(|i| (i, field_name))
        });
        if let Some((i, field_name)) = r {
            self.sel = Sel::Annot { page, index: i };
            self.say(format!("Added {field_name}. Rename it or change its choices in Properties."));
        }
    }

    pub fn rename_field(&mut self, page: usize, index: usize, new: &str) {
        let new = new.trim();
        if new.is_empty() {
            return;
        }
        self.file_update(&[page], |u| {
            if u.field_names().iter().any(|n| n == new) {
                return Err(format!("A field called \"{new}\" already exists."));
            }
            let id = u.annot_id(page, index)?;
            u.modify(id, |d| d.set("T", annots::text_string(new)))
        });
    }

    pub fn set_field_choices(&mut self, page: usize, index: usize, choices: &[String]) {
        let opt = lopdf::Object::Array(choices.iter().filter(|c| !c.trim().is_empty()).map(|c| annots::text_string(c.trim())).collect());
        self.file_update(&[page], |u| {
            let id = u.annot_id(page, index)?;
            u.modify(id, |d| {
                d.set("Opt", opt);
                d.set("V", annots::text_string(""));
            })
        });
    }

    /// Draw every comment, mark and form field into the pages, so they can no longer be changed.
    /// Links keep working.
    pub fn flatten(&mut self) {
        self.commit_edits();
        let Some(n) = self.file.as_ref().map(|f| f.page_count()) else { return };
        let pages: Vec<usize> = (0..n).collect();
        let Some(drawn) = self.file_update(&pages, |u| {
            let mut drawn = 0;
            for p in 0..n {
                drawn += u.flatten_page(p)?;
            }
            if drawn > 0 {
                u.remove_form()?;
            }
            Ok(drawn)
        }) else {
            return;
        };
        self.clear_transient();
        self.epoch += 1;
        if drawn == 0 {
            self.say("There is nothing to flatten.");
        } else {
            self.say(format!("Flattened {}", plural(drawn, "comment, mark or field", "comments, marks and fields")));
        }
    }

    // ── form fields ──

    pub fn begin_field_edit(&mut self, page: usize, index: usize) {
        self.commit_edits();
        let Some(a) = self.file.as_ref().and_then(|f| f.annots(page).get(index).cloned()) else { return };
        let Some(field) = a.field else { return };
        self.field_edit = Some(FieldEdit { page, index, text: field.value, multiline: field.flags & pdf::FIELD_FLAG_MULTILINE != 0, focus: true });
    }

    pub fn commit_field(&mut self) {
        let Some(e) = self.field_edit.take() else { return };
        self.set_field_text(e.page, e.index, &e.text);
    }

    pub fn set_field_text(&mut self, page: usize, index: usize, text: &str) {
        let old = self.file.as_ref().and_then(|f| f.annots(page).get(index).and_then(|a| a.field.as_ref().map(|x| x.value.clone())));
        if old.as_deref() == Some(text) {
            return;
        }
        self.apply(page, |p| p.set_field_text(index, text));
    }

    pub fn click_field(&mut self, page: usize, index: usize) {
        self.apply(page, |p| p.click_field(index));
    }

    pub fn select_option(&mut self, page: usize, index: usize, option: usize) {
        self.apply(page, |p| p.select_option(index, option));
    }

    // ── signatures ──

    pub fn place_signature(&mut self, page: usize, center: (f32, f32)) {
        let Some(sig) = self.signature.clone() else {
            self.signature_dialog = true;
            return;
        };
        let Some(f) = &self.file else { return };
        let unit = f.geom(page).map_or(1.0, |g| g.unit()).max(1e-3);
        match sig {
            Signature::Drawn { strokes, aspect } => {
                let w = 170.0 / unit;
                let h = w / aspect.max(0.1);
                let (x0, y0) = (center.0 - w / 2.0, center.1 + h / 2.0);
                let mut segs = Vec::new();
                for s in &strokes {
                    let mut pts = s.iter().map(|&(x, y)| (x0 + x * h, y0 - y * h));
                    if let Some((x, y)) = pts.next() {
                        segs.push(pdf::Seg::Move(x, y));
                        // A dot (single point) still needs a visible mark.
                        segs.push(pdf::Seg::Line(x + 0.01, y));
                        for (x, y) in pts {
                            segs.push(pdf::Seg::Line(x, y));
                        }
                    }
                }
                let style = PathStyle { fill: None, stroke: Some([20, 30, 70, 255]), width: (h * 0.035).max(0.8) };
                self.add_path(page, segs, style);
            }
            Signature::Typed { text } => {
                let Some(font) = fonts::script().or_else(|| fonts::generic("serif", false, true)).and_then(|p| f.doc.truetype_font(p)) else {
                    self.error = Some("No handwriting font is installed.".into());
                    return;
                };
                let size = 30.0 / unit;
                let w = pdf::estimate_width(&text, size) * 0.85;
                let at = [1.0, 0.0, 0.0, 1.0, center.0 - w / 2.0, center.1 - size * 0.3];
                let added = self.apply(page, |p| p.add_text(font, size, &text, at, [20, 30, 70, 255]));
                if let Some(Some(i)) = added {
                    self.sel = Sel::Objects { page, items: vec![i] };
                }
            }
            Signature::Image { path } => match PendingImage::load(&path) {
                Ok(img) => {
                    let w = 170.0 / unit;
                    let h = w * img.h as f32 / img.w.max(1) as f32;
                    self.image = Some(img);
                    let tool = self.tool;
                    self.place_image(page, [center.0 - w / 2.0, center.1 - h / 2.0, w, h]);
                    self.tool = tool;
                }
                Err(e) => {
                    self.error = Some(format!("The signature image could not be read: {e}"));
                    self.signature = None;
                }
            },
        }
        self.say("Signature placed. Drag to move it, or drag a corner to resize it.");
        self.tool = Tool::Select;
    }

    // ── pages ──

    pub fn selected_pages(&self) -> Vec<usize> {
        let mut v = self.page_sel.clone();
        if v.is_empty() {
            v.push(self.current);
        }
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn rotate_pages(&mut self, turns: i32) {
        let pages = self.selected_pages();
        let n = pages.len();
        self.apply_doc(|f| {
            for &i in &pages {
                f.doc.rotate_page(i, turns);
            }
        });
        self.say(format!("Rotated {}", plural(n, "page", "pages")));
    }

    pub fn delete_pages(&mut self) {
        let pages = self.selected_pages();
        let total = self.file.as_ref().map_or(0, |f| f.page_count());
        if pages.len() >= total {
            self.error = Some("A PDF needs at least one page, so the last pages cannot all be deleted.".into());
            return;
        }
        let n = pages.len();
        self.apply_doc(|f| {
            for &i in pages.iter().rev() {
                f.doc.delete_page(i);
            }
        });
        self.page_sel.clear();
        let total = self.file.as_ref().map_or(0, |f| f.page_count());
        self.current = self.current.min(total.saturating_sub(1));
        self.say(format!("Deleted {}", plural(n, "page", "pages")));
    }

    /// Insert a blank page after `after`, the same size as it.
    pub fn insert_blank(&mut self, after: usize) {
        let Some(f) = &self.file else { return };
        let (w, h) = f.sizes.get(after).copied().unwrap_or((595.0, 842.0));
        self.apply_doc(|f| f.doc.insert_blank(after + 1, w, h));
        self.page_sel = vec![after + 1];
        self.goto = Some(after + 1);
    }

    pub fn duplicate_pages(&mut self) {
        let pages = self.selected_pages();
        let Some(copy) = self.file.as_ref().and_then(|f| f.snapshot()) else { return };
        let at = pages.last().copied().unwrap_or(0) + 1;
        let n = pages.len();
        self.apply_doc(|f| f.doc.import(&copy, Some(&pages), at));
        self.page_sel = (at..at + n).collect();
        self.say(format!("Duplicated {}", plural(n, "page", "pages")));
    }

    /// Insert every page of PDF files picked by the user after page `after`.
    pub fn insert_from_files(&mut self, after: usize) {
        let Some(paths) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_files() else { return };
        let mut at = after + 1;
        let mut added = 0;
        let mut failed = Vec::new();
        for path in paths {
            let Ok(bytes) = std::fs::read(&path) else {
                failed.push(path);
                continue;
            };
            let Ok(src) = Doc::open(bytes, None) else {
                failed.push(path);
                continue;
            };
            let n = src.page_count();
            if self.apply_doc(|f| f.doc.import(&src, None, at)) == Some(true) {
                at += n;
                added += n;
            } else {
                failed.push(path);
            }
        }
        if added > 0 {
            self.page_sel = (after + 1..after + 1 + added).collect();
            self.goto = Some(after + 1);
            self.say(format!("Inserted {}", plural(added, "page", "pages")));
        }
        if !failed.is_empty() {
            let names: Vec<String> = failed.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
            self.error = Some(format!("These files could not be inserted (damaged or password protected): {}", names.join(", ")));
        }
    }

    /// Move `pages` so they sit, in order, before the page that was at `before` (or at the end).
    pub fn move_pages(&mut self, pages: Vec<usize>, before: usize) {
        let mut pages = pages;
        pages.sort_unstable();
        pages.dedup();
        let dest = before - pages.iter().filter(|&&p| p < before).count();
        if pages.iter().enumerate().all(|(k, &p)| p == dest + k) {
            return;
        }
        let n = pages.len();
        if self.apply_doc(|f| f.doc.move_pages(&pages, dest)) == Some(true) {
            self.page_sel = (dest..dest + n).collect();
            self.say(format!("Moved {}", plural(n, "page", "pages")));
        }
    }

    /// Save the selected pages as a new PDF.
    pub fn extract_pages(&mut self) {
        let pages = self.selected_pages();
        let Some(f) = &self.file else { return };
        let stem = f.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into());
        let name = format!("{stem} pages {}.pdf", page_ranges(&pages));
        let Some(target) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(name).save_file() else { return };
        let out = Doc::new_empty();
        if !out.import(&f.doc, Some(&pages), 0) {
            self.error = Some("The pages could not be copied.".into());
            return;
        }
        match out.save().and_then(|b| std::fs::write(&target, b).map_err(|e| e.to_string())) {
            Ok(()) => self.say(format!("Saved {} to {}", plural(pages.len(), "page", "pages"), target.file_name().unwrap_or_default().to_string_lossy())),
            Err(e) => self.error = Some(format!("Could not save {}: {e}", target.display())),
        }
    }
}

/// For files with /NeedAppearances: draw every annotation's appearance once (by loading each
/// page), then clear the flag, so PDFium does not redraw them on every load and leave copies
/// behind. Other files come back unchanged.
fn settle_appearances(doc: Doc, bytes: Vec<u8>, password: Option<&str>) -> (Doc, Vec<u8>) {
    if !bytes.windows(15).any(|w| w == b"NeedAppearances") || doc.encrypted() {
        return (doc, bytes);
    }
    for i in 0..doc.page_count() {
        drop(doc.page(i));
    }
    let Some(fixed) = doc.save().ok().and_then(|b| incremental::clear_need_appearances(&b)) else { return (doc, bytes) };
    match Doc::open(fixed.clone(), password) {
        Ok(d) => (d, fixed),
        Err(_) => (doc, bytes),
    }
}

/// "1-3, 5" for pages [0, 1, 2, 4].
pub fn page_ranges(pages: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let start = pages[i];
        let mut end = start;
        while i + 1 < pages.len() && pages[i + 1] == end + 1 {
            i += 1;
            end = pages[i];
        }
        out.push(if start == end { format!("{}", start + 1) } else { format!("{}-{}", start + 1, end + 1) });
        i += 1;
    }
    out.join(", ")
}

/// Parse "1-3, 5, 8-" style ranges (1-based, inclusive) into groups of 0-based page indices.
pub fn parse_ranges(s: &str, count: usize) -> Result<Vec<Vec<usize>>, String> {
    let mut groups = Vec::new();
    for part in s.split([',', ';']).map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let parse = |t: &str, default: usize| -> Result<usize, String> {
            if t.is_empty() {
                return Ok(default);
            }
            t.parse::<usize>().map_err(|_| format!("\"{part}\" is not a page range."))
        };
        let (a, b) = (parse(a, 1)?, parse(b, count)?);
        if a == 0 || b == 0 || a > count || b > count {
            return Err(format!("\"{part}\" is outside pages 1 to {count}."));
        }
        let g: Vec<usize> = if a <= b { (a - 1..b).collect() } else { (b - 1..a).rev().collect() };
        groups.push(g);
    }
    if groups.is_empty() {
        return Err("Enter at least one page range, for example 1-3, 4-10.".into());
    }
    Ok(groups)
}
