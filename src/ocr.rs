//! Text recognition (OCR) for scanned pages, with Windows' built-in engine (Windows.Media.Ocr).
//!
//! Pages are rendered here, one at a time, and recognized on a worker thread so the window stays
//! responsive. Each recognized word is added to its page as invisible text sized to cover the
//! word in the picture, so the page can be searched, selected and copied while looking the same.

use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::{Editor, plural};
use crate::pageops::{Scope, scope_pages, scope_ui};
use crate::theme::{self, pal};
use egui::RichText;
use std::sync::mpsc::{Receiver, Sender, channel};

/// A recognized word: its text and its box in the picture (x, y, width, height; y down, pixels).
#[derive(Clone, Debug)]
pub struct Word {
    pub text: String,
    pub rect: [f32; 4],
}

/// Languages the engine can read on this computer: (tag, name).
pub fn languages() -> Vec<(String, String)> {
    use windows::Media::Ocr::OcrEngine;
    let Ok(list) = OcrEngine::AvailableRecognizerLanguages() else { return Vec::new() };
    list.into_iter().filter_map(|l| Some((l.LanguageTag().ok()?.to_string_lossy(), l.DisplayName().ok()?.to_string_lossy()))).collect()
}

/// Recognize the words in a BGRA picture. `lang` is a language tag, or None for the user's languages.
pub fn recognize(bgra: &[u8], w: u32, h: u32, lang: Option<&str>) -> Result<Vec<Word>, String> {
    use windows::Globalization::Language;
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;
    let err = |e: windows::core::Error| format!("Text recognition failed ({}).", e.message());
    let engine = match lang {
        Some(tag) => OcrEngine::TryCreateFromLanguage(&Language::CreateLanguage(&windows::core::HSTRING::from(tag)).map_err(err)?),
        None => OcrEngine::TryCreateFromUserProfileLanguages(),
    }
    .map_err(|_| "No text recognition language is installed. Add one in Windows Settings > Time & language > Language & region (with \"Optical character recognition\").".to_string())?;
    let buffer = CryptographicBuffer::CreateFromByteArray(bgra).map_err(err)?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32, BitmapAlphaMode::Premultiplied).map_err(err)?;
    let result = engine.RecognizeAsync(&bitmap).map_err(err)?.join().map_err(err)?;
    let mut words = Vec::new();
    for line in result.Lines().map_err(err)? {
        for word in line.Words().map_err(err)? {
            let r = word.BoundingRect().map_err(err)?;
            words.push(Word { text: word.Text().map_err(err)?.to_string_lossy(), rect: [r.X, r.Y, r.Width, r.Height] });
        }
    }
    Ok(words)
}

/// The largest picture side the engine accepts.
fn max_side() -> u32 {
    windows::Media::Ocr::OcrEngine::MaxImageDimension().unwrap_or(4000).min(6000)
}

struct Job {
    page: usize,
    scale: f32,
    w: u32,
    h: u32,
    bgra: Vec<u8>,
    lang: Option<String>,
}

type Done = (usize, f32, Result<Vec<Word>, String>);

/// A thread that recognizes one picture at a time.
struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
}

