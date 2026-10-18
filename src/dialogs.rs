//! Windows that sit over the app: save changes, password, messages, combine, split and signature.
//! All are drawn in the app's own style rather than as system dialogs (file pickers excepted).

use crate::editor::{Editor, Signature, page_ranges, parse_ranges, plural};
use crate::pdf::Doc;
use crate::theme::{self, UiExt, pal};
use egui::{Color32, Key, Margin, Modifiers, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use std::path::PathBuf;

fn frame(ctx: &egui::Context) -> egui::Frame {
    let p = pal(ctx);
    egui::Frame::new()
        .fill(p.window_fill)
        .stroke(Stroke::new(1.0, p.window_stroke))
        .corner_radius(12)
        .inner_margin(Margin::same(18))
        .shadow(ctx.global_style().visuals.window_shadow)
}

fn backdrop(ctx: &egui::Context) -> Color32 {
    Color32::from_black_alpha(if ctx.theme() == egui::Theme::Dark { 120 } else { 60 })
}

/// A modal window. Returns the closure's result and whether it should close (Escape or click outside).
fn modal<R>(ctx: &egui::Context, id: &str, width: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let resp = egui::Modal::new(egui::Id::new(id)).frame(frame(ctx)).backdrop_color(backdrop(ctx)).show(ctx, |ui| {
        ui.set_width(width);
        add(ui)
    });
    let close = resp.should_close();
    (resp.inner, close)
}

fn title(ui: &mut Ui, text: impl Into<String>) {
    ui.label(theme::semibold(text).size(15.0));
    ui.add_space(2.0);
}

fn primary(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    let p = pal(ui.ctx());
    let b = egui::Button::new(theme::semibold(text).color(Color32::WHITE)).fill(p.accent).min_size(vec2(84.0, 0.0));
    ui.add_enabled(enabled, b).clicked()
}

fn secondary_button(ui: &mut Ui, text: &str) -> bool {
    ui.add(egui::Button::new(text).min_size(vec2(76.0, 0.0))).clicked()
}

// ── save changes ──

pub enum SaveChoice {
    Save,
    Discard,
    Cancel,
}

pub fn save_changes(ctx: &egui::Context, name: &str, closing_app: bool) -> Option<SaveChoice> {
    let p = pal(ctx);
    let mut choice = None;
    let (_, close) = modal(ctx, "save-changes", 340.0, |ui| {
        title(ui, format!("Save changes to {name}?"));
        let why = if closing_app {
            "If you don't save, changes since the last save are lost when Dockit closes."
        } else {
            "If you don't save, changes since the last save are lost."
        };
        ui.label(theme::secondary(ui, why));
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Don't save").color(p.danger)).clicked() {
                choice = Some(SaveChoice::Discard);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, "Save", true) {
                    choice = Some(SaveChoice::Save);
                }
                if secondary_button(ui, "Cancel") {
                    choice = Some(SaveChoice::Cancel);
                }
            });
        });
    });
    if choice.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        choice = Some(SaveChoice::Save);
    }
    if choice.is_none() && close {
        choice = Some(SaveChoice::Cancel);
    }
    choice
}

// ── password and messages ──

pub fn password(ctx: &egui::Context, ed: &mut Editor) {
    let Some(ask) = &mut ed.password else { return };
    let p = pal(ctx);
    let name = ask.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut submit = false;
    let mut cancel = false;
    let (_, close) = modal(ctx, "password", 340.0, |ui| {
        title(ui, format!("{name} is protected"));
        ui.label(theme::secondary(ui, "Enter the password to open it."));
        ui.add_space(8.0);
        let resp = ui.add(egui::TextEdit::singleline(&mut ask.text).password(true).desired_width(f32::INFINITY).hint_text("Password"));
        if !resp.has_focus() && ask.text.is_empty() {
            resp.request_focus();
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            submit = true;
        }
        if ask.wrong {
            ui.label(RichText::new("That password is not right.").color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Open", !ask.text.is_empty()) {
                submit = true;
            }
            if secondary_button(ui, "Cancel") {
                cancel = true;
            }
        });
    });
    if submit && !ask.text.is_empty() {
        let ask = ed.password.take().unwrap();
        ed.open_bytes(&ask.path, ask.bytes, Some(ask.text));
    } else if cancel || close {
        ed.password = None;
    }
}

pub fn error(ctx: &egui::Context, ed: &mut Editor) {
    let Some(msg) = ed.error.clone() else { return };
    let mut ok = false;
    let (_, close) = modal(ctx, "message", 360.0, |ui| {
        title(ui, "Something went wrong");
        ui.label(msg);
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "OK", true) {
                ok = true;
            }
        });
    });
    if ok || close || ctx.input(|i| i.key_pressed(Key::Enter)) {
        ed.error = None;
    }
}

// ── combine ──

