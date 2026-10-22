//! Bates numbering: a running number with a prefix and suffix on every page, as legal teams mark
//! documents. It can continue through more files, which are saved as numbered copies beside the
//! originals (the originals are not changed).

use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::{Editor, plural};
use crate::pageops::MM;
use crate::theme::{self, UiExt, pal};
use egui::RichText;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

pub struct BatesDialog {
    prefix: String,
    start: u64,
    digits: usize,
    suffix: String,
    corner: Corner,
    size: f32,
    margin_mm: f32,
    color: crate::pdf::Rgba,
    /// More files to number after this document, in order.
    files: Vec<PathBuf>,
    error: Option<String>,
}

impl BatesDialog {
    #[cfg(test)]
    pub fn set_for_test(&mut self, prefix: &str, start: u64, digits: usize, files: Vec<PathBuf>) {
        (self.prefix, self.start, self.digits, self.files) = (prefix.into(), start, digits, files);
    }

    pub fn new() -> Self {
        BatesDialog {
            prefix: String::new(),
            start: 1,
            digits: 6,
            suffix: String::new(),
            corner: Corner::BottomRight,
            size: 9.0,
            margin_mm: 8.0,
            color: [20, 22, 28, 255],
            files: Vec::new(),
            error: None,
        }
    }

    fn label(&self, n: u64) -> String {
        format!("{}{:0width$}{}", self.prefix, n, self.suffix, width = self.digits)
    }
}

impl Editor {
    /// Number every page from `first`. Returns the next number.
    fn bates_pages(&mut self, d: &BatesDialog, first: u64) -> Result<u64, String> {
        let n = self.file.as_ref().map_or(0, |f| f.page_count());
        let margin = d.margin_mm * MM;
        for i in 0..n {
            let text = d.label(first + i as u64);
            let (w, h) = self.file.as_ref().map(|f| f.sizes[i]).unwrap_or((612.0, 792.0));
            let tw = self.line_width(&text, d.size);
            let x = match d.corner {
                Corner::TopLeft | Corner::BottomLeft => margin,
                Corner::TopCenter | Corner::BottomCenter => (w - tw) / 2.0,
                Corner::TopRight | Corner::BottomRight => w - margin - tw,
            };
            let y = match d.corner {
                Corner::TopLeft | Corner::TopCenter | Corner::TopRight => margin + d.size * 0.8,
                _ => h - margin,
            };
            if !self.put_text(i, (x, y), 0.0, &text, d.size, d.color) {
                return Err("No font was found for the numbers.".into());
            }
        }
        Ok(first + n as u64)
    }
}

/// Returns false when it closes.
pub fn bates_window(ctx: &egui::Context, ed: &mut Editor, d: &mut BatesDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut run = false;
    let (_, close) = modal(ctx, "bates", 460.0, |ui| {
        title(ui, "Bates numbering");
        ui.label(theme::secondary(ui, "A running number on every page. It can carry on through more files, which are saved as numbered copies."));
        theme::heading(ui, "NUMBER");
        ui.horizontal(|ui| {
            ui.label("Prefix");
            ui.add(egui::TextEdit::singleline(&mut d.prefix).desired_width(90.0).hint_text("ABC"));
            ui.label("Suffix");
            ui.add(egui::TextEdit::singleline(&mut d.suffix).desired_width(70.0));
        });
        ui.horizontal(|ui| {
            ui.label("Start at");
            ui.add(egui::DragValue::new(&mut d.start).range(0..=999_999_999u64).speed(1.0));
            ui.label("Digits");
            ui.add(egui::DragValue::new(&mut d.digits).range(1..=12).speed(0.1));
        });
        ui.label(theme::secondary(ui, format!("For example {}", d.label(d.start))));
        theme::heading(ui, "PLACE");
        ui.segmented(&mut d.corner, &[(Corner::TopLeft, "Top left"), (Corner::TopCenter, "Top"), (Corner::TopRight, "Top right")]);
        ui.segmented(&mut d.corner, &[(Corner::BottomLeft, "Bottom left"), (Corner::BottomCenter, "Bottom"), (Corner::BottomRight, "Bottom right")]);
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add(egui::DragValue::new(&mut d.size).range(5.0..=30.0).speed(0.2).suffix(" pt"));
            ui.label("Margin");
            ui.add(egui::DragValue::new(&mut d.margin_mm).range(2.0..=40.0).speed(0.2).suffix(" mm"));
        });
        crate::canvas::color_row(ui, &mut d.color);
        theme::heading(ui, "MORE FILES");
        if d.files.is_empty() {
            ui.label(theme::secondary(ui, "Only this document."));
        }
        let mut gone = None;
        for (k, f) in d.files.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(f.file_name().unwrap_or_default().to_string_lossy());
                if ui.small_button("×").on_hover_text("Leave out").clicked() {
                    gone = Some(k);
                }
            });
        }
        if let Some(k) = gone {
            d.files.remove(k);
        }
        if ui.button("Add files…").clicked()
            && let Some(more) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_files()
        {
            d.files.extend(more);
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Number pages", true) {
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
        match number_all(ed, d) {
            Ok(msg) => {
                ed.say(msg);
                open = false;
            }
            Err(e) => d.error = Some(e),
        }
    }
    open
}

pub(crate) fn number_all(ed: &mut Editor, d: &BatesDialog) -> Result<String, String> {
    let first = d.start;
    let mut next = ed.bates_pages(d, first)?;
    let mut copies = 0;
    for path in &d.files {
        let mut other = Editor::open_headless(path).map_err(|e| format!("{} was not numbered: {e}.", path.display()))?;
        next = other.bates_pages(d, next)?;
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let out = path.with_file_name(format!("{stem} (Bates).pdf"));
        other.write_to(&out)?;
        copies += 1;
    }
    let range = format!("{} to {}", d.label(first), d.label(next.saturating_sub(1)));
    Ok(if copies == 0 { format!("Numbered {range}") } else { format!("Numbered {range}; {} saved beside the originals", plural(copies, "numbered copy", "numbered copies")) })
}
