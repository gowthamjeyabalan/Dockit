//! A small safe layer over PDFium's C API. pdfium-render supplies the raw function table
//! (`PdfiumLibraryBindings`); everything here owns its handles and closes them on drop.
//!
//! Coordinates: "user" space is PDF points with the origin at the bottom left of the unrotated
//! page. "View" space is points with the origin at the top left of the page as displayed (after
//! its /Rotate). `Geom` converts between the two.

use pdfium_render::prelude::{
    FPDF_DOCUMENT, FPDF_FILEACCESS, FPDF_FILEWRITE, FPDF_FONT, FPDF_FORMFILLINFO, FPDF_FORMHANDLE, FPDF_PAGE, FPDF_PAGEOBJECT, FS_MATRIX, FS_POINTF,
    FPDF_COLORSCHEME, FS_QUADPOINTSF, IFSDK_PAUSE, FS_RECTF, FS_SIZEF, Pdfium, PdfiumLibraryBindings,
};
use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_uchar, c_ulong, c_void};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

struct Lib(Box<dyn PdfiumLibraryBindings>);
// PDFium is only ever called from the UI thread.
unsafe impl Sync for Lib {}
unsafe impl Send for Lib {}

static LIB: OnceLock<Lib> = OnceLock::new();

/// Load pdfium.dll from `dir` and initialise it. Must succeed before anything else here is used.
pub fn init(dir: &Path) -> Result<(), String> {
    let path = Pdfium::pdfium_platform_library_name_at_path(dir);
    let bindings = Pdfium::bind_to_library(&path).map_err(|e| format!("{} could not be loaded ({e:?})", path.display()))?;
    unsafe { bindings.FPDF_InitLibrary() };
    let _ = LIB.set(Lib(bindings));
    Ok(())
}

fn lib() -> &'static dyn PdfiumLibraryBindings {
    LIB.get().expect("PDFium is loaded at start-up").0.as_ref()
}

// ── constants from the PDFium headers ──

pub const ANNOT_TEXT: i32 = 1;
pub const ANNOT_LINE: i32 = 4;
pub const ANNOT_LINK: i32 = 2;
pub const ANNOT_FREETEXT: i32 = 3;
pub const ANNOT_SQUARE: i32 = 5;
pub const ANNOT_CIRCLE: i32 = 6;
pub const ANNOT_HIGHLIGHT: i32 = 9;
pub const ANNOT_UNDERLINE: i32 = 10;
pub const ANNOT_SQUIGGLY: i32 = 11;
pub const ANNOT_STRIKEOUT: i32 = 12;
pub const ANNOT_STAMP: i32 = 13;
pub const ANNOT_INK: i32 = 15;
pub const ANNOT_POPUP: i32 = 16;
pub const ANNOT_WIDGET: i32 = 20;
pub const ANNOT_REDACT: i32 = 28;

pub const FIELD_PUSHBUTTON: i32 = 1;
pub const FIELD_CHECKBOX: i32 = 2;
pub const FIELD_RADIO: i32 = 3;
pub const FIELD_COMBO: i32 = 4;
pub const FIELD_LIST: i32 = 5;
pub const FIELD_TEXT: i32 = 6;
pub const FIELD_SIGNATURE: i32 = 7;

pub const FIELD_FLAG_READONLY: i32 = 1;
pub const FIELD_FLAG_MULTILINE: i32 = 1 << 12;

const FLAG_ANNOT: c_int = 0x01;
const FLAG_PRINTING: c_int = 0x800;
/// The page background in dark reading, ARGB.
pub const DARK_PAGE: u32 = 0xFF1E1F22;
const FLAG_REVERSE_BYTE_ORDER: c_int = 0x10;
const BITMAP_BGRA: c_int = 4;
const BITMAP_BGRX: c_int = 3;
const SAVE_NO_INCREMENTAL: c_ulong = 2;
const SAVE_REMOVE_SECURITY: c_ulong = 3;
const FONT_TRUETYPE: c_int = 2;
const ANNOT_FLAG_PRINT: c_int = 4;
const COLORTYPE_COLOR: u32 = 0;

pub type Rgba = [u8; 4];

// ── strings ──

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Calls a PDFium "fill this UTF-16 buffer" function twice: once for the size in bytes, once for the text.
fn read_wide(f: impl Fn(*mut u16, c_ulong) -> c_ulong) -> String {
    let n = f(null_mut(), 0) as usize;
    if n <= 2 {
        return String::new();
    }
    let mut buf = vec![0u16; n.div_ceil(2)];
    f(buf.as_mut_ptr(), (buf.len() * 2) as c_ulong);
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Same as `read_wide` for PDFium functions that return Latin-1/UTF-8 `char` strings.
fn read_narrow(f: impl Fn(*mut std::ffi::c_char, usize) -> usize) -> String {
    let n = f(null_mut(), 0);
    if n <= 1 {
        return String::new();
    }
    let mut buf = vec![0u8; n];
    f(buf.as_mut_ptr() as *mut std::ffi::c_char, n);
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

// ── documents ──

/// A signature as the file stores it.
pub struct RawSignature {
    /// The CMS blob (with any zero padding).
    pub contents: Vec<u8>,
    pub byte_range: Vec<i32>,
    pub reason: String,
    /// PDF date string.
    pub time: String,
    /// Certifying signature's permission (1 to 3), or 0.
    pub mdp: u32,
}

#[derive(Debug)]
pub enum OpenError {
    Password,
    Failed(String),
}

pub struct Doc {
    h: FPDF_DOCUMENT,
    form: FPDF_FORMHANDLE,
    _form_info: Box<FPDF_FORMFILLINFO>,
    /// PDFium reads from this buffer for as long as the document is open.
    _bytes: Vec<u8>,
    /// Fonts loaded into this document, with their file data kept alive alongside.
    fonts: RefCell<Vec<(String, FPDF_FONT, Vec<u8>)>>,
}

impl Drop for Doc {
    fn drop(&mut self) {
        let l = lib();
        unsafe {
            for (_, f, _) in self.fonts.borrow().iter() {
                l.FPDFFont_Close(*f);
            }
            if !self.form.is_null() {
                l.FPDFDOC_ExitFormFillEnvironment(self.form);
            }
            l.FPDF_CloseDocument(self.h);
        }
    }
}

#[repr(C)]
struct Writer {
    fw: FPDF_FILEWRITE,
    out: Vec<u8>,
}

unsafe extern "C" fn write_block(this: *mut FPDF_FILEWRITE, data: *const c_void, size: c_ulong) -> c_int {
    let w = unsafe { &mut *(this as *mut Writer) };
    if size > 0 {
        w.out.extend_from_slice(unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) });
    }
    1
}

struct Source {
    ptr: *const u8,
    len: usize,
}

unsafe extern "C" fn get_block(param: *mut c_void, pos: c_ulong, buf: *mut c_uchar, size: c_ulong) -> c_int {
    let src = unsafe { &*(param as *const Source) };
    let (pos, size) = (pos as usize, size as usize);
    if pos + size > src.len {
        return 0;
    }
    unsafe { std::ptr::copy_nonoverlapping(src.ptr.add(pos), buf, size) };
    1
}

impl Doc {
    pub fn open(bytes: Vec<u8>, password: Option<&str>) -> Result<Doc, OpenError> {
        let h = unsafe { lib().FPDF_LoadMemDocument64(&bytes, password) };
        if h.is_null() {
            let code = unsafe { lib().FPDF_GetLastError() };
            return Err(match code {
                4 => OpenError::Password,
                3 => OpenError::Failed("The file is not a PDF or is damaged.".into()),
                5 => OpenError::Failed("The file uses a security method Dockit cannot open.".into()),
                _ => OpenError::Failed("The file could not be read.".into()),
            });
        }
        Ok(Self::wrap(h, bytes))
    }

    pub fn new_empty() -> Doc {
        Self::wrap(unsafe { lib().FPDF_CreateNewDocument() }, Vec::new())
    }

    fn wrap(h: FPDF_DOCUMENT, bytes: Vec<u8>) -> Doc {
        // The form-fill module loads widgets so fields render and can be filled. No callbacks are needed.
        let mut info = Box::new(FPDF_FORMFILLINFO {
            version: 2,
            Release: None,
            FFI_Invalidate: None,
            FFI_OutputSelectedRect: None,
            FFI_SetCursor: None,
            FFI_SetTimer: None,
            FFI_KillTimer: None,
            FFI_GetLocalTime: None,
            FFI_OnChange: None,
            FFI_GetPage: None,
            FFI_GetCurrentPage: None,
            FFI_GetRotation: None,
            FFI_ExecuteNamedAction: None,
            FFI_SetTextFieldFocus: None,
            FFI_DoURIAction: None,
            FFI_DoGoToAction: None,
            m_pJsPlatform: null_mut(),
            xfa_disabled: 0,
            FFI_DisplayCaret: None,
            FFI_GetCurrentPageIndex: None,
            FFI_SetCurrentPage: None,
            FFI_GotoURL: None,
            FFI_GetPageViewRect: None,
            FFI_PageEvent: None,
            FFI_PopupMenu: None,
            FFI_OpenFile: None,
            FFI_EmailTo: None,
            FFI_UploadTo: None,
            FFI_GetPlatform: None,
            FFI_GetLanguage: None,
            FFI_DownloadFromURL: None,
            FFI_PostRequestURL: None,
            FFI_PutRequestURL: None,
            FFI_OnFocusChange: None,
            FFI_DoURIActionWithKeyboardModifier: None,
        });
        let form = unsafe { lib().FPDFDOC_InitFormFillEnvironment(h, &mut *info) };
        if !form.is_null() {
            // A faint accent wash shows where fields are, as browsers do. (PDFium reads 0xBBGGRR.)
            unsafe {
                lib().FPDF_SetFormFieldHighlightColor(form, 0, 0xFF840A);
                lib().FPDF_SetFormFieldHighlightAlpha(form, 36);
            }
        }
        Doc { h, form, _form_info: info, _bytes: bytes, fonts: RefCell::new(Vec::new()) }
    }