pub struct Combine {
    files: Vec<PathBuf>,
    with_current: bool,
    open_after: bool,
}

impl Combine {
    pub fn new(ed: &Editor) -> Combine {
        Combine { files: Vec::new(), with_current: ed.file.is_some(), open_after: true }
    }

    pub fn with_files(ed: &Editor, files: Vec<PathBuf>) -> Combine {
        Combine { files, with_current: false, open_after: ed.file.is_none() }
    }
}

/// Returns false once the window is closed.
pub fn combine(ctx: &egui::Context, ed: &mut Editor, c: &mut Combine) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut run = false;
    let current_name = ed.file.as_ref().map(|f| f.name());
    let (_, close) = modal(ctx, "combine", 420.0, |ui| {
        title(ui, "Combine files");
        ui.label(theme::secondary(ui, "Pages are joined in this order into one new PDF."));
        ui.add_space(8.0);
        if let Some(name) = &current_name {
            ui.switch(&mut c.with_current, format!("Start with {name}"));
            ui.add_space(4.0);
        }
        let mut remove = None;
        let mut swap = None;
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            let n = c.files.len();
            for (k, path) in c.files.iter().enumerate() {
                theme::inner_card(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(theme::secondary(ui, format!("{}", k + 1 + c.with_current as usize)));
                        ui.label(path.file_name().unwrap_or_default().to_string_lossy()).on_hover_text(path.display().to_string());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").on_hover_text("Remove").clicked() {
                                remove = Some(k);
                            }
                            if ui.add_enabled(k + 1 < n, egui::Button::new("↓").small()).clicked() {
                                swap = Some((k, k + 1));
                            }
                            if ui.add_enabled(k > 0, egui::Button::new("↑").small()).clicked() {
                                swap = Some((k - 1, k));
                            }
                        });
                    });
                });
            }
        });
        if let Some(k) = remove {
            c.files.remove(k);
        }
        if let Some((a, b)) = swap {
            c.files.swap(a, b);
        }
        if ui.button("Add files…").clicked()
            && let Some(more) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_files()
        {
            c.files.extend(more);
        }
        let total = c.files.len() + c.with_current as usize;
        ui.add_space(6.0);
        ui.switch(&mut c.open_after, "Open the combined file");
        if total < 2 {
            ui.label(theme::secondary(ui, "Add at least two files to combine."));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Combine…", total >= 2) {
                run = true;
            }
            if secondary_button(ui, "Cancel") {
                open = false;
            }
        });
        let _ = p;
    });
    if close {
        open = false;
    }
    if run {
        let Some(target) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name("Combined.pdf").save_file() else { return true };
        let out = Doc::new_empty();
        let mut failed = Vec::new();
        if c.with_current {
            ed.commit_edits();
            ed.commit();
            if let Some(f) = &ed.file {
                out.import(&f.doc, None, out.page_count());
            }
        }
        for path in &c.files {
            match std::fs::read(path).ok().and_then(|b| Doc::open(b, None).ok()) {
                Some(src) => {
                    if !out.import(&src, None, out.page_count()) {
                        failed.push(path.clone());
                    }
                }
                None => failed.push(path.clone()),
            }
        }
        let pages = out.page_count();
        match out.save().and_then(|b| std::fs::write(&target, b).map_err(|e| e.to_string())) {
            Ok(()) => {
                ed.say(format!("Combined {} into {}", plural(pages, "page", "pages"), target.file_name().unwrap_or_default().to_string_lossy()));
                if c.open_after {
                    if ed.needs_save() {
                        ed.say("Combined file saved. The open document has unsaved changes, so it stays open.");
                    } else {
                        ed.open_path(&target);
                    }
                }
            }
            Err(e) => ed.error = Some(format!("Could not save {}: {e}", target.display())),
        }
        if !failed.is_empty() {
            let names: Vec<String> = failed.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
            ed.error = Some(format!("These files were left out (damaged or password protected): {}", names.join(", ")));
        }
        open = false;
    }
    open
}

// ── split ──

#[derive(Clone, Copy, PartialEq, Eq)]
enum SplitMode {
    Every,
    Ranges,
}

pub struct Split {
    mode: SplitMode,
    every: usize,
    ranges: String,
    folder: Option<PathBuf>,
}

impl Split {
    pub fn new(ed: &Editor) -> Split {
        let n = ed.file.as_ref().map_or(1, |f| f.page_count());
        let folder = ed.file.as_ref().and_then(|f| f.path.as_ref()).and_then(|p| p.parent()).map(|p| p.to_path_buf());
        Split { mode: SplitMode::Every, every: 1, ranges: if n > 1 { format!("1-{}, {}-{}", n / 2, n / 2 + 1, n) } else { "1".into() }, folder }
    }
}

