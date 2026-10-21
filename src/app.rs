//! The window: top bar, dock area, dialogs, shortcuts, file drops and settings.

use crate::canvas;
use crate::dialogs::{self, Combine, SaveChoice, SignaturePad, Split};
use crate::editor::{Editor, Request, Signature, Style, Tool};
use crate::panels;
use crate::theme::{self, Appearance, UiExt, pal};
use egui::{Key, KeyboardShortcut, Margin, Modifiers, Ui, ViewportCommand, vec2};
use egui_dock::{DockArea, DockState, NodeIndex, TabStyle, TabViewer};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Tab {
    Document,
    Pages,
    Tools,
    Properties,
    Comments,
    Form,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Settings {
    appearance: Appearance,
    dock_version: u32,
    dock: Option<DockState<Tab>>,
    recent: Vec<PathBuf>,
    style: Option<Style>,
    signature: Option<Signature>,
}

const SETTINGS_KEY: &str = "dockit-settings";
const DOCK_VERSION: u32 = 1;

fn default_dock() -> DockState<Tab> {
    let mut d = DockState::new(vec![Tab::Document]);
    let s = d.main_surface_mut();
    // The document takes most of the window: pages on the left, tools and details on the right.
    let [doc, _pages] = s.split_left(NodeIndex::root(), 0.14, vec![Tab::Pages]);
    let [_doc, right] = s.split_right(doc, 0.78, vec![Tab::Tools]);
    s.split_below(right, 0.46, vec![Tab::Properties, Tab::Comments, Tab::Form]);
    d
}

/// The handwriting face used to preview typed signatures. egui only uses a newly added font from
/// the next frame on, so it is loaded when the signature window first opens and used after that.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScriptFont {
    NotLoaded,
    Loading,
    Ready,
    Missing,
}

/// Something that closes or replaces the document, waiting on the "Save changes?" dialog.
#[derive(Clone, Debug)]
enum Pending {
    Quit,
    Close,
    Open,
    OpenFile(PathBuf),
}