    /// True when the document has fillable fields (an AcroForm or XFA form).
    pub fn has_form(&self) -> bool {
        !self.form.is_null() && unsafe { lib().FPDF_GetFormType(self.h) } != 0
    }

    pub fn page_count(&self) -> usize {
        unsafe { lib().FPDF_GetPageCount(self.h).max(0) as usize }
    }

    /// Displayed page size in points (rotation applied), without loading the page.
    pub fn page_size(&self, index: usize) -> (f32, f32) {
        let mut s = FS_SIZEF { width: 612.0, height: 792.0 };
        unsafe { lib().FPDF_GetPageSizeByIndexF(self.h, index as c_int, &mut s) };
        (s.width.max(1.0), s.height.max(1.0))
    }

    pub fn page(&self, index: usize) -> Option<Page<'_>> {
        let h = unsafe { lib().FPDF_LoadPage(self.h, index as c_int) };
        if h.is_null() {
            return None;
        }
        if !self.form.is_null() {
            unsafe { lib().FORM_OnAfterLoadPage(h, self.form) };
        }
        Some(Page { doc: self, h, edited: Cell::new(false) })
    }

    /// The whole document as a PDF file.
    pub fn save(&self) -> Result<Vec<u8>, String> {
        if !self.form.is_null() {
            unsafe { lib().FORM_ForceToKillFocus(self.form) };
        }
        let mut w = Writer { fw: FPDF_FILEWRITE { version: 1, WriteBlock: Some(write_block) }, out: Vec::new() };
        let ok = unsafe { lib().FPDF_SaveAsCopy(self.h, &mut w.fw, SAVE_NO_INCREMENTAL) };
        if ok != 0 { Ok(w.out) } else { Err("PDFium could not write the document.".into()) }
    }

    /// The document without its security handler (for rewriting page content directly).
    pub fn save_plain(&self) -> Result<Vec<u8>, String> {
        if !self.form.is_null() {
            unsafe { lib().FORM_ForceToKillFocus(self.form) };
        }
        let mut w = Writer { fw: FPDF_FILEWRITE { version: 1, WriteBlock: Some(write_block) }, out: Vec::new() };
        let flags = if self.encrypted() { SAVE_REMOVE_SECURITY } else { SAVE_NO_INCREMENTAL };
        let ok = unsafe { lib().FPDF_SaveAsCopy(self.h, &mut w.fw, flags) };
        if ok != 0 { Ok(w.out) } else { Err("PDFium could not write the document.".into()) }
    }

    /// A document information entry ("Title", "Author", "Subject", "Keywords", "Creator",
    /// "Producer", "CreationDate", "ModDate").
    pub fn meta(&self, tag: &str) -> String {
        let l = lib();
        read_wide(|b, n| unsafe { l.FPDF_GetMetaText(self.h, tag, b as *mut c_void, n) })
    }

    /// Embedded files: their names and sizes.
    pub fn attachments(&self) -> Vec<(String, usize)> {
        let l = lib();
        let n = unsafe { l.FPDFDoc_GetAttachmentCount(self.h) }.max(0);
        (0..n)
            .filter_map(|i| unsafe {
                let a = l.FPDFDoc_GetAttachment(self.h, i);
                if a.is_null() {
                    return None;
                }
                let name = read_wide(|b, n| l.FPDFAttachment_GetName(a, b, n));
                let mut len: c_ulong = 0;
                l.FPDFAttachment_GetFile(a, null_mut(), 0, &mut len);
                Some((name, len as usize))
            })
            .collect()
    }

    /// The contents of embedded file `i`.
    pub fn attachment_data(&self, i: usize) -> Option<Vec<u8>> {
        let l = lib();
        unsafe {
            let a = l.FPDFDoc_GetAttachment(self.h, i as c_int);
            if a.is_null() {
                return None;
            }
            let mut len: c_ulong = 0;
            l.FPDFAttachment_GetFile(a, null_mut(), 0, &mut len);
            let mut buf = vec![0u8; len as usize];
            (l.FPDFAttachment_GetFile(a, buf.as_mut_ptr() as *mut c_void, len, &mut len) != 0).then_some(buf)
        }
    }

    pub fn add_attachment(&self, name: &str, data: &[u8]) -> bool {
        let l = lib();
        unsafe {
            let w = wide(name);
            let a = l.FPDFDoc_AddAttachment(self.h, w.as_ptr());
            !a.is_null() && l.FPDFAttachment_SetFile(a, self.h, data.as_ptr() as *const c_void, data.len() as c_ulong) != 0
        }
    }

    pub fn delete_attachment(&self, i: usize) -> bool {
        unsafe { lib().FPDFDoc_DeleteAttachment(self.h, i as c_int) != 0 }
    }

    /// The document's digital signatures, as stored.
    pub fn signatures(&self) -> Vec<RawSignature> {
        let l = lib();
        let n = unsafe { l.FPDF_GetSignatureCount(self.h) }.max(0);
        (0..n)
            .filter_map(|i| unsafe {
                let s = l.FPDF_GetSignatureObject(self.h, i);
                if s.is_null() {
                    return None;
                }
                let len = l.FPDFSignatureObj_GetContents(s, null_mut(), 0) as usize;
                let mut contents = vec![0u8; len];
                l.FPDFSignatureObj_GetContents(s, contents.as_mut_ptr() as *mut c_void, len as c_ulong);
                let count = l.FPDFSignatureObj_GetByteRange(s, null_mut(), 0) as usize;
                let mut byte_range = vec![0 as c_int; count];
                l.FPDFSignatureObj_GetByteRange(s, byte_range.as_mut_ptr(), count as c_ulong);
                let reason = read_wide(|b, n| l.FPDFSignatureObj_GetReason(s, b as *mut c_void, n));
                let time = read_narrow(|b, n| l.FPDFSignatureObj_GetTime(s, b, n as c_ulong) as usize);
                let mdp = l.FPDFSignatureObj_GetDocMDPPermission(s);
                Some(RawSignature { contents, byte_range, reason, time, mdp })
            })
            .collect()
    }

    /// The permission word (P) for how the document was opened; all bits set without restrictions.
    pub fn permissions(&self) -> u32 {
        unsafe { lib().FPDF_GetDocPermissions(self.h) as u32 }
    }

    pub fn encrypted(&self) -> bool {
        unsafe { lib().FPDF_GetSecurityHandlerRevision(self.h) != -1 }
    }

    /// Copy pages from `src` (all of them when `indices` is None) to position `at`.
    pub fn import(&self, src: &Doc, indices: Option<&[usize]>, at: usize) -> bool {
        let ok = match indices {
            Some(ix) => {
                let v: Vec<c_int> = ix.iter().map(|&i| i as c_int).collect();
                unsafe { lib().FPDF_ImportPagesByIndex(self.h, src.h, v.as_ptr(), v.len() as c_ulong, at as c_int) }
            }
            None => unsafe { lib().FPDF_ImportPagesByIndex(self.h, src.h, null(), 0, at as c_int) },
        };
        ok != 0
    }

    /// Move the given pages so they sit, in this order, from `dest` on.
    pub fn move_pages(&self, indices: &[usize], dest: usize) -> bool {
        let v: Vec<c_int> = indices.iter().map(|&i| i as c_int).collect();
        unsafe { lib().FPDF_MovePages(self.h, v.as_ptr(), v.len() as c_ulong, dest as c_int) != 0 }
    }

    pub fn delete_page(&self, index: usize) {
        unsafe { lib().FPDFPage_Delete(self.h, index as c_int) };
    }

    pub fn insert_blank(&self, at: usize, w: f32, h: f32) -> bool {
        let p = unsafe { lib().FPDFPage_New(self.h, at as c_int, w as f64, h as f64) };
        if p.is_null() {
            return false;
        }
        unsafe {
            lib().FPDFPage_GenerateContent(p);
            lib().FPDF_ClosePage(p);
        }
        true
    }

    /// Turn a page by quarter turns (positive is clockwise).
    pub fn rotate_page(&self, index: usize, quarter_turns: i32) {
        if let Some(p) = self.page(index) {
            let r = unsafe { lib().FPDFPage_GetRotation(p.h) };
            unsafe { lib().FPDFPage_SetRotation(p.h, (r + quarter_turns).rem_euclid(4)) };
        }
    }

    /// One of the 14 standard PDF fonts (not embedded, Latin text only).
    pub fn standard_font(&self, name: &str) -> FPDF_FONT {
        let key = format!("std:{name}");
        if let Some(f) = self.fonts.borrow().iter().find(|(k, _, _)| *k == key) {
            return f.1;
        }
        let f = unsafe { lib().FPDFText_LoadStandardFont(self.h, name) };
        if !f.is_null() {
            self.fonts.borrow_mut().push((key, f, Vec::new()));
        }
        f
    }

    /// Width of `text` in `font` at `size` (text space), from the font's glyph widths.
    pub fn font_text_width(font: FPDF_FONT, text: &str, size: f32) -> f32 {
        if font.is_null() {
            return text.chars().count() as f32 * size * 0.5;
        }
        text.chars()
            .map(|c| {
                let mut w = 0.0;
                let ok = unsafe { lib().FPDFFont_GetGlyphWidth(font, c as u32, size, &mut w) };
                if ok != 0 { w } else { size * 0.5 }
            })
            .sum()
    }

    /// A TrueType font file embedded with full Unicode coverage.
    pub fn truetype_font(&self, path: &Path) -> Option<FPDF_FONT> {
        let key = path.to_string_lossy().into_owned();
        if let Some(f) = self.fonts.borrow().iter().find(|(k, _, _)| *k == key) {
            return Some(f.1);
        }
        let data = std::fs::read(path).ok()?;
        let f = unsafe { lib().FPDFText_LoadFont(self.h, data.as_ptr(), data.len() as _, FONT_TRUETYPE, 1) };
        if f.is_null() {
            return None;
        }
        self.fonts.borrow_mut().push((key, f, data));
        Some(f)
    }
}

