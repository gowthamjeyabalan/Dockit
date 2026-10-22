//! The Settings window: everything that can be set, in sections, with the keyboard shortcuts last.

use crate::canvas::color_row;
use crate::commands::{Cmd, Combo, PageLayout};
use crate::editor::{Editor, FontKind, Unit};
use crate::theme::{self, Appearance, UiExt, pal};
use egui::{CornerRadius, RichText, ScrollArea, Stroke, Ui, vec2};

const SECTIONS: [&str; 7] = ["Appearance", "Viewing", "Editing", "Measuring", "Signatures", "Keyboard shortcuts", "About"];

/// The window's own state, kept by the app.
#[derive(Default)]
pub struct SettingsWindow {
    pub open: bool,
    section: usize,
    /// The command waiting for its new keys.
    capturing: Option<Cmd>,
}

impl SettingsWindow {
    /// While a new shortcut is being pressed, keys belong to the window.
    pub fn capturing(&self) -> bool {
        self.open && self.capturing.is_some()
    }
}

/// One setting: its name and a line about it on the left, the control on the right.
fn row(ui: &mut Ui, name: &str, about: &str, control: impl FnOnce(&mut Ui)) {
    let p = pal(ui.ctx());
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w * 0.5).max(180.0));
            ui.label(RichText::new(name).size(13.5).color(p.text));
            if !about.is_empty() {
                ui.label(RichText::new(about).size(12.0).color(p.text_secondary));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| control(ui));
    });
    ui.add_space(12.0);
}

fn group(ui: &mut Ui, title: &str) {
    ui.add_space(8.0);
    theme::heading(ui, title);
    ui.add_space(6.0);
}

pub fn show(ctx: &egui::Context, win: &mut SettingsWindow, ed: &mut Editor, appearance: &mut Appearance) {
    if !win.open {
        return;
    }
    let p = pal(ctx);
    let screen = ctx.content_rect();
    let size = vec2(840.0f32.min(screen.width() - 40.0), 600.0f32.min(screen.height() - 60.0));
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::same(0)).corner_radius(12);
    let modal = egui::Modal::new(egui::Id::new("settings")).frame(frame).show(ctx, |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            // ── sections, on the left ──
            egui::Frame::new().fill(p.bg).corner_radius(CornerRadius { nw: 12, sw: 12, ne: 0, se: 0 }).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(176.0);
                    ui.set_min_height(size.y - 24.0);
                    ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                    ui.label(theme::semibold("Settings").size(16.0).color(p.text));
                    ui.add_space(12.0);
                    for (k, name) in SECTIONS.iter().enumerate() {
                        let sel = win.section == k;
                        let text = RichText::new(*name).size(13.0).color(if sel { p.text } else { p.text_secondary });
                        if ui.add(egui::Button::selectable(sel, text).min_size(vec2(176.0, 30.0))).clicked() {
                            win.section = k;
                            win.capturing = None;
                        }
                    }
                });
            });
            // ── the section, on the right ──
            ui.vertical(|ui| {
                ui.set_width(size.x - 200.0);
                ui.set_min_height(size.y);
                ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.label(theme::semibold(SECTIONS[win.section]).size(18.0).color(p.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(14.0);
                        if ui.add(egui::Button::new(RichText::new("×").size(17.0)).frame(false)).on_hover_text("Close (Esc)").clicked() {
                            win.open = false;
                        }
                    });
                });
                ui.add_space(6.0);
                ui.separator();
                ScrollArea::vertical().id_salt(("settings-body", win.section)).auto_shrink([false, false]).show(ui, |ui| {
                    egui::Frame::new().inner_margin(egui::Margin { left: 20, right: 22, top: 4, bottom: 16 }).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match win.section {
                            0 => appearance_section(ui, appearance),
                            1 => viewing(ui, ed),
                            2 => editing(ui, ed),
                            3 => measuring(ui, ed),
                            4 => signatures(ui, ed),
                            5 => shortcuts(ui, win, ed),
                            _ => about(ui),
                        }
                    });
                });
            });
        });
    });
    // Esc or a click outside closes it, unless Esc is cancelling a shortcut being set.
    if modal.should_close() && win.capturing.is_none() {
        win.open = false;
    }
    if !win.open {
        win.capturing = None;
    }
}

fn appearance_section(ui: &mut Ui, appearance: &mut Appearance) {
    group(ui, "LOOK");
    row(ui, "Mode", "Light, dark, or the same as Windows.", |ui| {
        ui.set_width(230.0);
        let mut a = *appearance;
        if ui.segmented(&mut a, &[(Appearance::System, "System"), (Appearance::Dark, "Dark"), (Appearance::Light, "Light")]) {
            *appearance = a;
            theme::set_appearance(ui.ctx(), a);
        }
    });
}