pub struct App {
    ed: Editor,
    dock: DockState<Tab>,
    appearance: Appearance,
    title: String,
    closing: bool,
    ask_save: Option<Pending>,
    combine: Option<Combine>,
    split: Option<Split>,
    pad: Option<SignaturePad>,
    script_font: ScriptFont,
    /// Ctrl+V was down last frame (so one press pastes once).
    v_was_down: bool,
    last_paste: Option<std::time::Instant>,
    /// Set when PDFium could not be loaded; the app shows why instead of the editor.
    broken: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, broken: Option<String>) -> Self {
        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();
        theme::apply(&cc.egui_ctx, settings.appearance);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        crate::fonts::warm_up();
        let mut ed = Editor::default();
        ed.recent = settings.recent;
        if let Some(s) = settings.style {
            ed.style = s;
        }
        ed.signature = settings.signature;
        if broken.is_none()
            && let Some(arg) = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.exists())
        {
            ed.open_path(&arg);
        }
        let dock = settings.dock.filter(|_| settings.dock_version == DOCK_VERSION).unwrap_or_else(default_dock);
        App {
            ed,
            dock,
            appearance: settings.appearance,
            title: String::new(),
            closing: false,
            ask_save: None,
            combine: None,
            split: None,
            pad: None,
            script_font: ScriptFont::NotLoaded,
            v_was_down: false,
            last_paste: None,
            broken,
        }
    }

    fn request(&mut self, ctx: &egui::Context, action: Pending) {
        self.ed.commit_edits();
        self.ed.commit();
        if self.ed.needs_save() {
            self.ask_save = Some(action);
        } else {
            self.perform(ctx, action);
        }
    }

    fn perform(&mut self, ctx: &egui::Context, action: Pending) {
        match action {
            Pending::Quit => {
                self.closing = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            Pending::Close => self.close_doc(),
            Pending::Open => self.ed.open_dialog(),
            Pending::OpenFile(p) => self.ed.open_path(&p),
        }
        self.ed.reset_view_caches();
    }

    fn close_doc(&mut self) {
        self.ed.close();
        self.ed.reset_view_caches();
    }

    fn save_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.ask_save.clone() else { return };
        let name = self.ed.file.as_ref().map(|f| f.name()).unwrap_or_default();
        match dialogs::save_changes(ctx, &name, matches!(action, Pending::Quit)) {
            Some(SaveChoice::Save) => {
                self.ask_save = None;
                if self.ed.save(false) {
                    self.perform(ctx, action);
                }
            }
            Some(SaveChoice::Discard) => {
                self.ask_save = None;
                self.perform(ctx, action);
            }
            Some(SaveChoice::Cancel) => self.ask_save = None,
            None => {}
        }
    }

    fn modal_open(&self) -> bool {
        self.ask_save.is_some() || self.combine.is_some() || self.split.is_some() || self.pad.is_some() || self.ed.password.is_some() || self.ed.error.is_some()
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.modal_open() {
            return;
        }
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        let cmd_shift = |k| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, k);
        let ev = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        if ev(cmd(Key::O)) {
            self.request(ctx, Pending::Open);
        }
        if ev(cmd(Key::W)) && self.ed.file.is_some() {
            self.request(ctx, Pending::Close);
        }
        if ev(cmd_shift(Key::S)) {
            self.ed.save(true);
        } else if ev(cmd(Key::S)) {
            self.ed.save(false);
        }
        if self.ed.file.is_none() {
            return;
        }
        // Zoom works while typing too.
        if ev(cmd(Key::Equals)) || ev(cmd(Key::Plus)) {
            self.ed.set_zoom(self.ed.zoom * 1.25, None);
        }
        if ev(cmd(Key::Minus)) {
            self.ed.set_zoom(self.ed.zoom / 1.25, None);
        }
        if ev(cmd(Key::Num0)) {
            self.ed.fit = Some(true);
        }
        if ev(cmd(Key::Num1)) {
            self.ed.set_zoom(1.0, None);
        }
        let typing = ctx.egui_wants_keyboard_input() || self.ed.text_edit.is_some() || self.ed.note.is_some() || self.ed.field_edit.is_some();
        if typing {
            return;
        }
        // Copy, cut and paste of page objects. egui reports Ctrl+V only when the system clipboard
        // holds text, so the key itself is watched too.
        let events = ctx.input(|i| i.events.clone());
        let mut paste = false;
        for e in &events {
            match e {
                egui::Event::Copy => {
                    self.ed.copy_selection();
                }
                egui::Event::Cut => self.ed.cut_selection(),
                egui::Event::Paste(_) => paste = true,
                _ => {}
            }
        }
        let v_down = ctx.input(|i| i.modifiers.command) && ctrl_v_down();
        // The key and the paste event can arrive in different frames: one press pastes once.
        let recent = self.last_paste.is_some_and(|t| t.elapsed().as_millis() < 350);
        if (paste || v_down) && !self.v_was_down && !recent && self.ed.can_paste() {
            self.ed.paste(self.ed.current);
            self.last_paste = Some(std::time::Instant::now());
        }
        self.v_was_down = v_down || paste;
        let ed = &mut self.ed;
        if ev(cmd_shift(Key::Z)) || ev(cmd(Key::Y)) {
            ed.redo();
        } else if ev(cmd(Key::Z)) {
            ed.undo();
        }
        if ev(cmd(Key::A)) {
            ed.select_all();
        }
        if ev(cmd(Key::D)) {
            ed.duplicate_selection();
        }
        if ev(cmd(Key::CloseBracket)) {
            ed.restack(true);
        }
        if ev(cmd(Key::OpenBracket)) {
            ed.restack(false);
        }
        if ev(cmd_shift(Key::G))
            && let crate::editor::Sel::Objects { page, items } = ed.sel.clone()
            && items.len() == 1
            && let Some(parts) = ed.ungroup(page, items[0])
        {
            ed.sel = crate::editor::Sel::Objects { page, items: parts };
        }
        let mods = ctx.input(|i| i.modifiers);
        if mods.is_none() {
            let pressed: Option<Tool> = ctx.input(|i| crate::panels_tools().into_iter().find(|t| i.key_pressed(t.key())));
            if let Some(t) = pressed {
                panels::choose_tool(ed, t);
            }
        }
        ctx.input(|i| {
            if i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace) {
                ed.delete_selection();
            }
            if i.key_pressed(Key::Escape) {
                if ed.sel == crate::editor::Sel::None {
                    ed.tool = Tool::Select;
                    ed.image = None;
                }
                ed.clear_selection();
            }
            if i.key_pressed(Key::Enter)
                && let crate::editor::Sel::Objects { page, items } = ed.sel.clone()
                && items.len() == 1
                && ed.file.as_ref().is_some_and(|f| f.objects(page).iter().any(|o| o.index == items[0] && o.kind == crate::pdf::Kind::Text))
            {
                ed.begin_text_edit(page, items[0]);
            }
            let n = ed.file.as_ref().map_or(0, |f| f.page_count());
            if ed.sel == crate::editor::Sel::None {
                if i.key_pressed(Key::PageDown) {
                    ed.goto = Some((ed.current + 1).min(n.saturating_sub(1)));
                }
                if i.key_pressed(Key::PageUp) {
                    ed.goto = Some(ed.current.saturating_sub(1));
                }
                if i.key_pressed(Key::Home) {
                    ed.goto = Some(0);
                }
                if i.key_pressed(Key::End) {
                    ed.goto = Some(n.saturating_sub(1));
                }
            } else {
                let step = if i.modifiers.shift { 10.0 } else { 1.0 };
                for (k, d) in [
                    (Key::ArrowLeft, vec2(-step, 0.0)),
                    (Key::ArrowRight, vec2(step, 0.0)),
                    (Key::ArrowUp, vec2(0.0, -step)),
                    (Key::ArrowDown, vec2(0.0, step)),
                ] {
                    if i.key_pressed(k) {
                        ed.nudge(d.x, d.y);
                    }
                }
            }
        });
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let p = pal(ui.ctx());
        let has = self.ed.file.is_some();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.add_space(4.0);
            let (r, _) = ui.allocate_exact_size(vec2(17.0, 17.0), egui::Sense::hover());
            theme::paint_mark(ui.painter(), r, p.text, p.accent);
            ui.add_space(-3.0);
            ui.label(egui::RichText::new("dockit").family(theme::display_family()).size(15.0).color(p.text));
            ui.add_space(6.0);
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                    self.request(ui.ctx(), Pending::Open);
                }
                let recent: Vec<PathBuf> = self.ed.recent.iter().filter(|p| p.exists()).cloned().collect();
                ui.add_enabled_ui(!recent.is_empty(), |ui| {
                    ui.menu_button("Open recent", |ui| {
                        for path in recent {
                            if ui.button(path.file_name().unwrap_or_default().to_string_lossy()).on_hover_text(path.display().to_string()).clicked() {
                                self.request(ui.ctx(), Pending::OpenFile(path));
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                if ui.add_enabled(has, egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    self.ed.save(false);
                }
                if ui.add_enabled(has, egui::Button::new("Save as…").shortcut_text("Ctrl+Shift+S")).clicked() {
                    self.ed.save(true);
                }
                if ui.add_enabled(has, egui::Button::new("Close").shortcut_text("Ctrl+W")).clicked() {
                    self.request(ui.ctx(), Pending::Close);
                }
                ui.separator();
                if ui.button("Combine files…").clicked() {
                    self.combine = Some(Combine::new(&self.ed));
                }
                if ui.add_enabled(has, egui::Button::new("Split into files…")).clicked() {
                    self.split = Some(Split::new(&self.ed));
                }
                if ui.add_enabled(has, egui::Button::new("Extract selected pages…")).clicked() {
                    self.ed.extract_pages();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    self.request(ui.ctx(), Pending::Quit);
                }
            });
            ui.add_enabled_ui(has, |ui| {
                ui.menu_button("Edit", |ui| {
                    if ui.add_enabled(self.ed.can_undo(), egui::Button::new("Undo").shortcut_text("Ctrl+Z")).clicked() {
                        self.ed.undo();
                    }
                    if ui.add_enabled(self.ed.can_redo(), egui::Button::new("Redo").shortcut_text("Ctrl+Y")).clicked() {
                        self.ed.redo();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Select all on page").shortcut_text("Ctrl+A")).clicked() {
                        self.ed.select_all();
                    }
                    match self.ed.sel.clone() {
                        crate::editor::Sel::Objects { page, items } => crate::canvas::object_menu(ui, &mut self.ed, page, &items),
                        crate::editor::Sel::Annot { .. } => {
                            if ui.button("Delete").clicked() {
                                self.ed.delete_selection();
                            }
                        }
                        crate::editor::Sel::None => {}
                    }
                    if self.ed.can_paste() && ui.add(egui::Button::new("Paste").shortcut_text("Ctrl+V")).clicked() {
                        let page = self.ed.current;
                        self.ed.paste(page);
                    }
                    ui.separator();
                    if ui.button("Change signature…").clicked() {
                        self.ed.signature_dialog = true;
                    }
                });
                ui.menu_button("Pages", |ui| {
                    let k = self.ed.selected_pages().len();
                    ui.label(theme::secondary(ui, if k > 1 { format!("{k} selected pages") } else { format!("Page {}", self.ed.current + 1) }));
                    if ui.button("Rotate left").clicked() {
                        self.ed.rotate_pages(-1);
                    }
                    if ui.button("Rotate right").clicked() {
                        self.ed.rotate_pages(1);
                    }
                    if ui.button("Duplicate").clicked() {
                        self.ed.duplicate_pages();
                    }
                    if ui.button("Delete").clicked() {
                        self.ed.delete_pages();
                    }
                    ui.separator();
                    let after = self.ed.selected_pages().last().copied().unwrap_or(self.ed.current);
                    if ui.button("Insert blank page").clicked() {
                        self.ed.insert_blank(after);
                    }
                    if ui.button("Insert pages from a file…").clicked() {
                        self.ed.insert_from_files(after);
                    }
                    ui.separator();
                    if ui.button("Extract…").clicked() {
                        self.ed.extract_pages();
                    }
                    if ui.button("Split into files…").clicked() {
                        self.split = Some(Split::new(&self.ed));
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.add(egui::Button::new("Zoom in").shortcut_text("Ctrl++")).clicked() {
                        self.ed.set_zoom(self.ed.zoom * 1.25, None);
                    }
                    if ui.add(egui::Button::new("Zoom out").shortcut_text("Ctrl+-")).clicked() {
                        self.ed.set_zoom(self.ed.zoom / 1.25, None);
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Fit width").shortcut_text("Ctrl+0")).clicked() {
                        self.ed.fit = Some(true);
                    }
                    if ui.button("Fit page").clicked() {
                        self.ed.fit = Some(false);
                    }
                    if ui.add(egui::Button::new("Actual size").shortcut_text("Ctrl+1")).clicked() {
                        self.ed.set_zoom(1.0, None);
                    }
                    ui.separator();
                    if ui.button("Reset panel layout").clicked() {
                        self.dock = default_dock();
                    }
                });
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                ui.scope(|ui| {
                    ui.set_width(190.0);
                    let mut a = self.appearance;
                    if ui.segmented(&mut a, &[(Appearance::System, "System"), (Appearance::Dark, "Dark"), (Appearance::Light, "Light")]) {
                        self.appearance = a;
                        theme::set_appearance(ui.ctx(), a);
                    }
                });
                if let Some(f) = &self.ed.file {
                    ui.add_space(8.0);
                    let zoom = format!("{:.0}%", self.ed.zoom * 100.0);
                    if ui.add(egui::Button::new(theme::secondary(ui, zoom)).frame(false)).on_hover_text("Fit width (Ctrl+0)").clicked() {
                        self.ed.fit = Some(true);
                    }
                    ui.label(theme::secondary(ui, format!("Page {} of {}", self.ed.current + 1, f.page_count())));
                }
                let mut info = String::new();
                if let Some((msg, t)) = &self.ed.status
                    && t.elapsed().as_secs_f32() < 6.0
                {
                    info = msg.clone();
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(6));
                }
                if info.is_empty()
                    && let Some(f) = &self.ed.file
                    && (f.dirty() || self.ed.needs_commit)
                {
                    info = "Unsaved changes".into();
                }
                ui.add(egui::Label::new(egui::RichText::new(info).color(p.text_secondary)).truncate());
            });
        });
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let t = match &self.ed.file {
            Some(f) => format!("{}{} - Dockit", if self.ed.needs_save() { "• " } else { "" }, f.name()),
            None => "Dockit".into(),
        };
        if t != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(t.clone()));
            self.title = t;
        }
    }

    fn drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if dropped.is_empty() {
            return;
        }
        let is_pdf = |p: &PathBuf| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
        let (pdfs, others): (Vec<PathBuf>, Vec<PathBuf>) = dropped.into_iter().partition(is_pdf);
        match pdfs.len() {
            0 => {}
            1 => self.request(ctx, Pending::OpenFile(pdfs[0].clone())),
            _ => self.combine = Some(Combine::with_files(&self.ed, pdfs)),
        }
        // An image dropped on an open document is ready to place.
        if let (Some(img), true) = (others.first(), self.ed.file.is_some()) {
            match crate::editor::PendingImage::load(img) {
                Ok(p) => {
                    self.ed.say(format!("Click on a page to place {}", p.name));
                    self.ed.image = Some(p);
                    self.ed.tool = Tool::Image;
                }
                Err(e) => self.ed.error = Some(e),
            }
        }
    }

    fn save_settings(&mut self, storage: &mut dyn eframe::Storage) {
        let s = Settings {
            appearance: self.appearance,
            dock_version: DOCK_VERSION,
            dock: Some(self.dock.clone()),
            recent: self.ed.recent.clone(),
            style: Some(self.ed.style.clone()),
            signature: self.ed.signature.clone(),
        };
        eframe::set_value(storage, SETTINGS_KEY, &s);
    }
}

