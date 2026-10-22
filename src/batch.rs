//! Batch processing: the same steps on many files. Each file is opened out of sight, worked on
//! and written to the chosen folder (the originals are never changed); one file per frame, so the
//! window shows progress and stays responsive.

use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::{Editor, plural};
use crate::pageops::Quality;
use crate::theme::{self, UiExt, pal};
use egui::RichText;
use std::path::PathBuf;

pub struct BatchDialog {
    files: Vec<PathBuf>,
    ocr: bool,
    watermark: bool,
    watermark_text: String,
    flatten: bool,
    sanitize: bool,
    compress: bool,
    quality: Quality,
    protect: bool,
    password: String,
    out_dir: Option<PathBuf>,
    /// Where the run is: the next file, and what happened to each so far.
    next: Option<usize>,
    log: Vec<(String, Result<String, String>)>,
    error: Option<String>,
}

impl BatchDialog {
    pub fn new() -> Self {
        BatchDialog {
            files: Vec::new(),
            ocr: false,
            watermark: false,
            watermark_text: "CONFIDENTIAL".into(),
            flatten: false,
            sanitize: true,
            compress: true,
            quality: Quality::Medium,
            protect: false,
            password: String::new(),
            out_dir: None,
            next: None,
            log: Vec::new(),
            error: None,
        }
    }

    fn any_step(&self) -> bool {
        self.ocr || (self.watermark && !self.watermark_text.trim().is_empty()) || self.flatten || self.sanitize || self.compress || self.protect
    }
}

/// Run the chosen steps on one file and write the result. Returns a line about what was done.
fn process(d: &BatchDialog, path: &PathBuf, out_dir: &PathBuf) -> Result<String, String> {
    let mut ed = Editor::open_headless(path)?;
    let mut done = Vec::new();
    if d.ocr {
        let words = crate::ocr::ocr_document(&mut ed, None)?;
        done.push(format!("{} recognized", plural(words, "word", "words")));
    }
    if d.watermark && !d.watermark_text.trim().is_empty() {
        let n = ed.file.as_ref().map_or(0, |f| f.page_count());
        let spec = crate::pageops::StampDialog::watermark(d.watermark_text.trim());
        ed.stamp_pages(&spec, &(0..n).collect::<Vec<_>>());
        done.push("watermarked".into());
    }
    if d.flatten {
        ed.flatten();
        done.push("flattened".into());
    }
    if d.sanitize {
        ed.sanitize(crate::docinfo::Sanitize::default())?;
        done.push("cleaned".into());
    }
    if d.compress {
        ed.compress(d.quality)?;
        done.push("compressed".into());
    }
    if d.protect
        && let Some(f) = &mut ed.file
    {
        f.security = crate::security::Security::Protect(crate::security::Protection { open: d.password.clone(), owner: String::new(), allow: crate::security::Allow::ALL });
        done.push("password protected".into());
    }
    if let Some(e) = ed.error.take() {
        return Err(e);
    }
    let name = path.file_name().ok_or("The file has no name.")?;
    let out = out_dir.join(name);
    if out == *path {
        return Err("The output folder is the folder the file is in; choose another so the original is kept.".into());
    }
    ed.write_to(&out)?;
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    Ok(format!("{} ({} KB)", done.join(", "), size / 1024))
}

