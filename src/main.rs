//! Dockit: a PDF editor. Edit text, images and shapes; combine, split and reorder pages; annotate,
//! fill in forms and sign. PDFium (Chrome's PDF engine) reads, draws and writes the files.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod arrange;
mod canvas;
mod content;
mod dialogs;
mod editor;
mod fonts;
mod incremental;
mod mark;
mod panels;
mod pdf;
mod theme;

/// Every tool, in the order the Tools panel lists them (used for keyboard shortcuts).
pub fn panels_tools() -> [editor::Tool; 11] {
    use editor::Tool::*;
    [Select, Text, Image, Rect, Ellipse, Line, Highlight, Note, Ink, Stamp, Sign]
}

fn main() -> eframe::Result<()> {
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