impl Worker {
    fn start(ctx: egui::Context) -> Worker {
        let (tx, jobs) = channel::<Job>();
        let (done, rx) = channel::<Done>();
        std::thread::spawn(move || {
            unsafe {
                let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
            }
            for j in jobs {
                let r = recognize(&j.bgra, j.w, j.h, j.lang.as_deref());
                if done.send((j.page, j.scale, r)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Worker { tx, rx }
    }
}

impl Editor {
    /// Add recognized words to `page` as invisible text. `scale` is pixels per point of the picture.
    /// Returns how many words were added.
    pub fn add_ocr_words(&mut self, page: usize, scale: f32, words: &[Word]) -> usize {
        let Some(f) = &self.file else { return 0 };
        let Some(g) = f.geom(page) else { return 0 };
        let latin = f.doc.standard_font("Helvetica");
        // Words outside Latin-1 need a font that has them; it is loaded once and only if needed.
        let needs_other = words.iter().any(|w| !crate::annots::latin(&w.text));
        let other = if needs_other { crate::fonts::generic("sans", false, false).and_then(|p| f.doc.truetype_font(p)) } else { None };
        let mut n = 0;
        self.apply(page, |p| {
            for w in words {
                let text = w.text.trim();
                if text.is_empty() {
                    continue;
                }
                let font = if crate::annots::latin(text) { latin } else if let Some(o) = other { o } else { continue };
                let [x, y, bw, bh] = w.rect.map(|v| v / scale);
                // The box's bottom-left and its two edges, in the page's own space (any rotation).
                let p0 = g.to_user(x, y + bh);
                let p1 = g.to_user(x + bw, y + bh);
                let p2 = g.to_user(x, y);
                let (ux, uy) = (p1.0 - p0.0, p1.1 - p0.1);
                let (vx, vy) = (p2.0 - p0.0, p2.1 - p0.1);
                let height = vx.hypot(vy).max(0.1);
                let size = height * 0.85;
                let natural = crate::pdf::Doc::font_text_width(font, text, size).max(0.1);
                // Stretch the text along the word's width; the baseline sits a little above the box bottom.
                let (hx, hy) = (vx / height, vy / height);
                let m = [ux / natural, uy / natural, hx, hy, p0.0 + hx * height * 0.18, p0.1 + hy * height * 0.18];
                if p.add_hidden_text(font, size, text, m) {
                    n += 1;
                }
            }
        });
        n
    }
}

// ── the dialog ──

pub struct OcrDialog {
    scope: Scope,
    range: String,
    skip_text: bool,
    langs: Vec<(String, String)>,
    lang: Option<String>,
    run: Option<Run>,
    error: Option<String>,
}

struct Run {
    worker: Worker,
    pages: Vec<usize>,
    /// Next page to render and send.
    next: usize,
    busy: bool,
    finished: usize,
    words: usize,
    skipped: usize,
}

impl OcrDialog {
    pub fn new() -> Self {
        let langs = languages();
        OcrDialog { scope: Scope::All, range: String::new(), skip_text: true, lang: langs.first().map(|l| l.0.clone()), langs, run: None, error: None }
    }
}

/// Returns false when the dialog closes.
pub fn ocr_window(ctx: &egui::Context, ed: &mut Editor, d: &mut OcrDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut start = false;
    let mut stop = false;
    let running = d.run.is_some();
    let (_, close) = modal(ctx, "ocr", 420.0, |ui| {
        title(ui, "Recognize text");
        ui.label(theme::secondary(ui, "Finds the words in scanned pages and adds them as invisible text, so the pages can be searched and copied. The pages look the same."));
        ui.add_enabled_ui(!running, |ui| {
            theme::heading(ui, "LANGUAGE");
            if d.langs.is_empty() {
                ui.label(RichText::new("No text recognition language is installed. Add one in Windows Settings > Time & language > Language & region.").color(p.danger));
            } else {
                let current = d.langs.iter().find(|l| Some(&l.0) == d.lang.as_ref()).map(|l| l.1.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("ocr-lang").selected_text(current).show_ui(ui, |ui| {
                    for (tag, name) in &d.langs {
                        ui.selectable_value(&mut d.lang, Some(tag.clone()), name);
                    }
                });
            }
            theme::heading(ui, "PAGES");
            scope_ui(ui, &mut d.scope, &mut d.range, ed);
            ui.checkbox(&mut d.skip_text, "Skip pages that already have text");
        });
        if let Some(r) = &d.run {
            ui.add_space(6.0);
            let frac = r.finished as f32 / r.pages.len().max(1) as f32;
            ui.add(egui::ProgressBar::new(frac).text(format!("Page {} of {}", (r.finished + 1).min(r.pages.len()), r.pages.len())));
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if running {
                if secondary_button(ui, "Stop") {
                    stop = true;
                }
            } else {
                if primary(ui, "Recognize", !d.langs.is_empty()) {
                    start = true;
                }
                if secondary_button(ui, "Cancel") {
                    open = false;
                }
            }
        });
    });
    if close && !running {
        open = false;
    }
    if start {
        match scope_pages(ed, d.scope, &d.range) {
            Ok(pages) if !pages.is_empty() => {
                d.error = None;
                d.run = Some(Run { worker: Worker::start(ctx.clone()), pages, next: 0, busy: false, finished: 0, words: 0, skipped: 0 });
            }
            Ok(_) => d.error = Some("No pages chosen.".into()),
            Err(e) => d.error = Some(e),
        }
    }
    if stop && let Some(r) = d.run.take() {
        ed.say(format!("Stopped. Recognized {} on {}.", plural(r.words, "word", "words"), plural(r.finished - r.skipped, "page", "pages")));
        return false;
    }
    let Some(r) = &mut d.run else { return open };

    // Collect a finished page.
    if let Ok((page, scale, result)) = r.worker.rx.try_recv() {
        r.busy = false;
        r.finished += 1;
        match result {
            Ok(words) => r.words += ed.add_ocr_words(page, scale, &words),
            Err(e) => {
                d.error = Some(e);
                d.run = None;
                return true;
            }
        }
    }
    // Send the next page.
    while !r.busy && r.next < r.pages.len() {
        let page = r.pages[r.next];
        r.next += 1;
        let Some(f) = &ed.file else { break };
        if d.skip_text && f.glyphs(page).len() > 20 {
            r.skipped += 1;
            r.finished += 1;
            continue;
        }
        let (w, h) = f.sizes[page];
        let scale = (300.0 / 72.0f32).min(max_side() as f32 / w.max(h).max(1.0));
        let Some(img) = f.doc.page(page).and_then(|p| p.render(scale, [0, 0, (w * scale).round() as i32, (h * scale).round() as i32])) else {
            r.finished += 1;
            continue;
        };
        let bgra: Vec<u8> = img.pixels.iter().flat_map(|c| [c.b(), c.g(), c.r(), c.a()]).collect();
        let job = Job { page, scale, w: img.size[0] as u32, h: img.size[1] as u32, bgra, lang: d.lang.clone() };
        if r.worker.tx.send(job).is_ok() {
            r.busy = true;
        }
    }
    if r.finished >= r.pages.len() && !r.busy {
        let msg = if r.skipped == r.pages.len() {
            "Every page already has text; nothing was recognized.".to_string()
        } else {
            let mut m = format!("Recognized {} on {}", plural(r.words, "word", "words"), plural(r.pages.len() - r.skipped, "page", "pages"));
            if r.skipped > 0 {
                m += &format!(" ({} already had text)", plural(r.skipped, "page", "pages"));
            }
            m
        };
        ed.say(msg);
        return false;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page that is only a picture of text becomes searchable, and looks the same (run with
    /// --ignored; needs pdfium.dll in target/debug and an OCR language).
    #[test]
    #[ignore]
    fn scanned_page_becomes_searchable() {
        use crate::pdf::{Doc, ImageData};
        crate::pdf::init(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug").as_path()).unwrap();
        let src = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/devtools/out/p1.pdf")).unwrap();
        let src = Doc::open(src, None).unwrap();
        let (w, h) = src.page_size(0);
        let shot = src.page(0).unwrap().render(2.0, [0, 0, (w * 2.0) as i32, (h * 2.0) as i32]).unwrap();
        let px: Vec<u8> = shot.pixels.iter().flat_map(|c| c.to_array()).collect();
        let doc = Doc::new_empty();
        assert!(doc.insert_blank(0, w, h));
        doc.page(0).unwrap().add_image(&ImageData::Rgba { w: shot.size[0] as u32, h: shot.size[1] as u32, px: &px }, [0.0, 0.0, w, h]).unwrap();
        let before = doc.page(0).unwrap().render(1.0, [0, 0, w as i32, h as i32]).unwrap();
        let mut ed = Editor::default();
        ed.open_untitled(doc);
        assert!(ed.file.as_ref().unwrap().glyphs(0).is_empty(), "no text layer yet");
        let bgra: Vec<u8> = shot.pixels.iter().flat_map(|c| [c.b(), c.g(), c.r(), c.a()]).collect();
        let words = recognize(&bgra, shot.size[0] as u32, shot.size[1] as u32, None).unwrap();
        let n = ed.add_ocr_words(0, 2.0, &words);
        assert!(n > 50, "added {n} words");
        let f = ed.file.as_ref().unwrap();
        let text: String = f.doc.page(0).unwrap().texts().into_iter().flatten().collect::<Vec<_>>().join(" ");
        assert!(text.contains("OpenStudio") && text.contains("Laboratory"), "{text}");
        let after = f.doc.page(0).unwrap().render(1.0, [0, 0, w as i32, h as i32]).unwrap();
        assert!(before.pixels == after.pixels, "the page looks the same");
        // The words sit where they are in the picture: the first character's box is close to the
        // original page's first character.
        let g = f.glyphs(0);
        let o = src.page(0).unwrap().glyphs();
        let (a, b) = (g.iter().find(|x| x.rect[2] > x.rect[0]).unwrap().rect, o.iter().find(|x| x.rect[2] > x.rect[0]).unwrap().rect);
        println!("ocr {a:?} original {b:?}");
        assert!((a[0] - b[0]).abs() < 6.0 && (a[1] - b[1]).abs() < 6.0);
    }

    /// Recognizes text in a screenshot of a page (run with --ignored; needs an OCR language).
    #[test]
    #[ignore]
    fn reads_a_screenshot() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/devtools/out/t1.png");
        let img = image::open(&path).unwrap().into_rgba8();
        let (w, h) = img.dimensions();
        let bgra: Vec<u8> = img.pixels().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
        let words = recognize(&bgra, w, h, None).unwrap();
        let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        println!("{} words: {:?}", words.len(), &text[..text.len().min(30)]);
        assert!(text.iter().any(|t| t.contains("OpenStudio")));
    }
}