fn viewing(ui: &mut Ui, ed: &mut Editor) {
    group(ui, "PAGE LAYOUT");
    row(ui, "Layout for documents", "How pages are laid out when a document opens. View changes it for the document you are reading.", |ui| {
        egui::ComboBox::from_id_salt("settings-layout").width(200.0).selected_text(ed.view_prefs.layout.name()).show_ui(ui, |ui| {
            for l in PageLayout::ALL {
                ui.selectable_value(&mut ed.view_prefs.layout, l, l.name());
            }
        });
    });
    row(ui, "Cover page alone", "With two pages side by side, show the first page by itself, like a book's cover, so spreads pair up as printed.", |ui| {
        ui.switch(&mut ed.view_prefs.cover, "");
    });
    row(ui, "Remember each document's layout", "Open a document with the layout it had last time.", |ui| {
        ui.switch(&mut ed.view_prefs.remember, "");
    });
    group(ui, "OPENING");
    row(ui, "Open documents for editing", "When off, documents open for reading and Edit PDF switches to editing.", |ui| {
        ui.switch(&mut ed.view_prefs.open_editing, "");
    });
}

fn editing(ui: &mut Ui, ed: &mut Editor) {
    let s = &mut ed.style;
    group(ui, "NEW TEXT");
    row(ui, "Font", "", |ui| {
        ui.set_width(220.0);
        ui.segmented(&mut s.font, &[(FontKind::Sans, "Sans"), (FontKind::Serif, "Serif"), (FontKind::Mono, "Mono")]);
    });
    row(ui, "Size", "", |ui| {
        ui.add(egui::DragValue::new(&mut s.text_size).range(4.0..=200.0).speed(0.2).suffix(" pt"));
    });
    row(ui, "Colour", "", |ui| {
        color_row(ui, &mut s.text_color);
    });
    group(ui, "SHAPES");
    row(ui, "Outline width", "For rectangles, ellipses and lines.", |ui| {
        ui.add(egui::DragValue::new(&mut s.width).range(0.25..=40.0).speed(0.1).suffix(" pt"));
    });
    row(ui, "Outline colour", "", |ui| {
        color_row(ui, &mut s.stroke);
    });
    group(ui, "REVIEW");
    row(ui, "Markup colour", "Arrows, text boxes, callouts and measurements.", |ui| {
        color_row(ui, &mut s.review_color);
    });
    row(ui, "Highlight colour", "", |ui| {
        color_row(ui, &mut s.highlight);
    });
}

fn measuring(ui: &mut Ui, ed: &mut Editor) {
    let s = &mut ed.style;
    group(ui, "MEASURE TOOL");
    row(ui, "Units", "", |ui| {
        ui.set_width(220.0);
        ui.segmented(&mut s.unit, &[(Unit::Mm, "mm"), (Unit::Cm, "cm"), (Unit::In, "in"), (Unit::Pt, "pt")]);
    });
    row(ui, "Scale 1 :", "For scale drawings: 1 : 100 means 1 mm on the page is 100 mm in reality.", |ui| {
        ui.add(egui::DragValue::new(&mut s.scale).range(0.001..=100000.0).speed(1.0).max_decimals(3));
    });
}

fn signatures(ui: &mut Ui, ed: &mut Editor) {
    let p = pal(ui.ctx());
    group(ui, "HANDWRITTEN");
    row(ui, "Your signature", "Placed with the Sign tool.", |ui| {
        if ui.button(if ed.signature.is_some() { "Change…" } else { "Create…" }).clicked() {
            ed.signature_dialog = true;
        }
        if ed.signature.is_some() && ui.button(RichText::new("Forget").color(p.danger)).clicked() {
            ed.signature = None;
        }
    });
    group(ui, "DIGITAL");
    let current = ed.style.pfx.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "None yet".into());
    row(ui, "Certificate", &format!("Used by Sign digitally. {current}"), |ui| {
        if ed.style.pfx.is_some() && ui.button(RichText::new("Forget").color(p.danger)).clicked() {
            ed.style.pfx = None;
        }
    });
}

