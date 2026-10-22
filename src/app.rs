//! The window: top bar, dock area, dialogs, shortcuts, file drops and settings.

use crate::canvas;
use crate::dialogs::{self, Combine, SaveChoice, SignaturePad, Split};
use crate::commands::{Cmd, Keymap, PageLayout};
use crate::editor::{Editor, Request, Signature, Style, Tool, ViewPrefs};
use crate::panels;
use crate::theme::{self, Appearance, pal};
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
    view_dock: Option<DockState<Tab>>,
    recent: Vec<PathBuf>,
    style: Option<Style>,
    signature: Option<Signature>,
    keys: Keymap,
    view: ViewPrefs,
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

/// Reading layout: page thumbnails beside the document, nothing else.
fn default_view_dock() -> DockState<Tab> {
    let mut d = DockState::new(vec![Tab::Document]);
    let s = d.main_surface_mut();
    s.split_left(NodeIndex::root(), 0.13, vec![Tab::Pages]);
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
    /// Layout while editing.
    dock: DockState<Tab>,
    /// Layout while reading.
    view_dock: DockState<Tab>,
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
    /// Open documents. The active one lives in `ed`; its slot here holds an empty placeholder.
    tabs: Vec<Editor>,
    active: usize,
    crop: Option<crate::pageops::CropDialog>,
    stamp: Option<crate::pageops::StampDialog>,
    compress: Option<crate::pageops::CompressDialog>,
    security: Option<crate::security::SecurityDialog>,
    sign: Option<crate::sign::SignDialog>,
    ocr: Option<crate::ocr::OcrDialog>,
    compare: Option<crate::compare::Compare>,
    printing: Option<crate::print::PrintJob>,
    bates: Option<crate::bates::BatesDialog>,
    find_redact: Option<crate::redact::FindRedactDialog>,
    properties: Option<crate::docinfo::PropertiesDialog>,
    sanitize: Option<crate::docinfo::SanitizeDialog>,
    attachments: Option<crate::docinfo::AttachmentsDialog>,
    batch: Option<crate::batch::BatchDialog>,
    reader: Option<crate::speech::Reader>,
    access: Option<crate::accessibility::CheckDialog>,
    alt: Option<crate::accessibility::AltDialog>,
    standard: Option<crate::standards::StandardDialog>,
    preflight: Option<crate::standards::PreflightDialog>,
    marks: Option<crate::standards::MarksDialog>,
    autosave: crate::recovery::Autosave,
    settings_win: crate::settings::SettingsWindow,
    /// Files handed over by later launches (double-clicked in Explorer).
    incoming: Option<std::sync::mpsc::Receiver<PathBuf>>,
    recovery: Option<crate::recovery::RecoveryDialog>,
    export: Option<crate::pageops::ExportDialog>,
    images: Option<crate::pageops::ImagesDialog>,
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
        ed.keys = settings.keys;
        ed.view_prefs = settings.view;
        if broken.is_none()
            && let Some(arg) = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.exists())
        {
            ed.open_path(&arg);
        }
        let dock = settings.dock.filter(|_| settings.dock_version == DOCK_VERSION).unwrap_or_else(default_dock);
        let view_dock = settings.view_dock.filter(|_| settings.dock_version == DOCK_VERSION).unwrap_or_else(default_view_dock);
        let broken_none = broken.is_none();
        App {
            ed,
            dock,
            view_dock,
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
            tabs: vec![Editor::default()],
            active: 0,
            crop: None,
            stamp: None,
            compress: None,
            security: None,
            sign: None,
            ocr: None,
            compare: None,
            printing: None,
            bates: None,
            find_redact: None,
            properties: None,
            sanitize: None,
            attachments: None,
            batch: None,
            reader: None,
            access: None,
            alt: None,
            standard: None,
            preflight: None,
            marks: None,
            autosave: Default::default(),
            settings_win: Default::default(),
            incoming: broken_none.then(|| crate::instance::listen(cc.egui_ctx.clone())),
            recovery: if broken_none {
                let items = crate::recovery::orphans();
                (!items.is_empty()).then_some(crate::recovery::RecoveryDialog { items })
            } else {
                None
            },
            export: None,
            images: None,
        }
    }

    // ── tabs ──

    /// Make tab `j` the active one. The clipboard, style, signature and recent files go with you.
    fn switch_tab(&mut self, j: usize) {
        if j == self.active || j >= self.tabs.len() {
            return;
        }
        self.ed.commit_edits();
        let clip = self.ed.clip.take();
        let (style, signature, recent) = (self.ed.style.clone(), self.ed.signature.clone(), self.ed.recent.clone());
        let (keys, view_prefs) = (self.ed.keys.clone(), self.ed.view_prefs.clone());
        std::mem::swap(&mut self.ed, &mut self.tabs[self.active]);
        std::mem::swap(&mut self.ed, &mut self.tabs[j]);
        self.active = j;
        self.ed.clip = clip;
        self.ed.style = style;
        self.ed.signature = signature;
        self.ed.recent = recent;
        self.ed.keys = keys;
        self.ed.view_prefs = view_prefs;
    }

    /// Open a new, empty tab and switch to it.
    fn new_tab(&mut self) {
        let mut fresh = Editor::default();
        fresh.recent = self.ed.recent.clone();
        fresh.view_prefs = self.ed.view_prefs.clone();
        self.tabs.push(fresh);
        let j = self.tabs.len() - 1;
        self.switch_tab(j);
    }

    /// Close the active tab without asking (the caller has already asked).
    fn close_tab(&mut self) {
        if self.tabs.len() <= 1 {
            self.ed.close();
            self.ed.reset_view_caches();
            return;
        }
        let gone = self.active;
        let next = if gone + 1 < self.tabs.len() { gone + 1 } else { gone - 1 };
        self.switch_tab(next);
        self.tabs.remove(gone);
        if self.active > gone {
            self.active -= 1;
        }
    }

    /// A tab other than the active one with unsaved changes.
    fn other_dirty_tab(&self) -> Option<usize> {
        (0..self.tabs.len()).find(|&i| i != self.active && self.tabs[i].needs_save())
    }

    /// Where a document should open: the active tab when it is empty, otherwise a new tab.
    fn tab_for_opening(&mut self) {
        if self.ed.file.is_some() {
            self.new_tab();
        }
    }

    fn request(&mut self, ctx: &egui::Context, action: Pending) {
        self.ed.commit_edits();
        self.ed.commit();
        // Opening never replaces a document: it gets its own tab.
        if matches!(action, Pending::Open | Pending::OpenFile(_)) || !self.ed.needs_save() {
            self.perform(ctx, action);
        } else {
            self.ask_save = Some(action);
        }
    }

    fn perform(&mut self, ctx: &egui::Context, action: Pending) {
        match action {
            Pending::Quit => {
                // This tab was saved or let go; ask about the next one with changes.
                if let Some(i) = self.other_dirty_tab() {
                    let done = self.active;
                    self.switch_tab(i);
                    self.tabs.remove(done);
                    if self.active > done {
                        self.active -= 1;
                    }
                    self.ask_save = Some(Pending::Quit);
                    return;
                }
                self.closing = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            Pending::Close => self.close_tab(),
            Pending::Open => {
                if let Some(p) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file() {
                    self.tab_for_opening();
                    self.ed.open_path(&p);
                }
            }
            Pending::OpenFile(p) => {
                self.tab_for_opening();
                self.ed.open_path(&p);
            }
        }
        self.ed.reset_view_caches();
    }

    /// Tab strip, shown when more than one document is open.
    fn tab_bar(&mut self, ui: &mut Ui) {
        let p = pal(ui.ctx());
        let names: Vec<(String, bool)> = (0..self.tabs.len())
            .map(|i| {
                let e = if i == self.active { &self.ed } else { &self.tabs[i] };
                (e.file.as_ref().map(|f| f.name()).unwrap_or_else(|| "New tab".into()), e.needs_save())
            })
            .collect();
        let mut switch = None;
        let mut close = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, (name, dirty)) in names.iter().enumerate() {
                let on = i == self.active;
                let frame = egui::Frame::new()
                    .fill(if on { p.bg_elevated } else { egui::Color32::TRANSPARENT })
                    .corner_radius(7)
                    .inner_margin(Margin::symmetric(10, 4));
                frame.show(ui, |ui| {
                    let label = format!("{}{name}", if *dirty { "• " } else { "" });
                    let text = egui::RichText::new(label).color(if on { p.text } else { p.text_secondary });
                    if ui.add(egui::Label::new(text).sense(egui::Sense::click()).truncate()).clicked() {
                        switch = Some(i);
                    }
                    if ui.add(egui::Button::new(egui::RichText::new("×").size(10.0).color(p.text_secondary)).frame(false)).on_hover_text("Close").clicked() {
                        close = Some(i);
                    }
                });
            }
            if ui.add(egui::Button::new("+").frame(false)).on_hover_text("Open in a new tab (Ctrl+O)").clicked() {
                self.request(ui.ctx(), Pending::Open);
            }
        });
        if let Some(i) = switch {
            self.switch_tab(i);
        }
        if let Some(i) = close {
            self.switch_tab(i);
            self.request(ui.ctx(), Pending::Close);
        }
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
        self.crop.is_some() || self.stamp.is_some() || self.compress.is_some() || self.security.is_some() || self.sign.is_some() || self.ocr.is_some() || self.recovery.is_some() || self.settings_win.open
            || self.bates.is_some()
            || self.find_redact.is_some()
            || self.properties.is_some()
            || self.sanitize.is_some()
            || self.attachments.is_some()
            || self.batch.is_some()
            || self.access.is_some()
            || self.alt.is_some()
            || self.standard.is_some()
            || self.preflight.is_some()
            || self.marks.is_some()
            || self.export.is_some() || self.images.is_some() || self.ask_save.is_some() || self.combine.is_some() || self.split.is_some() || self.pad.is_some() || self.ed.password.is_some() || self.ed.error.is_some()
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.modal_open() || self.settings_win.capturing() || self.ed.palette.is_some() {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input() || self.ed.text_edit.is_some() || self.ed.note.is_some() || self.ed.field_edit.is_some();
        // Commands, through the keymap (Settings > Keyboard shortcuts).
        let presses: Vec<(Key, Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, m) in presses {
            let Some(c) = self.ed.keys.command_for(key, m) else { continue };
            if typing && !c.while_typing() {
                continue;
            }
            // While reading, keys never start changing the document.
            if !self.ed.editing && c.edits() {
                continue;
            }
            ctx.input_mut(|i| i.consume_key(m, key));
            self.run(ctx, c);
        }
        if self.ed.file.is_none() || typing {
            return;
        }
        let ed = &mut self.ed;
        // Ctrl+Shift+Z redoes too.
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z))) {
            ed.redo();
        }
        // Selected text goes to the clipboard.
        if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) && !matches!(ed.sel, crate::editor::Sel::Objects { .. })
            && let Some(t) = ed.selected_text()
        {
            ctx.copy_text(t);
            ed.say("Copied the selected text");
        }
        if !ed.editing {
            ctx.input(|i| {
                if i.key_pressed(Key::PageDown) || i.key_pressed(Key::ArrowRight) {
                    ed.next_page();
                }
                if i.key_pressed(Key::PageUp) || i.key_pressed(Key::ArrowLeft) {
                    ed.prev_page();
                }
                if i.key_pressed(Key::Home) {
                    ed.goto = Some(0);
                }
                if i.key_pressed(Key::End) {
                    let n = ed.file.as_ref().map_or(0, |f| f.page_count());
                    ed.goto = Some(ed.row_start(n.saturating_sub(1)));
                }
            });
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
                if i.key_pressed(Key::PageDown) || i.key_pressed(Key::ArrowRight) {
                    ed.next_page();
                }
                if i.key_pressed(Key::PageUp) || i.key_pressed(Key::ArrowLeft) {
                    ed.prev_page();
                }
                if i.key_pressed(Key::Home) {
                    ed.goto = Some(0);
                }
                if i.key_pressed(Key::End) {
                    ed.goto = Some(ed.row_start(n.saturating_sub(1)));
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

    /// Can `c` run now?
    fn enabled(&self, c: Cmd) -> bool {
        let ed = &self.ed;
        if c.needs_doc() && ed.file.is_none() {
            return false;
        }
        let objects = matches!(ed.sel, crate::editor::Sel::Objects { .. });
        match c {
            Cmd::Undo => ed.can_undo(),
            Cmd::Redo => ed.can_redo(),
            Cmd::Duplicate | Cmd::BringFront | Cmd::SendBack => objects,
            Cmd::Ungroup => matches!(&ed.sel, crate::editor::Sel::Objects { page, items } if items.len() == 1
                && ed.file.as_ref().is_some_and(|f| f.objects(*page).iter().any(|o| o.index == items[0] && o.kind == crate::pdf::Kind::Group))),
            Cmd::ApplyRedactions => !ed.redaction_marks().is_empty(),
            Cmd::CoverPage => ed.layout.two(),
            _ => true,
        }
    }

    /// Run a command, from a menu, a shortcut or the palette.
    fn run(&mut self, ctx: &egui::Context, c: Cmd) {
        if !self.enabled(c) {
            return;
        }
        // Commands that change the document switch to editing first.
        if c.edits() && !self.ed.editing {
            self.ed.set_editing(true);
            if !self.ed.editing {
                return;
            }
        }
        let ed = &mut self.ed;
        let insert_after = ed.selected_pages().last().copied().unwrap_or(ed.current);
        match c {
            Cmd::Open => self.request(ctx, Pending::Open),
            Cmd::Save => {
                ed.save(false);
            }
            Cmd::SaveAs => {
                ed.save(true);
            }
            Cmd::Close => self.request(ctx, Pending::Close),
            Cmd::ExportImages => self.export = Some(crate::pageops::ExportDialog::new(&self.ed)),
            Cmd::Print => {
                ed.commit_edits();
                self.printing = crate::print::begin(&mut self.ed);
            }
            Cmd::Find => match &mut ed.find {
                Some(f) => f.focus = true,
                None => ed.find = Some(crate::find::Find::open()),
            },
            Cmd::ReadAloud => {
                if self.reader.take().is_none() {
                    match crate::speech::Reader::new() {
                        Ok(mut r) => {
                            if r.start(&self.ed) {
                                self.reader = Some(r);
                            }
                        }
                        Err(e) => self.ed.error = Some(e),
                    }
                }
            }
            Cmd::DarkPages => {
                ed.view_prefs.dark_pages = !ed.view_prefs.dark_pages;
                ed.reset_view_caches();
            }
            Cmd::Quit => self.request(ctx, Pending::Quit),
            Cmd::ToggleEdit => {
                let on = !ed.editing;
                ed.set_editing(on);
            }
            Cmd::Undo => ed.undo(),
            Cmd::Redo => ed.redo(),
            Cmd::SelectAll => ed.select_all(),
            Cmd::Duplicate => ed.duplicate_selection(),
            Cmd::BringFront => ed.restack(true),
            Cmd::SendBack => ed.restack(false),
            Cmd::Ungroup => {
                if let crate::editor::Sel::Objects { page, items } = ed.sel.clone()
                    && let Some(parts) = ed.ungroup(page, items[0])
                {
                    ed.sel = crate::editor::Sel::Objects { page, items: parts };
                }
            }
            Cmd::ApplyRedactions => {
                let yes = rfd::MessageDialog::new()
                    .set_title("Apply redactions")
                    .set_description("Everything under the marks will be removed from the document for good once you save. Apply now?")
                    .set_level(rfd::MessageLevel::Warning)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if yes {
                    ed.apply_redactions();
                }
            }
            Cmd::ChangeSignature => ed.signature_dialog = true,
            Cmd::RotateLeft => ed.rotate_pages(-1),
            Cmd::RotateRight => ed.rotate_pages(1),
            Cmd::DuplicatePages => ed.duplicate_pages(),
            Cmd::DeletePages => ed.delete_pages(),
            Cmd::InsertBlank => ed.insert_blank(insert_after),
            Cmd::InsertFromFile => ed.insert_from_files(insert_after),
            Cmd::ExtractPages => ed.extract_pages(),
            Cmd::Crop => self.crop = Some(crate::pageops::CropDialog::new()),
            Cmd::Stamp => self.stamp = Some(crate::pageops::StampDialog::new()),
            Cmd::Flatten => ed.flatten(),
            Cmd::ZoomIn => ed.set_zoom(ed.zoom * 1.25, None),
            Cmd::ZoomOut => ed.set_zoom(ed.zoom / 1.25, None),
            Cmd::FitWidth => ed.fit = Some(true),
            Cmd::FitPage => ed.fit = Some(false),
            Cmd::ActualSize => ed.set_zoom(1.0, None),
            Cmd::Layout(l) => {
                let cover = ed.cover;
                ed.set_layout(l, cover);
                // A page at a time fits the page; a column fits the width.
                ed.fit = Some(!l.paged());
            }
            Cmd::CoverPage => {
                let (l, cover) = (ed.layout, !ed.cover);
                ed.set_layout(l, cover);
            }
            Cmd::ResetPanels => {
                if ed.editing {
                    self.dock = default_dock();
                } else {
                    self.view_dock = default_view_dock();
                }
            }
            Cmd::Palette => ed.palette = Some(crate::palette::Palette { focus: true, ..Default::default() }),
            Cmd::Combine => self.combine = Some(Combine::new(&self.ed)),
            Cmd::Split => self.split = Some(Split::new(&self.ed)),
            Cmd::FromImages => {
                if let Some(files) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"]).pick_files() {
                    self.images = Some(crate::pageops::ImagesDialog::new(files));
                }
            }
            Cmd::Compress => self.compress = Some(crate::pageops::CompressDialog::new()),
            Cmd::Ocr => self.ocr = Some(crate::ocr::OcrDialog::new()),
            Cmd::Compare => {
                if let Some(other) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file()
                    && let Some(f) = &self.ed.file
                {
                    match crate::compare::Compare::new(&f.doc, other) {
                        Ok(c) => self.compare = Some(c),
                        Err(e) => self.ed.error = Some(e),
                    }
                }
            }
            Cmd::Security => self.security = Some(crate::security::SecurityDialog::new(&self.ed)),
            Cmd::SignDigitally => self.sign = Some(crate::sign::SignDialog::new(&self.ed, None)),
            Cmd::Bates => self.bates = Some(crate::bates::BatesDialog::new()),
            Cmd::FindRedact => self.find_redact = Some(crate::redact::FindRedactDialog::new()),
            Cmd::Properties => self.properties = Some(crate::docinfo::PropertiesDialog::new(&self.ed)),
            Cmd::Sanitize => self.sanitize = Some(crate::docinfo::SanitizeDialog::new()),
            Cmd::Attachments => self.attachments = Some(crate::docinfo::AttachmentsDialog::new()),
            Cmd::Batch => self.batch = Some(crate::batch::BatchDialog::new()),
            Cmd::DetectFields => ed.detect_fields(),
            Cmd::AccessCheck => self.access = Some(crate::accessibility::CheckDialog::new(&self.ed)),
            Cmd::AltText => self.alt = Some(crate::accessibility::AltDialog::new(&self.ed)),
            Cmd::PdfA => self.standard = Some(crate::standards::StandardDialog::new(crate::standards::Standard::PdfA)),
            Cmd::PdfX => self.standard = Some(crate::standards::StandardDialog::new(crate::standards::Standard::PdfX)),
            Cmd::Preflight => self.preflight = Some(crate::standards::PreflightDialog::new(&self.ed)),
            Cmd::PrintMarks => self.marks = Some(crate::standards::MarksDialog::new()),
            Cmd::Settings => self.settings_win.open = true,
            Cmd::Tool(t) => panels::choose_tool(ed, t),
        }
    }

    /// A menu entry for `c`, with its shortcut, greyed out when it cannot run.
    fn item(&mut self, ui: &mut Ui, c: Cmd) {
        let b = egui::Button::new(c.name()).shortcut_text(self.ed.keys.text(c));
        if ui.add_enabled(self.enabled(c), b).clicked() {
            self.run(ui.ctx(), c);
            ui.close();
        }
    }

    /// A menu entry that shows whether it is on.
    fn check_item(&mut self, ui: &mut Ui, c: Cmd, on: bool) {
        let b = egui::Button::selectable(on, c.name()).shortcut_text(self.ed.keys.text(c));
        if ui.add_enabled(self.enabled(c), b).clicked() {
            self.run(ui.ctx(), c);
            ui.close();
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let p = pal(ui.ctx());
        let has = self.ed.file.is_some();
        // As in Grapht and Overlay: the menus start the row; the window's own title bar carries the
        // icon and the name.
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                self.item(ui, Cmd::Open);
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
                self.item(ui, Cmd::Save);
                self.item(ui, Cmd::SaveAs);
                self.item(ui, Cmd::Close);
                ui.separator();
                self.item(ui, Cmd::ExportImages);
                self.item(ui, Cmd::Print);
                ui.separator();
                self.item(ui, Cmd::Quit);
            });
            let editing = self.ed.editing;
            // Reading shows File, View, Tools and Settings; editing adds Edit and Pages.
            if editing && has {
                ui.menu_button("Edit", |ui| {
                    self.item(ui, Cmd::Undo);
                    self.item(ui, Cmd::Redo);
                    ui.separator();
                    self.item(ui, Cmd::SelectAll);
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
                    if self.enabled(Cmd::ApplyRedactions) {
                        ui.separator();
                        self.item(ui, Cmd::ApplyRedactions);
                    }
                    ui.separator();
                    self.item(ui, Cmd::ChangeSignature);
                });
                ui.menu_button("Pages", |ui| {
                    let k = self.ed.selected_pages().len();
                    ui.label(theme::secondary(ui, if k > 1 { format!("{k} selected pages") } else { format!("Page {}", self.ed.current + 1) }));
                    self.item(ui, Cmd::RotateLeft);
                    self.item(ui, Cmd::RotateRight);
                    self.item(ui, Cmd::DuplicatePages);
                    self.item(ui, Cmd::DeletePages);
                    ui.separator();
                    self.item(ui, Cmd::InsertBlank);
                    self.item(ui, Cmd::InsertFromFile);
                    self.item(ui, Cmd::ExtractPages);
                    ui.separator();
                    self.item(ui, Cmd::Crop);
                    self.item(ui, Cmd::Stamp);
                    ui.separator();
                    self.item(ui, Cmd::Flatten);
                });
            }
            ui.menu_button("View", |ui| {
                self.item(ui, Cmd::Find);
                ui.separator();
                self.item(ui, Cmd::ZoomIn);
                self.item(ui, Cmd::ZoomOut);
                ui.separator();
                self.item(ui, Cmd::FitWidth);
                self.item(ui, Cmd::FitPage);
                self.item(ui, Cmd::ActualSize);
                ui.separator();
                for l in PageLayout::ALL {
                    let on = self.ed.layout == l;
                    self.check_item(ui, Cmd::Layout(l), on);
                }
                let cover = self.ed.cover;
                self.check_item(ui, Cmd::CoverPage, cover);
                ui.separator();
                let dark = self.ed.view_prefs.dark_pages;
                self.check_item(ui, Cmd::DarkPages, dark);
                let reading = self.reader.is_some();
                self.check_item(ui, Cmd::ReadAloud, reading);
                ui.separator();
                self.item(ui, Cmd::Palette);
                self.item(ui, Cmd::ResetPanels);
            });
            ui.menu_button("Tools", |ui| {
                self.item(ui, Cmd::Combine);
                self.item(ui, Cmd::Split);
                self.item(ui, Cmd::FromImages);
                self.item(ui, Cmd::Batch);
                ui.separator();
                self.item(ui, Cmd::Compress);
                self.item(ui, Cmd::Ocr);
                self.item(ui, Cmd::Compare);
                self.item(ui, Cmd::Bates);
                ui.separator();
                self.item(ui, Cmd::Properties);
                self.item(ui, Cmd::Attachments);
                self.item(ui, Cmd::DetectFields);
                ui.menu_button("Accessibility", |ui| {
                    self.item(ui, Cmd::AccessCheck);
                    self.item(ui, Cmd::AltText);
                });
                ui.menu_button("Standards and print", |ui| {
                    self.item(ui, Cmd::PdfA);
                    self.item(ui, Cmd::PdfX);
                    ui.separator();
                    self.item(ui, Cmd::Preflight);
                    self.item(ui, Cmd::PrintMarks);
                });
                ui.separator();
                self.item(ui, Cmd::Security);
                self.item(ui, Cmd::Tool(Tool::Redact));
                self.item(ui, Cmd::FindRedact);
                self.item(ui, Cmd::Sanitize);
                self.item(ui, Cmd::SignDigitally);
            });
            // Settings has its own place in the bar.
            if ui.button("Settings").on_hover_text(format!("Settings ({})", self.ed.keys.text(Cmd::Settings))).clicked() {
                self.settings_win.open = true;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                if has {
                    if self.ed.editing {
                        if ui.add(egui::Button::new("Done").min_size(vec2(72.0, 0.0))).on_hover_text(format!("Back to reading ({})", self.ed.keys.text(Cmd::ToggleEdit))).clicked() {
                            self.ed.set_editing(false);
                        }
                    } else {
                        let edit = egui::Button::new(theme::semibold("Edit PDF").color(egui::Color32::WHITE)).fill(p.accent).min_size(vec2(84.0, 0.0));
                        if ui.add(edit).on_hover_text(format!("Edit text, images, pages and more ({})", self.ed.keys.text(Cmd::ToggleEdit))).clicked() {
                            self.ed.set_editing(true);
                        }
                    }
                }
                if let Some(f) = &self.ed.file {
                    ui.add_space(8.0);
                    let zoom = format!("{:.0}%", self.ed.zoom * 100.0);
                    if ui.add(egui::Button::new(theme::secondary(ui, zoom)).frame(false)).on_hover_text("Fit width").clicked() {
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
            view_dock: Some(self.view_dock.clone()),
            recent: self.ed.recent.clone(),
            style: Some(self.ed.style.clone()),
            signature: self.ed.signature.clone(),
            keys: self.ed.keys.clone(),
            view: self.ed.view_prefs.clone(),
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
            // Ask about the active tab first, then any other tab with changes.
            if !self.ed.needs_save()
                && let Some(i) = self.other_dirty_tab()
            {
                self.switch_tab(i);
            }
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
        if self.tabs.len() > 1 {
            egui::Panel::top("tab-bar").frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(8, 2))).show(ui, |ui| self.tab_bar(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
            let style = theme::dock_style(ui.ctx());
            let dock = if self.ed.editing { &mut self.dock } else { &mut self.view_dock };
            DockArea::new(dock)
                .style(style)
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut Viewer { ed: &mut self.ed });
        });

        // Windows asked for from panels.
        match self.ed.request.take() {
            Some(Request::Combine) => self.combine = Some(Combine::new(&self.ed)),
            Some(Request::SignDigitally(field)) => self.sign = Some(crate::sign::SignDialog::new(&self.ed, field)),
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
        if let Some(d) = &mut self.crop
            && !crate::pageops::crop_window(&ctx, &mut self.ed, d)
        {
            self.crop = None;
        }
        if let Some(d) = &mut self.stamp
            && !crate::pageops::stamp_window(&ctx, &mut self.ed, d)
        {
            self.stamp = None;
        }
        if let Some(d) = &mut self.compress
            && !crate::pageops::compress_window(&ctx, &mut self.ed, d)
        {
            self.compress = None;
        }
        if let Some(d) = &mut self.security
            && !crate::security::security_window(&ctx, &mut self.ed, d)
        {
            self.security = None;
        }
        if let Some(d) = &mut self.sign
            && !crate::sign::sign_window(&ctx, &mut self.ed, d)
        {
            self.sign = None;
        }
        if let Some(d) = &mut self.ocr
            && !crate::ocr::ocr_window(&ctx, &mut self.ed, d)
        {
            self.ocr = None;
        }
        if let Some(c) = &mut self.compare
            && !crate::compare::compare_window(&ctx, self.ed.file.as_ref().map(|f| &f.doc), c)
        {
            self.compare = None;
        }
        if let Some(d) = &mut self.recovery {
            use crate::recovery::RecoveryAction;
            match crate::recovery::recovery_window(&ctx, d) {
                Some(RecoveryAction::Open(list)) => {
                    let opened: Vec<u64> = list.iter().map(|m| m.uid).collect();
                    for m in &list {
                        self.tab_for_opening();
                        self.ed.open_recovered(m);
                    }
                    self.ed.reset_view_caches();
                    if let Some(d) = &mut self.recovery {
                        d.items.retain(|m| !opened.contains(&m.uid));
                        if d.items.is_empty() {
                            self.recovery = None;
                        }
                    }
                }
                Some(RecoveryAction::Later) => self.recovery = None,
                None => {}
            }
        }

        crate::settings::show(&ctx, &mut self.settings_win, &mut self.ed, &mut self.appearance);
        if let Some(d) = &mut self.bates
            && !crate::bates::bates_window(&ctx, &mut self.ed, d)
        {
            self.bates = None;
        }
        if let Some(d) = &mut self.find_redact
            && !crate::redact::find_redact_window(&ctx, &mut self.ed, d)
        {
            self.find_redact = None;
        }
        if let Some(d) = &mut self.properties
            && !crate::docinfo::properties_window(&ctx, &mut self.ed, d)
        {
            self.properties = None;
        }
        if let Some(d) = &mut self.sanitize
            && !crate::docinfo::sanitize_window(&ctx, &mut self.ed, d)
        {
            self.sanitize = None;
        }
        if let Some(d) = &mut self.attachments
            && !crate::docinfo::attachments_window(&ctx, &mut self.ed, d)
        {
            self.attachments = None;
        }
        if let Some(d) = &mut self.batch
            && !crate::batch::batch_window(&ctx, d)
        {
            self.batch = None;
        }
        if let Some(d) = &mut self.access {
            let (keep, ask) = crate::accessibility::check_window(&ctx, &mut self.ed, d);
            if !keep {
                self.access = None;
            }
            match ask {
                Some(crate::accessibility::CheckAsk::Properties) => self.properties = Some(crate::docinfo::PropertiesDialog::new(&self.ed)),
                Some(crate::accessibility::CheckAsk::Ocr) => self.ocr = Some(crate::ocr::OcrDialog::new()),
                Some(crate::accessibility::CheckAsk::AltText) => self.alt = Some(crate::accessibility::AltDialog::new(&self.ed)),
                None => {}
            }
        }
        if let Some(d) = &mut self.alt
            && !crate::accessibility::alt_window(&ctx, &mut self.ed, d)
        {
            self.alt = None;
        }
        if let Some(d) = &mut self.standard
            && !crate::standards::standard_window(&ctx, &mut self.ed, d)
        {
            self.standard = None;
        }
        if let Some(d) = &mut self.preflight
            && !crate::standards::preflight_window(&ctx, &mut self.ed, d)
        {
            self.preflight = None;
        }
        if let Some(d) = &mut self.marks
            && !crate::standards::marks_window(&ctx, &mut self.ed, d)
        {
            self.marks = None;
        }
        if let Some(r) = &mut self.reader
            && !crate::speech::reader_bar(&ctx, &mut self.ed, r)
        {
            self.reader = None;
        }
        if self.ed.find.is_some() && !crate::find::find_bar(&ctx, &mut self.ed) {
            self.ed.find = None;
        }
        if let Some(job) = &mut self.printing {
            if job.step(&mut self.ed) {
                let msg = format!("Printing page {}", job.progress());
                self.ed.say(msg);
                ctx.request_repaint();
            } else {
                self.printing = None;
            }
        }
        let enabled_cmds: Vec<Cmd> = crate::commands::sections().into_iter().flat_map(|(_, v)| v).filter(|c| self.enabled(*c)).collect();
        if let Some(c) = crate::palette::show(&ctx, &mut self.ed, &|c| enabled_cmds.contains(&c)) {
            self.run(&ctx, c);
        }
        let incoming: Vec<PathBuf> = self.incoming.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for path in incoming {
            self.request(&ctx, Pending::OpenFile(path));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }

        // Keep copies of unsaved work, and drop them on a normal close.
        if self.closing {
            self.autosave.clear();
        } else {
            self.autosave.tick(std::iter::once(&self.ed).chain(self.tabs.iter()));
            if self.ed.needs_save() || self.tabs.iter().any(|t| t.needs_save()) {
                ctx.request_repaint_after(crate::recovery::EVERY);
            }
        }
        if let Some(d) = &mut self.export
            && !crate::pageops::export_window(&ctx, &mut self.ed, d)
        {
            self.export = None;
        }
        if let Some(d) = &mut self.images {
            let (open, made) = crate::pageops::images_window(&ctx, d);
            if !open {
                self.images = None;
            }
            if let Some(doc) = made {
                self.tab_for_opening();
                self.ed.open_untitled(doc);
                self.ed.reset_view_caches();
            }
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