pub fn split(ctx: &egui::Context, ed: &mut Editor, s: &mut Split) -> bool {
    let Some(f) = &ed.file else { return false };
    let p = pal(ctx);
    let n = f.page_count();
    let stem = f.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into());
    let mut open = true;
    let mut run = false;
    let groups: Result<Vec<Vec<usize>>, String> = match s.mode {
        SplitMode::Every => Ok((0..n).collect::<Vec<_>>().chunks(s.every.max(1)).map(|c| c.to_vec()).collect()),
        SplitMode::Ranges => parse_ranges(&s.ranges, n),
    };
    let (_, close) = modal(ctx, "split", 400.0, |ui| {
        title(ui, "Split into files");
        ui.label(theme::secondary(ui, format!("{} has {}.", f.name(), plural(n, "page", "pages"))));
        ui.add_space(8.0);
        ui.segmented(&mut s.mode, &[(SplitMode::Every, "Every few pages"), (SplitMode::Ranges, "Page ranges")]);
        ui.add_space(6.0);
        match s.mode {
            SplitMode::Every => {
                ui.horizontal(|ui| {
                    ui.label("A new file every");
                    ui.add(egui::DragValue::new(&mut s.every).range(1..=n.max(1)).speed(0.1));
                    ui.label(if s.every == 1 { "page" } else { "pages" });
                });
            }
            SplitMode::Ranges => {
                ui.add(egui::TextEdit::singleline(&mut s.ranges).desired_width(f32::INFINITY).hint_text("For example 1-3, 4-10, 11"));
                ui.label(theme::secondary(ui, "Each range becomes one file."));
            }
        }
        ui.add_space(6.0);
        match &groups {
            Ok(g) => {
                ui.label(theme::secondary(ui, format!("Makes {}.", plural(g.len(), "file", "files"))));
            }
            Err(e) => {
                ui.label(RichText::new(e).color(p.danger));
            }
        }
        theme::heading(ui, "SAVE TO");
        let label = s.folder.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "No folder chosen".into());
        ui.add(egui::Label::new(theme::secondary(ui, label.clone())).truncate()).on_hover_text(label);
        if ui.button("Choose folder…").clicked()
            && let Some(d) = rfd::FileDialog::new().pick_folder()
        {
            s.folder = Some(d);
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Split", groups.is_ok() && s.folder.is_some()) {
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
        let (Ok(groups), Some(folder)) = (groups, s.folder.clone()) else { return true };
        let mut written = 0;
        for (k, g) in groups.iter().enumerate() {
            let out = Doc::new_empty();
            if !out.import(&f.doc, Some(g), 0) {
                continue;
            }
            let name = match s.mode {
                SplitMode::Every if s.every == 1 => format!("{stem} page {}.pdf", g[0] + 1),
                SplitMode::Every => format!("{stem} part {}.pdf", k + 1),
                SplitMode::Ranges => format!("{stem} pages {}.pdf", page_ranges(g)),
            };
            let target = folder.join(name);
            if out.save().and_then(|b| std::fs::write(&target, b).map_err(|e| e.to_string())).is_ok() {
                written += 1;
            }
        }
        if written == groups.len() {
            ed.say(format!("Saved {} in {}", plural(written, "file", "files"), folder.display()));
        } else {
            ed.error = Some(format!("Only {written} of {} files could be saved in {}.", groups.len(), folder.display()));
        }
        open = false;
    }
    open
}

// ── signature ──

#[derive(Clone, Copy, PartialEq, Eq)]
enum SigMode {
    Draw,
    Type,
    Image,
}

pub struct SignaturePad {
    mode: SigMode,
    strokes: Vec<Vec<Pos2>>,
    drawing: bool,
    text: String,
    image: Option<PathBuf>,
}

impl Default for SignaturePad {
    fn default() -> Self {
        SignaturePad { mode: SigMode::Draw, strokes: Vec::new(), drawing: false, text: String::new(), image: None }
    }
}