fn shortcuts(ui: &mut Ui, win: &mut SettingsWindow, ed: &mut Editor) {
    let p = pal(ui.ctx());
    // A shortcut being set takes the next key pressed.
    if let Some(a) = win.capturing {
        let press = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some((*key, *modifiers)),
                _ => None,
            })
        });
        if let Some((key, m)) = press {
            win.capturing = None;
            if !(key == egui::Key::Escape && m.is_none()) {
                let c = Combo::from_press(key, m);
                if let Some(other) = ed.keys.user_of(&c, a) {
                    ed.keys.set(other, None);
                    ed.say(format!("{} moved from {} to {}", c.label(), other.name(), a.name()));
                }
                ed.keys.set(a, Some(c));
            }
        }
    }
    ui.label(RichText::new("Click a shortcut, then press the keys for it. A shortcut can only belong to one command: giving it to another moves it there.").size(12.0).color(p.text_secondary));
    ui.add_space(6.0);
    if ui.button("Reset all to the defaults").clicked() {
        ed.keys.reset_all();
        win.capturing = None;
    }
    for (title, cmds) in crate::commands::sections() {
        group(ui, &title.to_uppercase());
        for a in cmds {
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 300.0).max(80.0);
                ui.allocate_ui_with_layout(vec2(w, 26.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(a.name()).size(13.0).color(p.text)).truncate());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let changed = !ed.keys.is_default(a);
                    if ui.add_enabled(changed, egui::Button::new(RichText::new("Reset").size(11.5)).frame(false)).on_hover_text("Back to the default").clicked() {
                        let d = crate::commands::default_combo(a);
                        if let Some(c) = &d
                            && let Some(other) = ed.keys.user_of(c, a)
                        {
                            ed.keys.set(other, None);
                        }
                        ed.keys.set(a, d);
                    }
                    if ui.add_enabled(ed.keys.get(a).is_some(), egui::Button::new(RichText::new("Clear").size(11.5)).frame(false)).on_hover_text("No shortcut").clicked() {
                        ed.keys.set(a, None);
                    }
                    let waiting = win.capturing == Some(a);
                    let text = if waiting { "Press keys… (Esc cancels)".to_owned() } else { ed.keys.get(a).map(|c| c.label()).unwrap_or_else(|| "None".into()) };
                    let rt = RichText::new(text).size(12.5).color(if waiting {
                        p.accent_text
                    } else if ed.keys.get(a).is_some() {
                        p.text
                    } else {
                        p.text_secondary
                    });
                    let b = egui::Button::new(rt).min_size(vec2(150.0, 26.0)).corner_radius(CornerRadius::same(6)).stroke(Stroke::new(1.0, if waiting { p.accent } else { p.hairline }));
                    if ui.add(b).on_hover_text("Click, then press the new keys").clicked() {
                        win.capturing = if waiting { None } else { Some(a) };
                    }
                });
            });
            ui.add_space(4.0);
        }
    }
    group(ui, "FIXED KEYS");
    for (keys, what) in [
        ("Esc", "Drop the selection; again for the Select tool"),
        ("Delete, Backspace", "Delete the selection"),
        ("Enter", "Edit the selected text"),
        ("Arrow keys", "Nudge the selection (Shift for 10); turn pages when nothing is selected"),
        ("Page Up, Page Down, Home, End", "Previous, next, first and last page"),
        ("Ctrl+C, Ctrl+X, Ctrl+V", "Copy, cut, paste"),
        ("Space + drag, middle button", "Pan"),
        ("Ctrl + wheel", "Zoom"),
        ("Shift while drawing", "Keep lines at 45° steps and boxes square"),
        ("Alt while moving", "Skip snapping"),
        ("Ctrl+Shift+Z", "Redo, as well as the Redo shortcut"),
    ] {
        ui.horizontal(|ui| {
            ui.label(RichText::new(what).size(13.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(keys).size(12.5).color(p.text_secondary));
            });
        });
        ui.add_space(4.0);
    }
}

fn about(ui: &mut Ui) {
    let p = pal(ui.ctx());
    ui.add_space(8.0);
    ui.label(theme::semibold(format!("Dockit {}", env!("CARGO_PKG_VERSION"))).size(15.0).color(p.text));
    ui.label(RichText::new("Edit, combine, split, sign and fill in PDFs.").size(13.0).color(p.text_secondary));
    ui.add_space(10.0);
    ui.label(RichText::new("Written in Rust with egui and PDFium. The look follows the Grapht design system. Released under the MIT licence.").size(12.5).color(p.text_secondary));
    ui.add_space(6.0);
    ui.hyperlink_to("github.com/gowthamjeyabalan/Dockit", "https://github.com/gowthamjeyabalan/Dockit");
}