// ── pages ──

/// A loaded page. Changes to page objects are written to its content stream when it is dropped.
pub struct Page<'d> {
    doc: &'d Doc,
    h: FPDF_PAGE,
    edited: Cell<bool>,
}

impl Drop for Page<'_> {
    fn drop(&mut self) {
        let l = lib();
        unsafe {
            if self.edited.get() {
                l.FPDFPage_GenerateContent(self.h);
            }
            if !self.doc.form.is_null() {
                l.FORM_OnBeforeClosePage(self.h, self.doc.form);
            }
            l.FPDF_ClosePage(self.h);
        }
    }
}

/// Maps user space to view space for one page.
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    /// Displayed width in points.
    pub w: f32,
    m: [f64; 6],
    inv: [f64; 6],
}

impl Geom {
    pub fn to_view(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.m;
        let (x, y) = (x as f64, y as f64);
        ((m[0] * x + m[2] * y + m[4]) as f32, (m[1] * x + m[3] * y + m[5]) as f32)
    }

    pub fn to_user(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.inv;
        let (x, y) = (x as f64, y as f64);
        ((m[0] * x + m[2] * y + m[4]) as f32, (m[1] * x + m[3] * y + m[5]) as f32)
    }

    /// Scale from user units to view units (1 unless the page has a /UserUnit-like transform).
    pub fn unit(&self) -> f32 {
        ((self.m[0] * self.m[3] - self.m[1] * self.m[2]).abs().sqrt()) as f32
    }
}