struct Viewer<'a> {
    ed: &'a mut Editor,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::Document => "Document",
            Tab::Pages => "Pages",
            Tab::Tools => "Tools",
            Tab::Properties => "Properties",
            Tab::Comments => "Comments",
            Tab::Form => "Form",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Document => canvas::canvas_ui(ui, self.ed),
            Tab::Pages => panels::pages_ui(ui, self.ed),
            Tab::Tools => panels::tools_ui(ui, self.ed),
            Tab::Properties => panels::properties_ui(ui, self.ed),
            Tab::Comments => panels::comments_ui(ui, self.ed),
            Tab::Form => panels::form_ui(ui, self.ed),
        }
    }

    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        [false, false]
    }

    fn tab_style_override(&self, tab: &Tab, global: &TabStyle) -> Option<TabStyle> {
        match tab {
            Tab::Document => {
                let mut s = global.clone();
                s.tab_body.inner_margin = Margin::ZERO;
                Some(s)
            }
            _ => None,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let p = pal(&ctx);
        if let Some(msg) = &self.broken {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(32))).show(ui, |ui| {
                ui.label(theme::semibold("Dockit could not start its PDF engine").size(16.0));
                ui.add_space(6.0);
                ui.label(msg);
                ui.label(theme::secondary(ui, "pdfium.dll must sit in the same folder as Dockit.exe."));
            });
            return;
        }

        // Closing with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.closing {
            self.ed.commit_edits();
            self.ed.commit();
            if self.ed.needs_save() {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.ask_save = Some(Pending::Quit);
            } else {
                self.closing = true;
            }
        }

        self.shortcuts(&ctx);
        self.drops(&ctx);
        self.update_title(&ctx);

        egui::Panel::top("top-bar").frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(6, 4))).show(ui, |ui| self.top_bar(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
            let style = theme::dock_style(ui.ctx());
            DockArea::new(&mut self.dock)
                .style(style)
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut Viewer { ed: &mut self.ed });
        });

        // Windows asked for from panels.
        match self.ed.request.take() {
            Some(Request::Combine) => self.combine = Some(Combine::new(&self.ed)),
            None => {}
        }
        if self.ed.signature_dialog {
            self.ed.signature_dialog = false;
            self.pad = Some(SignaturePad::default());
        }
        if let Some(c) = &mut self.combine
            && !dialogs::combine(&ctx, &mut self.ed, c)
        {
            self.combine = None;
        }
        if let Some(s) = &mut self.split
            && !dialogs::split(&ctx, &mut self.ed, s)
        {
            self.split = None;
        }
        if let Some(pad) = &mut self.pad {
            self.script_font = match self.script_font {
                ScriptFont::NotLoaded => match crate::fonts::script().and_then(|p| std::fs::read(p).ok()) {
                    Some(bytes) => {
                        ctx.add_font(egui::epaint::text::FontInsert::new(
                            "script",
                            egui::FontData::from_owned(bytes),
                            vec![egui::epaint::text::InsertFontFamily {
                                family: egui::FontFamily::Name("script".into()),
                                priority: egui::epaint::text::FontPriority::Highest,
                            }],
                        ));
                        ScriptFont::Loading
                    }
                    None => ScriptFont::Missing,
                },
                ScriptFont::Loading => ScriptFont::Ready,
                s => s,
            };
            if !dialogs::signature(&ctx, &mut self.ed, pad, self.script_font == ScriptFont::Ready, self.script_font == ScriptFont::Missing) {
                self.pad = None;
            }
        }
        dialogs::password(&ctx, &mut self.ed);
        self.save_dialog(&ctx);
        dialogs::error(&ctx, &mut self.ed);

        // Turn settled changes into history states.
        self.ed.tick(&ctx);

        // Highlight the window while files are dragged over it.
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
            let r = self.ed.view.canvas_rect.shrink(8.0);
            painter.rect_stroke(r, 10.0, egui::Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
            let what = if self.ed.file.is_some() { "Drop a PDF to open it, several to combine them, or an image to place it" } else { "Drop a PDF to open it" };
            painter.text(r.center(), egui::Align2::CENTER_CENTER, what, egui::FontId::proportional(14.0), p.accent_text);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.save_settings(storage);
    }
}

/// Is the V key physically down? Used to catch Ctrl+V when the system clipboard holds no text.
#[cfg(windows)]
fn ctrl_v_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { (GetAsyncKeyState(b'V' as i32) as u16 & 0x8000) != 0 }
}

#[cfg(not(windows))]
fn ctrl_v_down() -> bool {
    false
}