/// Returns false once the window is closed.
pub fn signature(ctx: &egui::Context, ed: &mut Editor, pad: &mut SignaturePad, script_font: bool, no_script_font: bool) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut done = false;
    let (_, close) = modal(ctx, "signature", 460.0, |ui| {
        title(ui, "Your signature");
        ui.label(theme::secondary(ui, "Dockit keeps it on this computer for the next time you sign."));
        ui.add_space(8.0);
        ui.segmented(&mut pad.mode, &[(SigMode::Draw, "Draw"), (SigMode::Type, "Type"), (SigMode::Image, "Image")]);
        ui.add_space(8.0);
        let ink = Color32::from_rgb(20, 30, 70);
        match pad.mode {
            SigMode::Draw => {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 170.0), Sense::drag());
                let painter = ui.painter_at(r);
                painter.rect_filled(r, 8.0, Color32::WHITE);
                painter.rect_stroke(r, 8.0, Stroke::new(1.0, p.hairline), StrokeKind::Inside);
                // A signing line, as on paper.
                let y = r.bottom() - 42.0;
                painter.line_segment([pos2(r.left() + 24.0, y), pos2(r.right() - 24.0, y)], Stroke::new(1.0, Color32::from_gray(205)));
                let local = |q: Pos2| q - r.min.to_vec2();
                if resp.drag_started() {
                    pad.strokes.push(Vec::new());
                    pad.drawing = true;
                }
                if resp.dragged()
                    && pad.drawing
                    && let (Some(q), Some(s)) = (resp.interact_pointer_pos(), pad.strokes.last_mut())
                {
                    let q = local(q.clamp(r.min, r.max));
                    if s.last().is_none_or(|l| l.distance(q) > 1.0) {
                        s.push(q);
                    }
                }
                if resp.drag_stopped() {
                    pad.drawing = false;
                }
                for s in &pad.strokes {
                    let pts: Vec<Pos2> = s.iter().map(|q| r.min + q.to_vec2()).collect();
                    if pts.len() == 1 {
                        painter.circle_filled(pts[0], 1.4, ink);
                    } else {
                        painter.add(egui::Shape::line(pts, Stroke::new(2.4, ink)));
                    }
                }
                if pad.strokes.is_empty() {
                    painter.text(
                        r.center() - vec2(0.0, 12.0),
                        egui::Align2::CENTER_CENTER,
                        "Sign here with the mouse or a pen",
                        egui::FontId::proportional(13.0),
                        Color32::from_gray(160),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!pad.strokes.is_empty(), egui::Button::new("Clear")).clicked() {
                        pad.strokes.clear();
                    }
                    if ui.add_enabled(!pad.strokes.is_empty(), egui::Button::new("Undo stroke")).clicked() {
                        pad.strokes.pop();
                    }
                });
            }
            SigMode::Type => {
                let resp = ui.add(egui::TextEdit::singleline(&mut pad.text).desired_width(f32::INFINITY).hint_text("Your name"));
                if pad.text.is_empty() && !resp.has_focus() {
                    resp.request_focus();
                }
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::hover());
                ui.painter().rect_filled(r, 8.0, Color32::WHITE);
                let family = if script_font { egui::FontFamily::Name("script".into()) } else { egui::FontFamily::Proportional };
                let shown = if pad.text.is_empty() { "Your name" } else { pad.text.as_str() };
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    shown,
                    egui::FontId::new(34.0, family),
                    if pad.text.is_empty() { Color32::from_gray(190) } else { ink },
                );
                if no_script_font {
                    ui.label(theme::secondary(ui, "No handwriting font is installed, so the signature uses a plain font."));
                }
            }
            SigMode::Image => {
                ui.label(theme::secondary(ui, "A picture of your signature, ideally a PNG with a transparent background."));
                ui.horizontal(|ui| {
                    if ui.button("Choose image…").clicked()
                        && let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp"]).pick_file()
                    {
                        pad.image = Some(path);
                    }
                    if let Some(path) = &pad.image {
                        ui.label(path.file_name().unwrap_or_default().to_string_lossy());
                    }
                });
            }
        }
        let ready = match pad.mode {
            SigMode::Draw => !pad.strokes.is_empty(),
            SigMode::Type => !pad.text.trim().is_empty(),
            SigMode::Image => pad.image.is_some(),
        };
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Use signature", ready) {
                done = true;
            }
            if secondary_button(ui, "Cancel") {
                open = false;
            }
        });
    });
    // A click outside should not throw away a drawing in progress; only Escape or Cancel close.
    if close && ctx.input(|i| i.key_pressed(Key::Escape)) {
        open = false;
    }
    if done {
        ed.signature = match pad.mode {
            SigMode::Draw => normalise(&pad.strokes),
            SigMode::Type => Some(Signature::Typed { text: pad.text.trim().to_string() }),
            SigMode::Image => pad.image.clone().map(|path| Signature::Image { path }),
        };
        if ed.signature.is_some() {
            ed.tool = crate::editor::Tool::Sign;
            ed.say("Click on a page to place your signature.");
        }
        open = false;
    }
    open
}

/// Strokes scaled so the drawing is 1 unit tall, starting at (0, 0).
fn normalise(strokes: &[Vec<Pos2>]) -> Option<Signature> {
    let all: Vec<Pos2> = strokes.iter().flatten().copied().collect();
    if all.is_empty() {
        return None;
    }
    let b = Rect::from_points(&all);
    let h = b.height().max(1.0);
    let strokes = strokes.iter().map(|s| s.iter().map(|q| ((q.x - b.left()) / h, (q.y - b.top()) / h)).collect()).collect();
    Some(Signature::Drawn { strokes, aspect: (b.width().max(1.0) / h).max(0.2) })
}