fn invert(m: [f64; 6]) -> [f64; 6] {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    [a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Path,
    Image,
    Shading,
    Group,
}

#[derive(Clone, Debug)]
pub struct Obj {
    pub index: usize,
    pub kind: Kind,
    /// Corners in user space (rotated objects have a rotated quad).
    pub quad: [(f32, f32); 4],
}

#[derive(Clone, Debug)]
pub struct FontInfo {
    pub name: String,
    pub family: String,
    pub flags: i32,
    pub weight: i32,
    pub embedded: bool,
}

impl FontInfo {
    pub fn fixed(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn serif(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn italic(&self) -> bool {
        self.flags & 64 != 0 || self.name.to_lowercase().contains("italic") || self.name.to_lowercase().contains("oblique")
    }
    pub fn bold(&self) -> bool {
        let n = self.name.to_lowercase();
        self.weight >= 600 || self.flags & (1 << 18) != 0 || n.contains("bold") || n.contains("black") || n.contains("heavy")
    }
    /// The font name without a subset tag ("ABCDEF+Calibri-Bold" → "Calibri-Bold").
    pub fn clean_name(&self) -> &str {
        match self.name.split_once('+') {
            Some((tag, rest)) if tag.len() == 6 && tag.chars().all(|c| c.is_ascii_uppercase()) => rest,
            _ => &self.name,
        }
    }
}

pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Curve(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Clone, Copy)]
pub struct PathStyle {
    pub fill: Option<Rgba>,
    pub stroke: Option<Rgba>,
    pub width: f32,
}

pub enum ImageData<'a> {
    Jpeg(&'a [u8]),
    Rgba { w: u32, h: u32, px: &'a [u8] },
}

#[derive(Clone, Debug, Default)]
pub struct Field {
    pub kind: i32,
    pub name: String,
    pub value: String,
    pub checked: bool,
    pub flags: i32,
    pub options: Vec<(String, bool)>,
    /// Scripts run on typing, for showing, for checking and for working out the value.
    pub keystroke_js: String,
    pub format_js: String,
    pub validate_js: String,
    pub calc_js: String,
}

impl Field {
    pub fn read_only(&self) -> bool {
        self.flags & FIELD_FLAG_READONLY != 0
    }
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            FIELD_PUSHBUTTON => "Button",
            FIELD_CHECKBOX => "Checkbox",
            FIELD_RADIO => "Option button",
            FIELD_COMBO => "Drop-down list",
            FIELD_LIST => "List",
            FIELD_TEXT => "Text field",
            FIELD_SIGNATURE => "Signature field",
            _ => "Field",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Annot {
    pub index: usize,
    pub subtype: i32,
    /// l, b, r, t in user space.
    pub rect: [f32; 4],
    pub contents: String,
    pub color: Option<Rgba>,
    pub field: Option<Field>,
    /// The annotation this one replies to (or records a review state for).
    pub reply_to: Option<usize>,
    /// A review state ("Completed", "None", ...) when this records one rather than a reply.
    pub state: Option<String>,
    pub author: String,
    /// Last change, as a PDF date string ("D:20261021...").
    pub modified: String,
    /// The /IT intent, such as "LineDimension" or "FreeTextCallout".
    pub intent: String,
}

impl Annot {
    pub fn kind_name(&self) -> &'static str {
        annot_name(self.subtype)
    }
}

pub fn annot_name(subtype: i32) -> &'static str {
    match subtype {
        ANNOT_TEXT => "Note",
        ANNOT_LINK => "Link",
        ANNOT_FREETEXT => "Text box",
        ANNOT_LINE => "Line",
        ANNOT_SQUARE => "Rectangle",
        ANNOT_CIRCLE => "Ellipse",
        7 => "Polygon",
        8 => "Polyline",
        ANNOT_HIGHLIGHT => "Highlight",
        ANNOT_UNDERLINE => "Underline",
        ANNOT_SQUIGGLY => "Squiggle",
        ANNOT_STRIKEOUT => "Strikethrough",
        ANNOT_STAMP => "Stamp",
        14 => "Caret",
        ANNOT_INK => "Drawing",
        ANNOT_POPUP => "Pop-up",
        17 => "Attachment",
        ANNOT_WIDGET => "Form field",
        ANNOT_REDACT => "Redaction mark",
        _ => "Annotation",
    }
}

/// A character with the text object it is drawn by.
#[derive(Clone, Copy, Debug)]
pub struct Char {
    pub obj: usize,
    pub ch: char,
    /// Where its baseline starts, in user space.
    pub origin: (f32, f32),
    /// The glyph's own box (empty for a word gap).
    pub rect: [f32; 4],
}

/// One character from the page's text layer.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    /// Loose box (full line height): l, b, r, t in user space.
    pub rect: [f32; 4],
}

impl<'d> Page<'d> {
    fn obj(&self, i: usize) -> FPDF_PAGEOBJECT {
        unsafe { lib().FPDFPage_GetObject(self.h, i as c_int) }
    }

    fn touch(&self) {
        self.edited.set(true);
    }

    pub fn geom(&self) -> Geom {
        let l = lib();
        let (w, h) = unsafe { (l.FPDF_GetPageWidthF(self.h), l.FPDF_GetPageHeightF(self.h)) };
        // Ask PDFium for the device mapping at 100 pixels per point, which keeps integer rounding tiny.
        const K: f64 = 100.0;
        let (sx, sy) = ((w as f64 * K).round() as c_int, (h as f64 * K).round() as c_int);
        let dev = |x: f64, y: f64| {
            let (mut dx, mut dy) = (0, 0);
            unsafe { l.FPDF_PageToDevice(self.h, 0, 0, sx, sy, 0, x, y, &mut dx, &mut dy) };
            (dx as f64 / K, dy as f64 / K)
        };
        let o = dev(0.0, 0.0);
        let ax = dev(100.0, 0.0);
        let ay = dev(0.0, 100.0);
        let m = [(ax.0 - o.0) / 100.0, (ax.1 - o.1) / 100.0, (ay.0 - o.0) / 100.0, (ay.1 - o.1) / 100.0, o.0, o.1];
        Geom { w, m, inv: invert(m) }
    }

    /// Render the pixel region `[x0, y0, x1, y1]` of the page drawn at `scale` pixels per point.
    pub fn render(&self, scale: f32, region: [i32; 4]) -> Option<egui::ColorImage> {
        self.render_with(scale, region, false)
    }

    /// Like `render`; `dark` draws the page light-on-dark for reading at night: text and lines in
    /// light colours, filled areas in the background colour, pictures as they are.
    pub fn render_with(&self, scale: f32, region: [i32; 4], dark: bool) -> Option<egui::ColorImage> {
        let l = lib();
        let (w, h) = unsafe { (l.FPDF_GetPageWidthF(self.h), l.FPDF_GetPageHeightF(self.h)) };
        let (fw, fh) = ((w * scale).round().max(1.0) as c_int, (h * scale).round().max(1.0) as c_int);
        let [x0, y0, x1, y1] = region;
        let (bw, bh) = ((x1 - x0).max(1), (y1 - y0).max(1));
        unsafe {
            let bmp = l.FPDFBitmap_CreateEx(bw, bh, BITMAP_BGRA, null_mut(), 0);
            if bmp.is_null() {
                return None;
            }
            let flags = FLAG_ANNOT | FLAG_REVERSE_BYTE_ORDER;
            if dark {
                // FillRect ignores the byte order flag, so its colour is given with red and blue swapped.
                let [_, r, g, b] = DARK_PAGE.to_be_bytes();
                l.FPDFBitmap_FillRect(bmp, 0, 0, bw, bh, u32::from_be_bytes([255, b, g, r]) as _);
                let scheme = FPDF_COLORSCHEME { path_fill_color: DARK_PAGE as _, path_stroke_color: 0xFFB4B6BC, text_fill_color: 0xFFE2E3E6, text_stroke_color: 0xFFE2E3E6 };
                // A progressive render that never pauses: it runs to the end in one go.
                unsafe extern "C" fn never(_: *mut IFSDK_PAUSE) -> pdfium_render::prelude::FPDF_BOOL {
                    0
                }
                let mut pause = IFSDK_PAUSE { version: 1, NeedToPauseNow: Some(never), user: null_mut() };
                let mut status = l.FPDF_RenderPageBitmapWithColorScheme_Start(bmp, self.h, -x0, -y0, fw, fh, 0, flags, &scheme, &mut pause);
                while status == 1 {
                    status = l.FPDF_RenderPage_Continue(self.h, &mut pause);
                }
                l.FPDF_RenderPage_Close(self.h);
            } else {
                l.FPDFBitmap_FillRect(bmp, 0, 0, bw, bh, 0xFFFF_FFFF);
                l.FPDF_RenderPageBitmap(bmp, self.h, -x0, -y0, fw, fh, 0, flags);
            }
            if !self.doc.form.is_null() {
                l.FPDF_FFLDraw(self.doc.form, bmp, self.h, -x0, -y0, fw, fh, 0, flags);
            }
            let stride = l.FPDFBitmap_GetStride(bmp) as usize;
            let buf = l.FPDFBitmap_GetBuffer(bmp) as *const u8;
            let row = bw as usize * 4;
            let mut px = Vec::with_capacity(row * bh as usize);
            for y in 0..bh as usize {
                px.extend_from_slice(std::slice::from_raw_parts(buf.add(y * stride), row));
            }
            l.FPDFBitmap_Destroy(bmp);
            Some(egui::ColorImage::from_rgba_unmultiplied([bw as usize, bh as usize], &px))
        }
    }

    /// The page drawn for a printer: `w` × `h` pixels, turned a quarter turn clockwise `rotate`
    /// times, as top-down BGRA rows (what a DIB wants).
    pub fn render_print(&self, w: i32, h: i32, rotate: i32) -> Option<Vec<u8>> {
        let l = lib();
        unsafe {
            let bmp = l.FPDFBitmap_CreateEx(w, h, BITMAP_BGRA, null_mut(), 0);
            if bmp.is_null() {
                return None;
            }
            l.FPDFBitmap_FillRect(bmp, 0, 0, w, h, 0xFFFF_FFFF);
            let flags = FLAG_ANNOT | FLAG_PRINTING;
            l.FPDF_RenderPageBitmap(bmp, self.h, 0, 0, w, h, rotate, flags);
            if !self.doc.form.is_null() {
                l.FPDF_FFLDraw(self.doc.form, bmp, self.h, 0, 0, w, h, rotate, flags);
            }
            let stride = l.FPDFBitmap_GetStride(bmp) as usize;
            let buf = l.FPDFBitmap_GetBuffer(bmp) as *const u8;
            let row = w as usize * 4;
            let mut px = Vec::with_capacity(row * h as usize);
            for y in 0..h as usize {
                px.extend_from_slice(std::slice::from_raw_parts(buf.add(y * stride), row));
            }
            l.FPDFBitmap_Destroy(bmp);
            Some(px)
        }
    }

    /// Every place `query` appears on the page, each as the rects (l, b, r, t) it covers.
    pub fn find(&self, query: &str, match_case: bool, whole_word: bool) -> Vec<Vec<[f32; 4]>> {
        let l = lib();
        let mut out = Vec::new();
        if query.is_empty() {
            return out;
        }
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            if tp.is_null() {
                return out;
            }
            let q = wide(query);
            let flags = (if match_case { 1 } else { 0 }) | (if whole_word { 2 } else { 0 });
            let sh = l.FPDFText_FindStart(tp, q.as_ptr(), flags, 0);
            if !sh.is_null() {
                while l.FPDFText_FindNext(sh) != 0 {
                    let (start, count) = (l.FPDFText_GetSchResultIndex(sh), l.FPDFText_GetSchCount(sh));
                    let n = l.FPDFText_CountRects(tp, start, count);
                    let rects = (0..n.max(0))
                        .filter_map(|k| {
                            let (mut a, mut t, mut r, mut b) = (0.0, 0.0, 0.0, 0.0);
                            (l.FPDFText_GetRect(tp, k, &mut a, &mut t, &mut r, &mut b) != 0).then_some([a as f32, b as f32, r as f32, t as f32])
                        })
                        .collect::<Vec<_>>();
                    if !rects.is_empty() {
                        out.push(rects);
                    }
                }
                l.FPDFText_FindClose(sh);
            }
            l.FPDFText_ClosePage(tp);
        }
        out
    }

    /// The text layer as a string, with the character index each byte of it comes from.
    pub fn chars_text(&self) -> (String, Vec<usize>) {
        let l = lib();
        let mut s = String::new();
        let mut at = Vec::new();
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            if tp.is_null() {
                return (s, at);
            }
            for i in 0..l.FPDFText_CountChars(tp).max(0) {
                let c = char::from_u32(l.FPDFText_GetUnicode(tp, i)).unwrap_or(' ');
                let c = if c == '\r' { ' ' } else { c };
                s.push(c);
                at.extend(std::iter::repeat_n(i as usize, c.len_utf8()));
            }
            l.FPDFText_ClosePage(tp);
        }
        (s, at)
    }

    /// The boxes (l, b, r, t) covering `count` characters of the text layer from `start`.
    pub fn text_rects(&self, start: usize, count: usize) -> Vec<[f32; 4]> {
        let l = lib();
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            if tp.is_null() {
                return Vec::new();
            }
            let n = l.FPDFText_CountRects(tp, start as c_int, count as c_int);
            let out = (0..n.max(0))
                .filter_map(|k| {
                    let (mut a, mut t, mut r, mut b) = (0.0, 0.0, 0.0, 0.0);
                    (l.FPDFText_GetRect(tp, k, &mut a, &mut t, &mut r, &mut b) != 0).then_some([a as f32, b as f32, r as f32, t as f32])
                })
                .collect();
            l.FPDFText_ClosePage(tp);
            out
        }
    }

    /// The text of characters `a..=b` of the text layer (as numbered by `glyphs`).
    pub fn text_range(&self, a: usize, b: usize) -> String {
        let l = lib();
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            if tp.is_null() {
                return String::new();
            }
            let count = (b.max(a) - a.min(b) + 1) as c_int;
            let mut buf = vec![0u16; count as usize + 1];
            let got = l.FPDFText_GetText(tp, a.min(b) as c_int, count, buf.as_mut_ptr()).max(1) as usize;
            l.FPDFText_ClosePage(tp);
            // Line breaks come as \r\n.
            String::from_utf16_lossy(&buf[..got - 1]).replace("\r\n", "\n")
        }
    }

    /// Makes PDFium build appearance streams for annotations that lack one (new notes, highlights,
    /// ink), so they are saved with the file and show in other viewers.
    fn refresh_appearances(&self) {
        let l = lib();
        unsafe {
            let bmp = l.FPDFBitmap_CreateEx(1, 1, BITMAP_BGRA, null_mut(), 0);
            if !bmp.is_null() {
                l.FPDF_RenderPageBitmap(bmp, self.h, 0, 0, 1, 1, 0, FLAG_ANNOT);
                l.FPDFBitmap_Destroy(bmp);
            }
        }
    }

    // ── page objects ──

    /// The points of path object `i` in user space: (x, y, kind, closes) where kind is 0 line,
    /// 1 curve point, 2 move.
    pub fn path_points(&self, i: usize) -> Vec<(f32, f32, i32, bool)> {
        let l = lib();
        let o = self.obj(i);
        let m = self.matrix(i);
        unsafe {
            (0..l.FPDFPath_CountSegments(o).max(0))
                .filter_map(|k| {
                    let s = l.FPDFPath_GetPathSegment(o, k);
                    if s.is_null() {
                        return None;
                    }
                    let (mut x, mut y) = (0.0f32, 0.0f32);
                    l.FPDFPathSegment_GetPoint(s, &mut x, &mut y);
                    let (ux, uy) = (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
                    Some((ux, uy, l.FPDFPathSegment_GetType(s), l.FPDFPathSegment_GetClose(s) != 0))
                })
                .collect()
        }
    }

    /// Every character of the text layer that belongs to a text object, in reading order, with
    /// the object it comes from.
    pub fn chars(&self) -> Vec<Char> {
        let l = lib();
        let mut out = Vec::new();
        let n_obj = unsafe { l.FPDFPage_CountObjects(self.h) }.max(0) as usize;
        let index: std::collections::HashMap<usize, usize> = (0..n_obj).map(|i| (self.obj(i) as usize, i)).collect();
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            if tp.is_null() {
                return out;
            }
            let mut last: Option<usize> = None;
            for i in 0..l.FPDFText_CountChars(tp).max(0) {
                let ch = char::from_u32(l.FPDFText_GetUnicode(tp, i)).unwrap_or(char::REPLACEMENT_CHARACTER);
                if l.FPDFText_IsGenerated(tp, i) == 1 {
                    // Word gaps made by spacing rather than a space character still separate words.
                    if let (' ', Some(obj)) = (ch, last) {
                        out.push(Char { obj, ch, origin: (0.0, 0.0), rect: [0.0; 4] });
                    }
                    continue;
                }
                let o = l.FPDFText_GetTextObject(tp, i);
                let Some(&obj) = index.get(&(o as usize)) else { continue };
                last = Some(obj);
                // The glyph's own box, not the line-height one, so neighbours are not caught.
                let (mut x0, mut x1, mut y0, mut y1) = (0.0, 0.0, 0.0, 0.0);
                l.FPDFText_GetCharBox(tp, i, &mut x0, &mut x1, &mut y0, &mut y1);
                let (mut x, mut y) = (0.0, 0.0);
                l.FPDFText_GetCharOrigin(tp, i, &mut x, &mut y);
                out.push(Char { obj, ch, origin: (x as f32, y as f32), rect: [x0 as f32, y0 as f32, x1 as f32, y1 as f32] });
            }
            l.FPDFText_ClosePage(tp);
        }
        out
    }

    /// An image object's pixels as RGBA, decoded but before any mask.
    pub fn image_rgba(&self, i: usize) -> Option<(u32, u32, Vec<u8>)> {
        let l = lib();
        unsafe {
            let bmp = l.FPDFImageObj_GetBitmap(self.obj(i));
            if bmp.is_null() {
                return None;
            }
            let (w, h, stride) = (l.FPDFBitmap_GetWidth(bmp), l.FPDFBitmap_GetHeight(bmp), l.FPDFBitmap_GetStride(bmp));
            let format = l.FPDFBitmap_GetFormat(bmp);
            let buf = l.FPDFBitmap_GetBuffer(bmp) as *const u8;
            let mut out = Vec::new();
            if !buf.is_null() && w > 0 && h > 0 {
                let data = std::slice::from_raw_parts(buf, (stride * h) as usize);
                out.reserve((w * h * 4) as usize);
                for y in 0..h as usize {
                    let row = &data[y * stride as usize..];
                    for x in 0..w as usize {
                        let px = match format {
                            1 => [row[x], row[x], row[x], 255],
                            2 => [row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255],
                            3 => [row[x * 4 + 2], row[x * 4 + 1], row[x * 4], 255],
                            4 => [row[x * 4 + 2], row[x * 4 + 1], row[x * 4], row[x * 4 + 3]],
                            _ => [0, 0, 0, 255],
                        };
                        out.extend_from_slice(&px);
                    }
                }
            }
            l.FPDFBitmap_Destroy(bmp);
            (!out.is_empty()).then_some((w as u32, h as u32, out))
        }
    }

    pub fn objects(&self) -> Vec<Obj> {
        let l = lib();
        let n = unsafe { l.FPDFPage_CountObjects(self.h) }.max(0) as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let o = self.obj(i);
            let kind = match unsafe { l.FPDFPageObj_GetType(o) } {
                1 => Kind::Text,
                2 => Kind::Path,
                3 => Kind::Image,
                4 => Kind::Shading,
                5 => Kind::Group,
                _ => continue,
            };
            let mut q = FS_QUADPOINTSF { x1: 0.0, y1: 0.0, x2: 0.0, y2: 0.0, x3: 0.0, y3: 0.0, x4: 0.0, y4: 0.0 };
            let quad = if matches!(kind, Kind::Text | Kind::Image) && unsafe { l.FPDFPageObj_GetRotatedBounds(o, &mut q) } != 0 {
                [(q.x1, q.y1), (q.x2, q.y2), (q.x3, q.y3), (q.x4, q.y4)]
            } else {
                let (mut a, mut b, mut c, mut d) = (0.0, 0.0, 0.0, 0.0);
                if unsafe { l.FPDFPageObj_GetBounds(o, &mut a, &mut b, &mut c, &mut d) } == 0 {
                    continue;
                }
                [(a, b), (c, b), (c, d), (a, d)]
            };
            out.push(Obj { index: i, kind, quad });
        }
        out
    }

    pub fn object_count(&self) -> usize {
        unsafe { lib().FPDFPage_CountObjects(self.h).max(0) as usize }
    }

    /// PDFium's type number for every object (1 text, 2 path, 3 image, 4 shading, 5 form).
    pub fn kinds(&self) -> Vec<i32> {
        (0..self.object_count()).map(|i| unsafe { lib().FPDFPageObj_GetType(self.obj(i)) }).collect()
    }

    pub fn matrix(&self, i: usize) -> [f32; 6] {
        let mut m = FS_MATRIX { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };
        unsafe { lib().FPDFPageObj_GetMatrix(self.obj(i), &mut m) };
        [m.a, m.b, m.c, m.d, m.e, m.f]
    }

    fn color(&self, i: usize, stroke: bool) -> Option<Rgba> {
        let (mut r, mut g, mut b, mut a) = (0, 0, 0, 0);
        let ok = unsafe {
            if stroke {
                lib().FPDFPageObj_GetStrokeColor(self.obj(i), &mut r, &mut g, &mut b, &mut a)
            } else {
                lib().FPDFPageObj_GetFillColor(self.obj(i), &mut r, &mut g, &mut b, &mut a)
            }
        };
        (ok != 0).then_some([r as u8, g as u8, b as u8, a as u8])
    }

    pub fn fill_color(&self, i: usize) -> Option<Rgba> {
        self.color(i, false)
    }

    pub fn stroke_color(&self, i: usize) -> Option<Rgba> {
        self.color(i, true)
    }

    pub fn stroke_width(&self, i: usize) -> f32 {
        let mut w = 0.0;
        unsafe { lib().FPDFPageObj_GetStrokeWidth(self.obj(i), &mut w) };
        w
    }

    /// (filled, stroked) for a path object.
    pub fn path_mode(&self, i: usize) -> (bool, bool) {
        let (mut fill, mut stroke) = (0, 0);
        unsafe { lib().FPDFPath_GetDrawMode(self.obj(i), &mut fill, &mut stroke) };
        (fill != 0, stroke != 0)
    }

    // ── text objects ──

    pub fn text(&self, i: usize) -> String {
        let l = lib();
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            if tp.is_null() {
                return String::new();
            }
            let o = self.obj(i);
            let s = read_wide(|buf, len| l.FPDFTextObj_GetText(o, tp, buf, len));
            l.FPDFText_ClosePage(tp);
            s
        }
    }

    /// The page's text in reading order, as PDFium extracts it.
    pub fn page_text(&self) -> String {
        let l = lib();
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            if tp.is_null() {
                return String::new();
            }
            let n = l.FPDFText_CountChars(tp).max(0);
            let mut buf = vec![0u16; n as usize + 1];
            let got = l.FPDFText_GetText(tp, 0, n, buf.as_mut_ptr()).max(1) as usize;
            l.FPDFText_ClosePage(tp);
            String::from_utf16_lossy(&buf[..got - 1])
        }
    }

    /// The text of every object (None for non-text objects), read with one text page.
    pub fn texts(&self) -> Vec<Option<String>> {
        let l = lib();
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            let out = (0..self.object_count())
                .map(|i| {
                    let o = self.obj(i);
                    (!tp.is_null() && l.FPDFPageObj_GetType(o) == 1).then(|| read_wide(|buf, len| l.FPDFTextObj_GetText(o, tp, buf, len)))
                })
                .collect();
            if !tp.is_null() {
                l.FPDFText_ClosePage(tp);
            }
            out
        }
    }

    pub fn font_size(&self, i: usize) -> f32 {
        let mut s = 0.0;
        unsafe { lib().FPDFTextObj_GetFontSize(self.obj(i), &mut s) };
        s
    }

    fn font_handle(&self, i: usize) -> FPDF_FONT {
        unsafe { lib().FPDFTextObj_GetFont(self.obj(i)) }
    }

    pub fn font(&self, i: usize) -> Option<FontInfo> {
        let l = lib();
        let f = self.font_handle(i);
        if f.is_null() {
            return None;
        }
        unsafe {
            Some(FontInfo {
                name: read_narrow(|b, n| l.FPDFFont_GetBaseFontName(f, b, n)),
                family: read_narrow(|b, n| l.FPDFFont_GetFamilyName(f, b, n)),
                flags: l.FPDFFont_GetFlags(f),
                weight: l.FPDFFont_GetWeight(f),
                embedded: l.FPDFFont_GetIsEmbedded(f) == 1,
            })
        }
    }

    /// Add a line of text placed by the text matrix `m` (its baseline origin is (m[4], m[5])).
    /// Returns the new object's index.
    pub fn add_text(&self, font: FPDF_FONT, size: f32, text: &str, m: [f32; 6], color: Rgba) -> Option<usize> {
        let l = lib();
        unsafe {
            let o = l.FPDFPageObj_CreateTextObj(self.doc.h, font, size);
            if o.is_null() {
                return None;
            }
            let w = wide(if text.is_empty() { " " } else { text });
            l.FPDFText_SetText(o, w.as_ptr());
            let fm = FS_MATRIX { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] };
            l.FPDFPageObj_SetMatrix(o, &fm);
            l.FPDFPageObj_SetFillColor(o, color[0] as _, color[1] as _, color[2] as _, color[3] as _);
            l.FPDFPage_InsertObject(self.h, o);
        }
        self.touch();
        Some(self.object_count() - 1)
    }

    /// Invisible text (render mode 3), as a recognized-text layer over a scanned page: it can be
    /// searched, selected and copied, but does not show.
    pub fn add_hidden_text(&self, font: FPDF_FONT, size: f32, text: &str, m: [f32; 6]) -> bool {
        let l = lib();
        unsafe {
            let o = l.FPDFPageObj_CreateTextObj(self.doc.h, font, size);
            if o.is_null() {
                return false;
            }
            let w = wide(text);
            l.FPDFText_SetText(o, w.as_ptr());
            let fm = FS_MATRIX { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] };
            l.FPDFPageObj_SetMatrix(o, &fm);
            l.FPDFTextObj_SetTextRenderMode(o, 3);
            l.FPDFPage_InsertObject(self.h, o);
        }
        self.touch();
        true
    }

    /// The crop box (l, b, r, t), or the media box when there is none.
    pub fn crop_box(&self) -> [f32; 4] {
        let (mut l, mut b, mut r, mut t) = (0.0, 0.0, 0.0, 0.0);
        let l_ = lib();
        unsafe {
            if l_.FPDFPage_GetCropBox(self.h, &mut l, &mut b, &mut r, &mut t) == 0 {
                l_.FPDFPage_GetMediaBox(self.h, &mut l, &mut b, &mut r, &mut t);
            }
        }
        [l.min(r), b.min(t), l.max(r), b.max(t)]
    }

    pub fn media_box(&self) -> [f32; 4] {
        let (mut l, mut b, mut r, mut t) = (0.0, 0.0, 0.0, 0.0);
        unsafe { lib().FPDFPage_GetMediaBox(self.h, &mut l, &mut b, &mut r, &mut t) };
        [l.min(r), b.min(t), l.max(r), b.max(t)]
    }

    /// Set the visible area. The page's content is untouched.
    pub fn set_crop_box(&self, r: [f32; 4]) {
        unsafe { lib().FPDFPage_SetCropBox(self.h, r[0], r[1], r[2], r[3]) };
    }

    pub fn set_media_box(&self, r: [f32; 4]) {
        unsafe { lib().FPDFPage_SetMediaBox(self.h, r[0], r[1], r[2], r[3]) };
    }

    /// Transform everything drawn on the page by `m` (PDFium wraps the existing content in a
    /// matrix rather than rewriting it).
    pub fn transform_content(&self, m: [f32; 6]) -> bool {
        let fm = FS_MATRIX { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] };
        unsafe { lib().FPDFPage_TransFormWithClip(self.h, &fm, null()) != 0 }
    }

    /// Draw page `src_page` of `src` on this page as one group (a form XObject), placed by `m`.
    /// PDFium copies the fonts and images it needs. Returns the new object's index.
    pub fn insert_page_as_group(&self, src: &Doc, src_page: usize, m: [f32; 6]) -> Option<usize> {
        let l = lib();
        unsafe {
            let x = l.FPDF_NewXObjectFromPage(self.doc.h, src.h, src_page as c_int);
            if x.is_null() {
                return None;
            }
            let o = l.FPDF_NewFormObjectFromXObject(x);
            l.FPDF_CloseXObject(x);
            if o.is_null() {
                return None;
            }
            let fm = FS_MATRIX { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] };
            l.FPDFPageObj_SetMatrix(o, &fm);
            l.FPDFPage_InsertObject(self.h, o);
        }
        self.touch();
        Some(self.object_count() - 1)
    }

    /// Width of `text` set in object `i`'s font at `size`, in text space (no spacing or scaling).
    pub fn text_width(&self, i: usize, text: &str, size: f32) -> f32 {
        Doc::font_text_width(self.font_handle(i), text, size)
    }

    // ── paths ──

    fn build_path(segs: &[Seg], style: PathStyle) -> FPDF_PAGEOBJECT {
        let l = lib();
        let (x0, y0) = match segs.first() {
            Some(Seg::Move(x, y)) => (*x, *y),
            _ => (0.0, 0.0),
        };
        unsafe {
            let o = l.FPDFPageObj_CreateNewPath(x0, y0);
            if o.is_null() {
                return o;
            }
            for s in segs.iter().skip(1) {
                match *s {
                    Seg::Move(x, y) => l.FPDFPath_MoveTo(o, x, y),
                    Seg::Line(x, y) => l.FPDFPath_LineTo(o, x, y),
                    Seg::Curve(a, b, c, d, e, f) => l.FPDFPath_BezierTo(o, a, b, c, d, e, f),
                    Seg::Close => l.FPDFPath_Close(o),
                };
            }
            if let Some(c) = style.fill {
                l.FPDFPageObj_SetFillColor(o, c[0] as _, c[1] as _, c[2] as _, c[3] as _);
            }
            if let Some(c) = style.stroke {
                l.FPDFPageObj_SetStrokeColor(o, c[0] as _, c[1] as _, c[2] as _, c[3] as _);
                l.FPDFPageObj_SetStrokeWidth(o, style.width);
            }
            // Round caps and joins suit freehand strokes and look right on shapes too.
            l.FPDFPageObj_SetLineCap(o, 1);
            l.FPDFPageObj_SetLineJoin(o, 1);
            l.FPDFPath_SetDrawMode(o, if style.fill.is_some() { 2 } else { 0 }, style.stroke.is_some() as c_int);
            o
        }
    }

    pub fn add_path(&self, segs: &[Seg], style: PathStyle) -> Option<usize> {
        let o = Self::build_path(segs, style);
        if o.is_null() {
            return None;
        }
        unsafe { lib().FPDFPage_InsertObject(self.h, o) };
        self.touch();
        Some(self.object_count() - 1)
    }

    // ── images ──

    fn load_image(&self, o: FPDF_PAGEOBJECT, data: &ImageData) -> bool {
        let l = lib();
        unsafe {
            match data {
                ImageData::Jpeg(bytes) => {
                    let src = Source { ptr: bytes.as_ptr(), len: bytes.len() };
                    let mut access =
                        FPDF_FILEACCESS { m_FileLen: bytes.len() as c_ulong, m_GetBlock: Some(get_block), m_Param: &src as *const Source as *mut c_void };
                    l.FPDFImageObj_LoadJpegFileInline(null_mut(), 0, o, &mut access) != 0
                }
                ImageData::Rgba { w, h, px } => {
                    let opaque = px.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
                    let bmp = l.FPDFBitmap_CreateEx(*w as c_int, *h as c_int, if opaque { BITMAP_BGRX } else { BITMAP_BGRA }, null_mut(), 0);
                    if bmp.is_null() {
                        return false;
                    }
                    let stride = l.FPDFBitmap_GetStride(bmp) as usize;
                    let buf = l.FPDFBitmap_GetBuffer(bmp) as *mut u8;
                    for y in 0..*h as usize {
                        let row = std::slice::from_raw_parts_mut(buf.add(y * stride), *w as usize * 4);
                        for (d, s) in row.as_chunks_mut::<4>().0.iter_mut().zip(px[y * *w as usize * 4..].chunks_exact(4)) {
                            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                        }
                    }
                    let ok = l.FPDFImageObj_SetBitmap(null_mut(), 0, o, bmp) != 0;
                    l.FPDFBitmap_Destroy(bmp);
                    ok
                }
            }
        }
    }

    /// Place an image in the user-space rectangle (x, y, w, h).
    pub fn add_image(&self, data: &ImageData, rect: [f32; 4]) -> Option<usize> {
        let l = lib();
        let o = unsafe { l.FPDFPageObj_NewImageObj(self.doc.h) };
        if o.is_null() {
            return None;
        }
        if !self.load_image(o, data) {
            unsafe { l.FPDFPageObj_Destroy(o) };
            return None;
        }
        let [x, y, w, h] = rect;
        unsafe {
            let m = FS_MATRIX { a: w, b: 0.0, c: 0.0, d: h, e: x, f: y };
            l.FPDFPageObj_SetMatrix(o, &m);
            l.FPDFPage_InsertObject(self.h, o);
        }
        self.touch();
        Some(self.object_count() - 1)
    }

    // ── text layer ──

    pub fn glyphs(&self) -> Vec<Glyph> {
        let l = lib();
        let mut out = Vec::new();
        unsafe {
            let tp = l.FPDFText_LoadPage(self.h);
            if tp.is_null() {
                return out;
            }
            let n = l.FPDFText_CountChars(tp).max(0);
            out.reserve(n as usize);
            for i in 0..n {
                let mut r = FS_RECTF { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
                if l.FPDFText_IsGenerated(tp, i) == 1 || l.FPDFText_GetLooseCharBox(tp, i, &mut r) == 0 {
                    r = FS_RECTF { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
                }
                out.push(Glyph { rect: [r.left, r.bottom, r.right, r.top] });
            }
            l.FPDFText_ClosePage(tp);
        }
        out
    }

    // ── annotations ──

    fn with_annot<R>(&self, i: usize, f: impl FnOnce(pdfium_render::prelude::FPDF_ANNOTATION) -> R) -> Option<R> {
        let l = lib();
        let a = unsafe { l.FPDFPage_GetAnnot(self.h, i as c_int) };
        if a.is_null() {
            return None;
        }
        let r = f(a);
        unsafe { l.FPDFPage_CloseAnnot(a) };
        Some(r)
    }

    pub fn annots(&self) -> Vec<Annot> {
        let l = lib();
        let n = unsafe { l.FPDFPage_GetAnnotCount(self.h) }.max(0) as usize;
        let form = self.doc.form;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let a = self.with_annot(i, |a| unsafe {
                let subtype = l.FPDFAnnot_GetSubtype(a);
                let mut r = FS_RECTF { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
                l.FPDFAnnot_GetRect(a, &mut r);
                let contents = read_wide(|b, n| l.FPDFAnnot_GetStringValue(a, "Contents", b, n));
                let (mut cr, mut cg, mut cb, mut ca) = (0, 0, 0, 0);
                let color = (l.FPDFAnnot_GetColor(a, COLORTYPE_COLOR, &mut cr, &mut cg, &mut cb, &mut ca) != 0)
                    .then_some([cr as u8, cg as u8, cb as u8, ca as u8])
                    .or_else(|| appearance_color(a));
                let field = (subtype == ANNOT_WIDGET && !form.is_null()).then(|| {
                    let kind = l.FPDFAnnot_GetFormFieldType(form, a);
                    let options = if matches!(kind, FIELD_COMBO | FIELD_LIST) {
                        (0..l.FPDFAnnot_GetOptionCount(form, a).max(0))
                            .map(|k| (read_wide(|b, n| l.FPDFAnnot_GetOptionLabel(form, a, k, b, n)), l.FPDFAnnot_IsOptionSelected(form, a, k) != 0))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let mut value = read_wide(|b, n| l.FPDFAnnot_GetFormFieldValue(form, a, b, n));
                    if kind == FIELD_SIGNATURE && value.is_empty() {
                        // PDFium gives no value for signatures: a signed field has a /V (here or on its parent).
                        let parent = l.FPDFAnnot_GetLinkedAnnot(a, "Parent");
                        let signed = l.FPDFAnnot_HasKey(a, "V") != 0 || (!parent.is_null() && l.FPDFAnnot_HasKey(parent, "V") != 0);
                        if !parent.is_null() {
                            l.FPDFPage_CloseAnnot(parent);
                        }
                        if signed {
                            value = "Signed".into();
                        }
                    }
                    Field {
                        kind,
                        name: read_wide(|b, n| l.FPDFAnnot_GetFormFieldName(form, a, b, n)),
                        value,
                        checked: matches!(kind, FIELD_CHECKBOX | FIELD_RADIO) && l.FPDFAnnot_IsChecked(form, a) != 0,
                        flags: l.FPDFAnnot_GetFormFieldFlags(form, a),
                        options,
                        keystroke_js: read_wide(|b, n| l.FPDFAnnot_GetFormAdditionalActionJavaScript(form, a, 12, b, n)),
                        format_js: read_wide(|b, n| l.FPDFAnnot_GetFormAdditionalActionJavaScript(form, a, 13, b, n)),
                        validate_js: read_wide(|b, n| l.FPDFAnnot_GetFormAdditionalActionJavaScript(form, a, 14, b, n)),
                        calc_js: read_wide(|b, n| l.FPDFAnnot_GetFormAdditionalActionJavaScript(form, a, 15, b, n)),
                    }
                });
                let (lo, hi) = (r.bottom.min(r.top), r.bottom.max(r.top));
                let irt = l.FPDFAnnot_GetLinkedAnnot(a, "IRT");
                let reply_to = (!irt.is_null()).then(|| {
                    let k = l.FPDFPage_GetAnnotIndex(self.h, irt);
                    l.FPDFPage_CloseAnnot(irt);
                    k
                });
                let reply_to = reply_to.filter(|&k| k >= 0).map(|k| k as usize);
                let get = |key: &str| read_wide(|b, n| l.FPDFAnnot_GetStringValue(a, key, b, n));
                let state = (l.FPDFAnnot_HasKey(a, "State") != 0).then(|| get("State"));
                Annot {
                    index: i,
                    subtype,
                    rect: [r.left.min(r.right), lo, r.left.max(r.right), hi],
                    contents,
                    color,
                    field,
                    reply_to,
                    state,
                    author: get("T"),
                    modified: get("M"),
                    intent: get("IT"),
                }
            });
            if let Some(a) = a {
                out.push(a);
            }
        }
        out
    }

    fn new_annot(&self, subtype: i32, color: Rgba, rect: [f32; 4], f: impl FnOnce(pdfium_render::prelude::FPDF_ANNOTATION)) -> Option<usize> {
        let l = lib();
        unsafe {
            let a = l.FPDFPage_CreateAnnot(self.h, subtype);
            if a.is_null() {
                return None;
            }
            let r = FS_RECTF { left: rect[0], bottom: rect[1], right: rect[2], top: rect[3] };
            l.FPDFAnnot_SetRect(a, &r);
            l.FPDFAnnot_SetColor(a, COLORTYPE_COLOR, color[0] as _, color[1] as _, color[2] as _, color[3] as _);
            l.FPDFAnnot_SetFlags(a, ANNOT_FLAG_PRINT);
            f(a);
            l.FPDFPage_CloseAnnot(a);
            self.refresh_appearances();
            Some((l.FPDFPage_GetAnnotCount(self.h) - 1).max(0) as usize)
        }
    }

    /// Highlight, underline, squiggle or strike through the given line boxes (l, b, r, t).
    pub fn add_markup(&self, subtype: i32, boxes: &[[f32; 4]], color: Rgba) -> Option<usize> {
        if boxes.is_empty() {
            return None;
        }
        let bounds = boxes.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |a, r| [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[2]), a[3].max(r[3])]);
        self.new_annot(subtype, color, bounds, |a| unsafe {
            for r in boxes {
                // Upper left, upper right, lower left, lower right, as the PDF specification orders them.
                let q = FS_QUADPOINTSF { x1: r[0], y1: r[3], x2: r[2], y2: r[3], x3: r[0], y3: r[1], x4: r[2], y4: r[1] };
                lib().FPDFAnnot_AppendAttachmentPoints(a, &q);
            }
        })
    }

    /// A sticky note whose icon's top left corner is at (x, y).
    pub fn add_note(&self, x: f32, y: f32, text: &str, color: Rgba) -> Option<usize> {
        self.new_annot(ANNOT_TEXT, color, [x, y - 20.0, x + 20.0, y], |a| unsafe {
            lib().FPDFAnnot_SetStringValue_str(a, "Contents", text);
            lib().FPDFAnnot_SetStringValue_str(a, "Name", "Comment");
        })
    }

    pub fn add_ink(&self, strokes: &[Vec<(f32, f32)>], color: Rgba, width: f32) -> Option<usize> {
        let pts = strokes.iter().flatten();
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for &(x, y) in pts {
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
        if b[0] > b[2] {
            return None;
        }
        let pad = width;
        self.new_annot(ANNOT_INK, color, [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad], |a| unsafe {
            lib().FPDFAnnot_SetBorder(a, 0.0, 0.0, width);
            for s in strokes {
                let p: Vec<FS_POINTF> = s.iter().map(|&(x, y)| FS_POINTF { x, y }).collect();
                lib().FPDFAnnot_AddInkStroke(a, p.as_ptr(), p.len());
            }
        })
    }

    /// A rubber-stamp style label in the rectangle (l, b, r, t).
    pub fn add_stamp(&self, rect: [f32; 4], label: &str, color: Rgba) -> Option<usize> {
        let font = self.doc.standard_font("Helvetica-Bold");
        let [x0, y0, x1, y1] = rect;
        let h = y1 - y0;
        let size = h * 0.48;
        // Width of the label in the font, to centre it.
        let text_w = estimate_width(label, size);
        self.new_annot(ANNOT_STAMP, color, rect, |a| unsafe {
            let l = lib();
            let border = (h * 0.07).max(1.5);
            let r = h * 0.18;
            let i = border / 2.0;
            let segs = rounded_rect(x0 + i, y0 + i, x1 - i, y1 - i, r);
            let frame = Self::build_path(&segs, PathStyle { fill: Some([color[0], color[1], color[2], 22]), stroke: Some(color), width: border });
            if !frame.is_null() {
                l.FPDFAnnot_AppendObject(a, frame);
            }
            let t = l.FPDFPageObj_CreateTextObj(self.doc.h, font, size);
            if !t.is_null() {
                let w = wide(label);
                l.FPDFText_SetText(t, w.as_ptr());
                let tx = (x0 + x1) / 2.0 - text_w / 2.0;
                let ty = y0 + (h - size * 0.72) / 2.0;
                l.FPDFPageObj_Transform(t, 1.0, 0.0, 0.0, 1.0, tx as f64, ty as f64);
                l.FPDFPageObj_SetFillColor(t, color[0] as _, color[1] as _, color[2] as _, 255);
                l.FPDFAnnot_AppendObject(a, t);
            }
        })
    }

    pub fn remove_annot(&self, i: usize) {
        unsafe { lib().FPDFPage_RemoveAnnot(self.h, i as c_int) };
    }

    pub fn set_annot_contents(&self, i: usize, s: &str) {
        self.with_annot(i, |a| unsafe { lib().FPDFAnnot_SetStringValue_str(a, "Contents", s) });
    }

    pub fn set_annot_color(&self, i: usize, c: Rgba) {
        self.with_annot(i, |a| unsafe {
            let l = lib();
            l.FPDFAnnot_SetColor(a, COLORTYPE_COLOR, c[0] as _, c[1] as _, c[2] as _, c[3] as _);
            // Drop the old appearance so PDFium draws a new one in the new colour.
            if l.FPDFAnnot_GetSubtype(a) != ANNOT_STAMP {
                l.FPDFAnnot_SetAP(a, 0, null());
            }
        });
        self.refresh_appearances();
    }

    pub fn move_annot(&self, i: usize, dx: f32, dy: f32) {
        self.with_annot(i, |a| unsafe {
            let l = lib();
            let mut r = FS_RECTF { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
            if l.FPDFAnnot_GetRect(a, &mut r) != 0 {
                let r = FS_RECTF { left: r.left + dx, right: r.right + dx, top: r.top + dy, bottom: r.bottom + dy };
                l.FPDFAnnot_SetRect(a, &r);
            }
        });
    }

    // ── form fields ──

    pub fn set_field_text(&self, i: usize, text: &str) -> bool {
        let form = self.doc.form;
        if form.is_null() {
            return false;
        }
        self.with_annot(i, |a| unsafe {
            let l = lib();
            if l.FORM_SetFocusedAnnot(form, a) == 0 {
                return false;
            }
            l.FORM_SelectAllText(form, self.h);
            let w = wide(text);
            l.FORM_ReplaceSelection(form, self.h, w.as_ptr());
            l.FORM_ForceToKillFocus(form);
            true
        })
        .unwrap_or(false)
    }

    /// Click the middle of a field, which toggles checkboxes and option buttons.
    pub fn click_field(&self, i: usize) {
        let form = self.doc.form;
        if form.is_null() {
            return;
        }
        let Some(r) = self.with_annot(i, |a| unsafe {
            let mut r = FS_RECTF { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
            lib().FPDFAnnot_GetRect(a, &mut r);
            r
        }) else {
            return;
        };
        let (x, y) = (((r.left + r.right) / 2.0) as f64, ((r.top + r.bottom) / 2.0) as f64);
        unsafe {
            let l = lib();
            l.FORM_OnMouseMove(form, self.h, 0, x, y);
            l.FORM_OnLButtonDown(form, self.h, 0, x, y);
            l.FORM_OnLButtonUp(form, self.h, 0, x, y);
            l.FORM_ForceToKillFocus(form);
        }
    }

    pub fn select_option(&self, i: usize, option: usize) -> bool {
        let form = self.doc.form;
        if form.is_null() {
            return false;
        }
        self.with_annot(i, |a| unsafe {
            let l = lib();
            if l.FORM_SetFocusedAnnot(form, a) == 0 {
                return false;
            }
            let ok = l.FORM_SetIndexSelected(form, self.h, option as c_int, 1) != 0;
            l.FORM_ForceToKillFocus(form);
            ok
        })
        .unwrap_or(false)
    }
}

/// PDFium will not report the colour of an annotation that has its own appearance, so take it
/// from the first path drawn in that appearance (its outline, or its fill if it has none).
unsafe fn appearance_color(a: pdfium_render::prelude::FPDF_ANNOTATION) -> Option<Rgba> {
    let l = lib();
    unsafe {
        for k in 0..l.FPDFAnnot_GetObjectCount(a) {
            let o = l.FPDFAnnot_GetObject(a, k);
            if o.is_null() || l.FPDFPageObj_GetType(o) != 2 {
                continue;
            }
            let (mut fill, mut stroke) = (0, 0);
            l.FPDFPath_GetDrawMode(o, &mut fill, &mut stroke);
            let (mut r, mut g, mut b, mut al) = (0, 0, 0, 0);
            let ok = if stroke != 0 {
                l.FPDFPageObj_GetStrokeColor(o, &mut r, &mut g, &mut b, &mut al)
            } else {
                l.FPDFPageObj_GetFillColor(o, &mut r, &mut g, &mut b, &mut al)
            };
            if ok != 0 {
                return Some([r as u8, g as u8, b as u8, 255]);
            }
        }
    }
    None
}

/// Approximate width of Helvetica Bold text (average advance), enough to centre a stamp label.
pub fn estimate_width(s: &str, size: f32) -> f32 {
    s.chars()
        .map(|c| match c {
            'i' | 'l' | 'j' | '.' | ',' | '\'' | '!' | '|' => 0.28,
            ' ' => 0.28,
            'I' => 0.28,
            'm' | 'w' => 0.89,
            'M' | 'W' => 0.89,
            c if c.is_uppercase() => 0.72,
            _ => 0.61,
        })
        .sum::<f32>()
        * size
}

/// A rounded rectangle as path segments (corner curves use the usual 0.5523 Bézier constant).
pub fn rounded_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Vec<Seg> {
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    let k = r * 0.552_284_8;
    vec![
        Seg::Move(x0 + r, y0),
        Seg::Line(x1 - r, y0),
        Seg::Curve(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r),
        Seg::Line(x1, y1 - r),
        Seg::Curve(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1),
        Seg::Line(x0 + r, y1),
        Seg::Curve(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r),
        Seg::Line(x0, y0 + r),
        Seg::Curve(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0),
        Seg::Close,
    ]
}

/// An ellipse inside (x0, y0)–(x1, y1) as four Bézier curves.
pub fn ellipse(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Seg> {
    let (cx, cy, rx, ry) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0).abs() / 2.0, (y1 - y0).abs() / 2.0);
    let (kx, ky) = (rx * 0.552_284_8, ry * 0.552_284_8);
    vec![
        Seg::Move(cx + rx, cy),
        Seg::Curve(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry),
        Seg::Curve(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy),
        Seg::Curve(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry),
        Seg::Curve(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy),
        Seg::Close,
    ]
}

/// The folder the executable runs from (where pdfium.dll lives).
pub fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
}
