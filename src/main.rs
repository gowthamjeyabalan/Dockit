//! Dockit: a PDF editor. Edit text, images and shapes; combine, split and reorder pages; annotate,
//! fill in forms and sign. PDFium (Chrome's PDF engine) reads, draws and writes the files.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod accessibility;
mod annots;
mod app;
mod batch;
mod bates;
mod arrange;
mod canvas;
mod commands;
mod compare;
mod content;
mod dialogs;
mod docinfo;
mod editor;
mod find;
mod fonts;
mod forms;
mod incremental;
mod instance;
mod mark;
mod ocr;
mod pageops;
mod palette;
mod panels;
mod pdf;
mod print;
mod recovery;
mod redact;
mod security;
mod settings;
mod sign;
mod speech;
mod standards;
mod theme;

/// Every tool, in the order the Tools panel lists them (used for keyboard shortcuts).
pub fn panels_tools() -> [editor::Tool; 17] {
    use editor::Tool::*;
    [Select, Text, Image, Rect, Ellipse, Line, Highlight, Note, Ink, Stamp, Arrow, TextBox, Callout, Measure, Field, Redact, Sign]
}

fn main() -> eframe::Result<()> {
    // A file opened from Explorer goes to the Dockit already running, as a new tab.
    if let Some(path) = std::env::args().nth(1).map(std::path::PathBuf::from)
        && path.exists()
        && instance::forward(&path)
    {
        return Ok(());
    }
    let broken = pdf::init(&pdf::exe_dir()).err();
    let icon = egui::IconData { rgba: mark::icon_rgba(256), width: 256, height: 256 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dockit")
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([760.0, 500.0])
            .with_drag_and_drop(true)
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native("Dockit", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, broken)))))
}
