//! Dockit: a PDF editor. Edit text, images and shapes; combine, split and reorder pages; annotate,
//! fill in forms and sign. PDFium (Chrome's PDF engine) reads, draws and writes the files.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod annots;
mod app;
mod arrange;
mod canvas;
mod compare;
mod content;
mod dialogs;
mod editor;
mod fonts;
mod incremental;
mod mark;
mod ocr;
mod pageops;
mod panels;
mod pdf;
mod recovery;
mod redact;
mod security;
mod setup;
mod sign;
mod theme;

/// Every tool, in the order the Tools panel lists them (used for keyboard shortcuts).
pub fn panels_tools() -> [editor::Tool; 17] {
    use editor::Tool::*;
    [Select, Text, Image, Rect, Ellipse, Line, Highlight, Note, Ink, Stamp, Arrow, TextBox, Callout, Measure, Field, Redact, Sign]
}

fn main() -> eframe::Result<()> {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--install") => {
            if let Err(e) = setup::install(false) {
                eprintln!("{e}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some("--uninstall") => {
            let yes = rfd::MessageDialog::new()
                .set_title("Uninstall Dockit")
                .set_description("Remove Dockit from this computer? Your settings and documents are kept.")
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                == rfd::MessageDialogResult::Yes;
            if yes {
                setup::uninstall();
            }
            return Ok(());
        }
        // A file opened from Explorer goes to the Dockit already running, as a new tab.
        Some(path) if std::path::Path::new(path).exists() && setup::forward(std::path::Path::new(path)) => return Ok(()),
        _ => {}
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