/// Returns false when it closes.
pub fn batch_window(ctx: &egui::Context, d: &mut BatchDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut start = false;
    let running = d.next.is_some();
    let (_, close) = modal(ctx, "batch", 480.0, |ui| {
        title(ui, "Batch process");
        ui.label(theme::secondary(ui, "The same steps on many files. The results go to a folder you choose; the originals are not changed."));
        ui.add_enabled_ui(!running, |ui| {
            theme::heading(ui, "FILES");
            ui.horizontal(|ui| {
                ui.label(if d.files.is_empty() { "None yet".to_string() } else { plural(d.files.len(), "file", "files") });
                if ui.button("Add files…").clicked()
                    && let Some(more) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_files()
                {
                    d.files.extend(more);
                    d.files.dedup();
                }
                if ui.button("Add a folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    && let Ok(entries) = std::fs::read_dir(dir)
                {
                    let mut more: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf"))).collect();
                    more.sort();
                    d.files.extend(more);
                }
                if !d.files.is_empty() && ui.button("Clear").clicked() {
                    d.files.clear();
                    d.log.clear();
                }
            });
            theme::heading(ui, "STEPS, IN THIS ORDER");
            ui.checkbox(&mut d.ocr, "Recognize text on scanned pages");
            ui.horizontal(|ui| {
                ui.checkbox(&mut d.watermark, "Watermark");
                ui.add_enabled(d.watermark, egui::TextEdit::singleline(&mut d.watermark_text).desired_width(200.0));
            });
            ui.checkbox(&mut d.flatten, "Flatten comments and form fields");
            ui.checkbox(&mut d.sanitize, "Remove hidden information");
            ui.horizontal(|ui| {
                ui.checkbox(&mut d.compress, "Compress");
                ui.add_enabled_ui(d.compress, |ui| {
                    ui.segmented(&mut d.quality, &[(Quality::High, "High quality"), (Quality::Medium, "Balanced"), (Quality::Small, "Smallest")]);
                });
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut d.protect, "Password to open");
                ui.add_enabled(d.protect, egui::TextEdit::singleline(&mut d.password).password(true).desired_width(160.0));
            });
            theme::heading(ui, "SAVE TO");
            ui.horizontal(|ui| {
                ui.label(d.out_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "No folder chosen".into()));
                if ui.button("Choose folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    d.out_dir = Some(dir);
                }
            });
        });
        if !d.log.is_empty() || running {
            ui.add_space(6.0);
            if let Some(k) = d.next {
                ui.add(egui::ProgressBar::new(k as f32 / d.files.len().max(1) as f32).text(format!("File {} of {}", (k + 1).min(d.files.len()), d.files.len())));
            }
            egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                for (name, r) in &d.log {
                    match r {
                        Ok(m) => ui.label(format!("{name}: {m}")),
                        Err(e) => ui.label(RichText::new(format!("{name}: {e}")).color(p.danger)),
                    };
                }
            });
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if running {
                if secondary_button(ui, "Stop") {
                    d.next = None;
                }
            } else {
                if primary(ui, "Start", !d.files.is_empty() && d.any_step() && d.out_dir.is_some()) {
                    start = true;
                }
                if secondary_button(ui, if d.log.is_empty() { "Cancel" } else { "Close" }) {
                    open = false;
                }
            }
        });
    });
    if close && !running {
        open = false;
    }
    if start {
        if d.protect && d.password.is_empty() {
            d.error = Some("Type the password to open the files with.".into());
        } else {
            d.error = None;
            d.log.clear();
            d.next = Some(0);
        }
    }
    // One file per frame.
    if let (Some(k), Some(out)) = (d.next, d.out_dir.clone()) {
        if let Some(path) = d.files.get(k).cloned() {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let r = process(d, &path, &out);
            d.log.push((name, r));
            d.next = Some(k + 1);
            ctx.request_repaint();
        } else {
            d.next = None;
        }
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::Doc;
    use std::path::Path;

    fn sample() -> PathBuf {
        crate::pdf::init(Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug").as_path()).unwrap();
        let dir = std::env::temp_dir().join("dockit-phase-c");
        let _ = std::fs::create_dir_all(dir.join("out"));
        let p = dir.join("sample.pdf");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("target/devtools/out/p1.pdf"), &p).unwrap();
        p
    }

    /// The new tools on a real document (run with --ignored; needs pdfium.dll in target/debug).
    #[test]
    #[ignore]
    fn tools_on_a_real_document() {
        let path = sample();
        let mut ed = Editor::open_headless(&path).unwrap();

        // Find and redact: words and dates.
        let words = ed.find_for_redaction(crate::redact::Pattern::Words, "OpenStudio", false).unwrap();
        assert!(words.len() > 10, "{}", words.len());
        let dates = ed.find_for_redaction(crate::redact::Pattern::Dates, "", false).unwrap();
        assert!(!dates.is_empty());
        let areas: Vec<(usize, [f32; 4])> = dates.iter().map(|(p, r)| (*p, r[0])).collect();
        assert_eq!(ed.add_redactions(&areas), areas.len());
        assert_eq!(ed.redaction_marks().len(), areas.len());

        // Properties.
        let fields = vec![("Title", "Release notes".to_string()), ("Author", "Tester".to_string())];
        assert!(ed.file_update(&[], |u| u.set_properties(&fields, None)).is_some());
        assert_eq!(ed.file.as_ref().unwrap().doc.meta("Title"), "Release notes");

        // Removing hidden information takes the properties away again.
        ed.sanitize(crate::docinfo::Sanitize::default()).unwrap();
        assert_eq!(ed.file.as_ref().unwrap().doc.meta("Author"), "");

        // Attachments.
        assert!(ed.apply_doc(|f| f.doc.add_attachment("note.txt", b"hello")).unwrap());
        assert_eq!(ed.file.as_ref().unwrap().doc.attachments(), vec![("note.txt".to_string(), 5)]);
        assert_eq!(ed.file.as_ref().unwrap().doc.attachment_data(0).unwrap(), b"hello");

        // Batch: watermark, clean, compress and protect into another folder.
        let mut d = BatchDialog::new();
        d.watermark = true;
        d.protect = true;
        d.password = "pw".into();
        let out_dir = path.parent().unwrap().join("out");
        let line = process(&d, &path, &out_dir).unwrap();
        println!("batch: {line}");
        let out = std::fs::read(out_dir.join("sample.pdf")).unwrap();
        assert!(Doc::open(out.clone(), None).is_err(), "needs its password");
        let doc = Doc::open(out, Some("pw")).unwrap();
        assert!(doc.page(0).unwrap().page_text().contains("CONFIDENTIAL"));
    }

    /// Bates numbers through two files (run with --ignored).
    #[test]
    #[ignore]
    fn bates_numbers_carry_on() {
        let path = sample();
        let mut ed = Editor::open_headless(&path).unwrap();
        let mut d = crate::bates::BatesDialog::new();
        d.set_for_test("ABC", 7, 4, vec![path.clone()]);
        let msg = crate::bates::number_all(&mut ed, &d).unwrap();
        println!("{msg}");
        let f = ed.file.as_ref().unwrap();
        assert!(f.doc.page(0).unwrap().page_text().contains("ABC0007"));
        let copy = path.with_file_name("sample (Bates).pdf");
        let doc = Doc::open(std::fs::read(copy).unwrap(), None).unwrap();
        assert!(doc.page(0).unwrap().page_text().contains("ABC0011"), "numbering carries on after 4 pages");
    }
}

#[cfg(test)]
mod big {
    use crate::pdf::Doc;
    use std::path::Path;

    /// Makes target/devtools/out/big.pdf: the 45-page sample twenty times over (run with --ignored).
    #[test]
    #[ignore]
    fn make_big_document() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        crate::pdf::init(base.join("target/debug").as_path()).unwrap();
        let src = Doc::open(std::fs::read(base.join("target/devtools/out/lua2.pdf")).unwrap(), None).unwrap();
        let doc = Doc::new_empty();
        for _ in 0..20 {
            assert!(doc.import(&src, None, doc.page_count()));
        }
        std::fs::write(base.join("target/devtools/out/big.pdf"), doc.save().unwrap()).unwrap();
        println!("{} pages", doc.page_count());
    }
}
