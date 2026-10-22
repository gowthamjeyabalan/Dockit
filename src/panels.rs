//! The dockable side panels: Tools, Pages, Properties, Comments and Form.

use crate::canvas::{THUMB_W, color_row};
use crate::content::Edit;
use crate::annots::{self, NewField};
use crate::editor::{Editor, FontKind, Markup, Sel, Tool, Unit, page_ranges, plural};
use crate::pdf::{self, Kind};
use crate::theme::{self, UiExt, pal};
use egui::{Color32, CornerRadius, Key, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

fn no_document(ui: &mut Ui) {
    ui.add_space(6.0);
    ui.label(theme::secondary(ui, "No document open"));
}

// ── Tools ──

const SECTIONS: [(&str, &[Tool]); 5] = [
    ("EDIT", &[Tool::Select, Tool::Text, Tool::Image]),
    ("SHAPES", &[Tool::Rect, Tool::Ellipse, Tool::Line]),
    ("REVIEW", &[Tool::Highlight, Tool::Note, Tool::Ink, Tool::Stamp, Tool::Arrow, Tool::TextBox, Tool::Callout, Tool::Measure]),
    ("FORMS", &[Tool::Field]),
    ("PROTECT", &[Tool::Redact, Tool::Sign]),
];

pub const STAMPS: [&str; 6] = ["Approved", "Draft", "Confidential", "Final", "Reviewed", "Not approved"];

/// Choose a tool, doing what it needs first (picking an image, making a signature).
pub fn choose_tool(ed: &mut Editor, t: Tool) {
    ed.commit_edits();
    if t == Tool::Image && ed.image.is_none() && !ed.pick_image() {
        return;
    }
    if t == Tool::Sign && ed.signature.is_none() {
        ed.signature_dialog = true;
    }
    ed.tool = t;
}

fn tool_button(ui: &mut Ui, ed: &mut Editor, t: Tool, w: f32) {
    let p = pal(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
    let on = ed.tool == t;
    let fill = if on {
        p.accent
    } else if resp.hovered() {
        p.bg_control_hover
    } else {
        p.bg_control
    };
    ui.painter().rect_filled(r, CornerRadius::same(7), fill);
    let text = if on { Color32::WHITE } else { p.text };
    let key = if on { Color32::from_white_alpha(170) } else { p.text_secondary };
    ui.painter().text(r.left_center() + vec2(9.0, 0.0), egui::Align2::LEFT_CENTER, t.name(), egui::FontId::new(12.5, theme::medium_family()), text);
    ui.painter().text(
        r.right_center() - vec2(9.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        ed.keys.text(crate::commands::Cmd::Tool(t)),
        egui::FontId::new(11.0, egui::FontFamily::Proportional),
        key,
    );
    if resp.on_hover_text(t.hint()).clicked() {
        choose_tool(ed, t);
    }
}

pub fn tools_ui(ui: &mut Ui, ed: &mut Editor) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        theme::panel_body(ui, |ui| {
            let full = ui.available_width();
            let cols = if full >= 200.0 { 2 } else { 1 };
            let w = ((full - 6.0 * (cols as f32 - 1.0)) / cols as f32).floor();
            for (title, tools) in SECTIONS {
                theme::heading(ui, title);
                for row in tools.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for &t in row {
                            tool_button(ui, ed, t, w);
                        }
                    });
                }
            }
            ui.add_space(6.0);
            ui.separator();
            tool_options(ui, ed);
        });
    });
}

fn tool_options(ui: &mut Ui, ed: &mut Editor) {
    let s = &mut ed.style;
    match ed.tool {
        Tool::Select => {}
        Tool::Text => {
            theme::heading(ui, "NEW TEXT");
            ui.segmented(&mut s.font, &[(FontKind::Sans, "Sans"), (FontKind::Serif, "Serif"), (FontKind::Mono, "Mono")]);
            ui.horizontal(|ui| {
                ui.switch(&mut s.bold, "Bold");
                ui.switch(&mut s.italic, "Italic");
            });
            ui.horizontal(|ui| {
                ui.label("Size");
                ui.add(egui::DragValue::new(&mut s.text_size).range(4.0..=200.0).speed(0.2).suffix(" pt"));
            });
            color_row(ui, &mut s.text_color);
        }
        Tool::Image => {
            theme::heading(ui, "IMAGE");
            match &ed.image {
                Some(img) => {
                    ui.label(&img.name);
                    ui.label(theme::secondary(ui, format!("{} × {} pixels", img.w, img.h)));
                }
                None => {
                    ui.label(theme::secondary(ui, "No image picked"));
                }
            }
            if ui.button("Pick image…").clicked() {
                ed.pick_image();
            }
        }
        Tool::Rect | Tool::Ellipse | Tool::Line => {
            theme::heading(ui, "OUTLINE");
            color_row(ui, &mut s.stroke);
            ui.horizontal(|ui| {
                ui.label("Width");
                ui.add(egui::DragValue::new(&mut s.width).range(0.25..=40.0).speed(0.1).suffix(" pt"));
            });
            if ed.tool != Tool::Line {
                theme::heading(ui, "FILL");
                let mut filled = s.fill.is_some();
                if ui.switch(&mut filled, "Fill the shape").changed() {
                    s.fill = filled.then_some([10, 132, 255, 60]);
                }
                if let Some(fc) = &mut s.fill {
                    color_row(ui, fc);
                    let mut a = fc[3] as f32 / 255.0 * 100.0;
                    ui.horizontal(|ui| {
                        ui.label("Opacity");
                        if ui.add(egui::Slider::new(&mut a, 5.0..=100.0).suffix("%").fixed_decimals(0)).changed() {
                            fc[3] = (a / 100.0 * 255.0).round() as u8;
                        }
                    });
                }
            }
        }
        Tool::Highlight => {
            theme::heading(ui, "MARK");
            ui.segmented(&mut s.markup, &[(Markup::Highlight, "Highlight"), (Markup::Underline, "Underline"), (Markup::Strikeout, "Strike")]);
            color_row(ui, &mut s.highlight);
        }
        Tool::Note => {
            theme::heading(ui, "NOTE COLOUR");
            color_row(ui, &mut s.note);
        }
        Tool::Ink => {
            theme::heading(ui, "PEN");
            color_row(ui, &mut s.ink);
            ui.horizontal(|ui| {
                ui.label("Width");
                ui.add(egui::DragValue::new(&mut s.ink_width).range(0.25..=20.0).speed(0.1).suffix(" pt"));
            });
        }
        Tool::Stamp => {
            theme::heading(ui, "STAMP");
            egui::ComboBox::from_id_salt("stamp-preset").selected_text(s.stamp.clone()).show_ui(ui, |ui| {
                for st in STAMPS {
                    ui.selectable_value(&mut s.stamp, st.to_string(), st);
                }
            });
            ui.add(egui::TextEdit::singleline(&mut s.stamp).hint_text("Custom text"));
            color_row(ui, &mut s.stamp_color);
        }
        Tool::Arrow | Tool::TextBox | Tool::Callout | Tool::Measure => {
            theme::heading(ui, "COLOUR");
            color_row(ui, &mut s.review_color);
            ui.horizontal(|ui| {
                ui.label("Line width");
                ui.add(egui::DragValue::new(&mut s.review_width).range(0.5..=10.0).speed(0.1).suffix(" pt"));
            });
            if matches!(ed.tool, Tool::TextBox | Tool::Callout) {
                theme::heading(ui, "TEXT");
                ui.horizontal(|ui| {
                    ui.label("Size");
                    ui.add(egui::DragValue::new(&mut s.box_size).range(6.0..=72.0).speed(0.2).suffix(" pt"));
                });
                color_row(ui, &mut s.text_color);
            }
            if ed.tool == Tool::Measure {
                theme::heading(ui, "UNITS");
                ui.segmented(&mut s.unit, &[(Unit::Mm, "mm"), (Unit::Cm, "cm"), (Unit::In, "in"), (Unit::Pt, "pt")]);
                ui.horizontal(|ui| {
                    ui.label("Scale  1 :");
                    ui.add(egui::DragValue::new(&mut s.scale).range(0.001..=100000.0).speed(1.0).max_decimals(3));
                });
                ui.label(theme::secondary(ui, "For scale drawings: 1 : 100 means 1 mm on the page is 100 mm in reality."));
            }
        }
        Tool::Redact => {
            theme::heading(ui, "REDACTION");
            let marks = ed.redaction_marks_seen();
            ui.label(theme::secondary(
                ui,
                if marks == 0 { "No areas marked yet.".to_string() } else { format!("{} marked. Review them, then apply.", plural(marks, "area", "areas")) },
            ));
            let confirm_id = egui::Id::new("redact-confirm");
            let confirming = ui.data(|d| d.get_temp::<bool>(confirm_id)).unwrap_or(false);
            if confirming {
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Apply redactions").color(pal(ui.ctx()).danger)).clicked() {
                        ui.data_mut(|d| d.remove::<bool>(confirm_id));
                        ed.apply_redactions();
                    }
                    if ui.button("Cancel").clicked() {
                        ui.data_mut(|d| d.remove::<bool>(confirm_id));
                    }
                });
                ui.label(RichText::new("Everything under the marks will be removed for good once you save.").color(pal(ui.ctx()).danger));
            } else if ui.add_enabled(marks > 0, egui::Button::new("Apply redactions…")).clicked() {
                ui.data_mut(|d| d.insert_temp(confirm_id, true));
            }
        }
        Tool::Field => {
            theme::heading(ui, "NEW FIELD");
            egui::ComboBox::from_id_salt("field-kind").selected_text(s.field.label()).show_ui(ui, |ui| {
                for k in [NewField::Text, NewField::Checkbox, NewField::Dropdown, NewField::Signature] {
                    ui.selectable_value(&mut s.field, k, k.label());
                }
            });
        }
        Tool::Sign => {
            theme::heading(ui, "SIGNATURE");
            match &ed.signature {
                Some(sig) => {
                    let what = match sig {
                        crate::editor::Signature::Drawn { .. } => "Drawn signature saved".to_string(),
                        crate::editor::Signature::Typed { text } => format!("Typed: {text}"),
                        crate::editor::Signature::Image { path } => format!("Image: {}", path.file_name().unwrap_or_default().to_string_lossy()),
                    };
                    ui.label(theme::secondary(ui, what));
                }
                None => {
                    ui.label(theme::secondary(ui, "No signature yet"));
                }
            }
            if ui.button(if ed.signature.is_some() { "Change signature…" } else { "Create signature…" }).clicked() {
                ed.signature_dialog = true;
            }
            ui.add_space(4.0);
            ui.label(theme::secondary(ui, "For a signature that proves who signed, use a certificate:"));
            if ui.button("Sign digitally…").clicked() {
                ed.request = Some(crate::editor::Request::SignDigitally(None));
            }
        }
    }
    ui.add_space(6.0);
    ui.label(theme::secondary(ui, ed.tool.hint()));
}

// ── Pages ──

pub fn pages_ui(ui: &mut Ui, ed: &mut Editor) {
    let Some(f) = &ed.file else { return no_document(ui) };
    let n = f.page_count();
    let p = pal(ui.ctx());
    // Page tools only while editing; reading shows just the thumbnails.
    let editing = ed.editing;
    if editing {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(5.0, 5.0);
            if ui.button("↺").on_hover_text("Rotate left").clicked() {
                ed.rotate_pages(-1);
            }
            if ui.button("↻").on_hover_text("Rotate right").clicked() {
                ed.rotate_pages(1);
            }
            ui.menu_button("Insert", |ui| {
                if ui.button("Blank page").clicked() {
                    let after = ed.selected_pages().last().copied().unwrap_or(ed.current);
                    ed.insert_blank(after);
                    ui.close();
                }
                if ui.button("Pages from a file…").clicked() {
                    let after = ed.selected_pages().last().copied().unwrap_or(ed.current);
                    ed.insert_from_files(after);
                    ui.close();
                }
                if ui.button("Duplicate selected").clicked() {
                    ed.duplicate_pages();
                    ui.close();
                }
            });
            if ui.button("Extract…").on_hover_text("Save the selected pages as a new PDF").clicked() {
                ed.extract_pages();
            }
            if ui.button(RichText::new("Delete").color(p.danger)).clicked() {
                ed.delete_pages();
            }
        });
    }
    let sel = ed.selected_pages();
    let info = if ed.page_sel.len() > 1 { format!("{}, {} selected", plural(n, "page", "pages"), ed.page_sel.len()) } else { plural(n, "page", "pages") };
    ui.label(theme::secondary(ui, info));
    ui.add_space(2.0);

    let ctx = ui.ctx().clone();
    let width = (ui.available_width() - 20.0).clamp(60.0, THUMB_W * 1.6);
    let dragging: Option<usize> = egui::DragAndDrop::payload::<usize>(&ctx).map(|v| *v);
    let mut drop_at: Option<usize> = None;
    let mut budget = 2u32;
    let follow = ed.view_follow_current();

    egui::ScrollArea::vertical().id_salt("pages").auto_shrink([false, false]).show(ui, |ui| {
        let sizes = ed.file.as_ref().map(|f| f.sizes.clone()).unwrap_or_default();
        let clip = ui.clip_rect();
        for (i, &(w, h)) in sizes.iter().enumerate() {
            let th = (width * h / w.max(1.0)).min(width * 2.0);
            let (outer, resp) = ui.allocate_exact_size(vec2(ui.available_width(), th + 26.0), Sense::click_and_drag());
            let r = Rect::from_center_size(pos2(outer.center().x, outer.top() + 4.0 + th / 2.0), vec2(width, th));
            if follow == Some(i) {
                ui.scroll_to_rect(outer, Some(egui::Align::Center));
            }
            if ui.is_rect_visible(outer) {
                let painter = ui.painter();
                painter.rect_filled(r, 0.0, Color32::WHITE);
                if outer.intersects(clip)
                    && let Some(tex) = ed.thumbnail(&ctx, i, &mut budget)
                {
                    painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                let selected = sel.contains(&i) && (!ed.page_sel.is_empty());
                let stroke = if selected {
                    Stroke::new(2.0, p.accent)
                } else if i == ed.current {
                    Stroke::new(1.5, p.text_secondary)
                } else {
                    Stroke::new(1.0, p.hairline)
                };
                painter.rect_stroke(r, 0.0, stroke, StrokeKind::Outside);
                let label = RichText::new(format!("{}", i + 1)).size(11.5).color(if selected { p.accent_text } else { p.text_secondary });
                painter.text(
                    pos2(r.center().x, r.bottom() + 11.0),
                    egui::Align2::CENTER_CENTER,
                    label.text(),
                    egui::FontId::new(11.5, theme::medium_family()),
                    if selected { p.accent_text } else { p.text_secondary },
                );
            }
            // Drop position: before this page if the pointer is in its top half.
            if dragging.is_some()
                && let Some(pos) = ctx.pointer_hover_pos()
                && outer.contains(pos)
            {
                drop_at = Some(if pos.y < outer.center().y { i } else { i + 1 });
            }
            if editing && resp.drag_started() {
                if !sel.contains(&i) || ed.page_sel.is_empty() {
                    ed.page_sel = vec![i];
                }
                egui::DragAndDrop::set_payload(&ctx, i);
            }
            if resp.clicked() {
                let mods = ui.input(|inp| inp.modifiers);
                if mods.command {
                    if let Some(k) = ed.page_sel.iter().position(|&x| x == i) {
                        ed.page_sel.remove(k);
                    } else {
                        ed.page_sel.push(i);
                    }
                } else if mods.shift && !ed.page_sel.is_empty() {
                    let a = *ed.page_sel.last().unwrap();
                    ed.page_sel = if a <= i { (a..=i).collect() } else { (i..=a).rev().collect() };
                } else {
                    ed.page_sel = vec![i];
                    ed.goto = Some(i);
                }
            }
            if resp.double_clicked() {
                ed.goto = Some(i);
            }
            if editing {
                resp.context_menu(|ui| {
                    if !ed.page_sel.contains(&i) {
                        ed.page_sel = vec![i];
                    }
                    let k = ed.selected_pages().len();
                    ui.label(theme::secondary(ui, if k > 1 { format!("{k} pages") } else { format!("Page {}", i + 1) }));
                    if ui.button("Rotate left").clicked() {
                        ed.rotate_pages(-1);
                        ui.close();
                    }
                    if ui.button("Rotate right").clicked() {
                        ed.rotate_pages(1);
                        ui.close();
                    }
                    if ui.button("Duplicate").clicked() {
                        ed.duplicate_pages();
                        ui.close();
                    }
                    if ui.button("Insert blank page after").clicked() {
                        ed.insert_blank(i);
                        ui.close();
                    }
                    if ui.button("Insert pages from a file…").clicked() {
                        ed.insert_from_files(i);
                        ui.close();
                    }
                    if ui.button("Extract…").clicked() {
                        ed.extract_pages();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(RichText::new("Delete").color(pal(ui.ctx()).danger)).clicked() {
                        ed.delete_pages();
                        ui.close();
                    }
                });
            }
        }
        // Insertion line while dragging.
        if let (Some(_), Some(at)) = (dragging, drop_at) {
            let y = ui.min_rect().top() + {
                let sizes = &sizes;
                let mut y = 0.0;
                for (k, &(w, h)) in sizes.iter().enumerate() {
                    if k == at {
                        break;
                    }
                    y += (width * h / w.max(1.0)).min(width * 2.0) + 26.0 + ui.spacing().item_spacing.y;
                }
                y - ui.spacing().item_spacing.y / 2.0
            };
            let x0 = ui.min_rect().center().x - width / 2.0 - 6.0;
            ui.painter().line_segment([pos2(x0, y), pos2(x0 + width + 12.0, y)], Stroke::new(2.5, p.accent));
            // Scroll when dragging near the edges.
            if let Some(pos) = ctx.pointer_hover_pos() {
                let edge = 40.0;
                if pos.y < clip.top() + edge {
                    ui.scroll_with_delta(vec2(0.0, 8.0));
                } else if pos.y > clip.bottom() - edge {
                    ui.scroll_with_delta(vec2(0.0, -8.0));
                }
            }
        }
    });

    if dragging.is_some() {
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        if ctx.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::clear_payload(&ctx);
            if let Some(at) = drop_at {
                let pages = ed.selected_pages();
                ed.move_pages(pages, at);
            }
        }
    }
}

impl Editor {
    /// When the document view moves to another page, the Pages panel scrolls along (once per change).
    fn view_follow_current(&mut self) -> Option<usize> {
        thread_local! { static LAST: std::cell::Cell<Option<(u64, usize)>> = const { std::cell::Cell::new(None) }; }
        let key = (self.epoch, self.current);
        LAST.with(|l| {
            if l.get() != Some(key) {
                l.set(Some(key));
                Some(self.current)
            } else {
                None
            }
        })
    }
}

// ── Properties ──

fn temp_text(ui: &Ui, id: egui::Id, init: impl FnOnce() -> String) -> String {
    ui.data_mut(|d| d.get_temp_mut_or_insert_with(id, init).clone())
}

fn set_temp_text(ui: &Ui, id: egui::Id, s: String) {
    ui.data_mut(|d| d.insert_temp(id, s));
}

fn mm(pt: f32) -> f32 {
    pt / 72.0 * 25.4
}

pub fn properties_ui(ui: &mut Ui, ed: &mut Editor) {
    if ed.file.is_none() {
        return no_document(ui);
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        theme::panel_body(ui, |ui| match ed.sel.clone() {
            Sel::None => document_props(ui, ed),
            Sel::Objects { page, items } => object_props(ui, ed, page, items),
            Sel::Annot { page, index } => annot_props(ui, ed, page, index),
        });
    });
}

fn document_props(ui: &mut Ui, ed: &Editor) {
    let Some(f) = &ed.file else { return };
    theme::heading(ui, "DOCUMENT");
    ui.label(f.name());
    ui.label(theme::secondary(ui, plural(f.page_count(), "page", "pages")));
    if let Some(&(w, h)) = f.sizes.get(ed.current) {
        theme::heading(ui, &format!("PAGE {}", ed.current + 1));
        ui.label(format!("{:.0} × {:.0} mm", mm(w), mm(h)));
        ui.label(theme::secondary(ui, format!("{:.0} × {:.0} pt", w, h)));
    }
    if f.doc.has_form() {
        ui.add_space(4.0);
        ui.label(theme::secondary(ui, "This document has form fields. Click a field to fill it in, or use the Form panel."));
    }
    signatures_props(ui, f);
    ui.add_space(8.0);
    ui.label(theme::secondary(ui, "Select something on a page to see its properties."));
}

fn annot_props(ui: &mut Ui, ed: &mut Editor, page: usize, index: usize) {
    let p = pal(ui.ctx());
    let Some(f) = &ed.file else { return };
    let rev = f.revs[page];
    let Some(a) = f.annots(page).iter().find(|a| a.index == index).cloned() else {
        ed.sel = Sel::None;
        return;
    };
    if let Some(field) = &a.field {
        return field_props(ui, ed, page, index, field.clone(), rev);
    }
    let kind = match (a.subtype, a.intent.as_str()) {
        (pdf::ANNOT_LINE, "LineDimension") => "MEASUREMENT",
        (pdf::ANNOT_LINE, _) => "ARROW",
        (pdf::ANNOT_FREETEXT, "FreeTextCallout") => "CALLOUT",
        _ => "",
    };
    theme::heading(ui, &if kind.is_empty() { a.kind_name().to_uppercase() } else { kind.to_string() });
    ui.label(theme::secondary(ui, format!("Page {}", page + 1)));
    if a.subtype != pdf::ANNOT_INK {
        if a.subtype == pdf::ANNOT_FREETEXT {
            theme::heading(ui, "TEXT");
        } else if a.intent == "LineDimension" {
            theme::heading(ui, "LABEL");
        } else {
            theme::heading(ui, "COMMENT");
        }
        let id = egui::Id::new(("annot-text", page, index, rev));
        let mut text = temp_text(ui, id, || a.contents.clone());
        let resp = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(3).desired_width(f32::INFINITY).hint_text("Add a comment"));
        set_temp_text(ui, id, text.clone());
        if resp.lost_focus() && text != a.contents {
            ed.set_annot_contents(page, index, &text);
        }
    }
    if a.subtype != pdf::ANNOT_STAMP {
        theme::heading(ui, "COLOUR");
        let mut c = a.color.unwrap_or([255, 214, 10, 255]);
        if color_row(ui, &mut c) {
            ed.set_annot_color(page, index, c);
        }
    }
    ui.add_space(10.0);
    if ui.button(RichText::new("Delete").color(p.danger)).clicked() {
        ed.delete_selection();
    }
}

/// Each digital signature and what checking it found.
fn signatures_props(ui: &mut Ui, f: &crate::editor::File) {
    if f.signatures.is_empty() {
        return;
    }
    let p = pal(ui.ctx());
    theme::heading(ui, "SIGNATURES");
    for s in &f.signatures {
        let who = if s.signer.is_empty() { "Unknown signer".to_string() } else { s.signer.clone() };
        let (mark, color) = if s.valid { ("✔", p.accent_text) } else { ("✖", p.danger) };
        ui.horizontal(|ui| {
            ui.label(RichText::new(mark).color(color));
            ui.label(theme::semibold(who));
        });
        if !s.time.is_empty() {
            ui.label(theme::secondary(ui, format!("Signed {}", s.time)));
        }
        if !s.reason.is_empty() {
            ui.label(theme::secondary(ui, format!("Reason: {}", s.reason)));
        }
        if let Some(c) = s.certifies {
            let what = match c {
                1 => "no changes allowed",
                2 => "only form filling and signing allowed",
                _ => "form filling, signing and comments allowed",
            };
            ui.label(theme::secondary(ui, format!("Certified by its author: {what}")));
        }
        let status = match (s.valid, s.whole) {
            (false, _) => "The signature does not match the document: it was changed or the signature is damaged.",
            (true, true) => "The document has not changed since it was signed.",
            (true, false) => "Valid for the version signed; the document was changed after signing.",
        };
        ui.label(RichText::new(status).color(if s.valid { p.text } else { p.danger }).size(12.0));
        ui.label(theme::secondary(
            ui,
            if s.trusted { "The certificate is trusted by Windows." } else { "The certificate is not trusted on this computer (self-signed or unknown issuer)." },
        ));
        ui.add_space(4.0);
    }
}

/// A text field's rules: a number (and its decimals), an allowed range, required, and a value
/// worked out from other fields.
fn rules_ui(ui: &mut Ui, ed: &mut Editor, page: usize, index: usize, field: &pdf::Field, rev: u64) {
    use crate::forms::{Calc, Rules};
    let id = egui::Id::new(("field-rules", page, index, rev));
    let current = Rules::read(field);
    let mut r: Rules = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| current.clone());
    theme::heading(ui, "VALUE");
    let mut number = r.number.is_some();
    if ui.checkbox(&mut number, "A number").changed() {
        r.number = number.then_some(2);
        if !number {
            r.range = None;
        }
    }
    if let Some(dec) = &mut r.number {
        ui.horizontal(|ui| {
            ui.label("Decimals");
            ui.add(egui::DragValue::new(dec).range(0..=6));
        });
        let mut ranged = r.range.is_some();
        if ui.checkbox(&mut ranged, "Allowed range").changed() {
            r.range = ranged.then_some((0.0, 100.0));
        }
        if let Some((lo, hi)) = &mut r.range {
            ui.horizontal(|ui| {
                ui.label("From");
                ui.add(egui::DragValue::new(lo).speed(1.0));
                ui.label("to");
                ui.add(egui::DragValue::new(hi).speed(1.0));
            });
        }
    }
    ui.checkbox(&mut r.required, "Required");
    // Worked out from other fields.
    let others: Vec<String> = ed
        .file
        .as_ref()
        .map(|f| (0..f.page_count()).flat_map(|pg| f.annots(pg).iter().filter_map(|a| a.field.as_ref()).filter(|x| x.kind == pdf::FIELD_TEXT && x.name != field.name).map(|x| x.name.clone()).collect::<Vec<_>>()).collect())
        .unwrap_or_default();
    let mut calc = r.calc.is_some();
    if ui.add_enabled(!others.is_empty(), egui::Checkbox::new(&mut calc, "Worked out from other fields")).changed() {
        r.calc = calc.then(|| (Calc::Sum, Vec::new()));
        if calc && r.number.is_none() {
            r.number = Some(2);
        }
    }
    if let Some((op, names)) = &mut r.calc {
        egui::ComboBox::from_id_salt(("calc-op", page, index)).selected_text(op.name()).show_ui(ui, |ui| {
            for c in Calc::ALL {
                ui.selectable_value(op, c, c.name());
            }
        });
        ui.label(theme::secondary(ui, "of"));
        egui::ScrollArea::vertical().id_salt(("calc-fields", page, index)).max_height(120.0).show(ui, |ui| {
            for o in &others {
                let mut on = names.contains(o);
                if ui.checkbox(&mut on, o).changed() {
                    if on {
                        names.push(o.clone());
                    } else {
                        names.retain(|n| n != o);
                    }
                }
            }
        });
    }
    let changed = r != current;
    if ui.add_enabled(changed, egui::Button::new("Save rules")).clicked() {
        ed.set_field_rules(page, index, &r);
        ui.data_mut(|d| d.remove::<Rules>(id));
    } else if changed {
        ui.data_mut(|d| d.insert_temp(id, r));
    } else {
        ui.data_mut(|d| d.remove::<Rules>(id));
    }
}

fn field_props(ui: &mut Ui, ed: &mut Editor, page: usize, index: usize, field: pdf::Field, rev: u64) {
    let p = pal(ui.ctx());
    theme::heading(ui, &field.kind_name().to_uppercase());
    ui.label(theme::secondary(ui, format!("Page {}", page + 1)));
    theme::heading(ui, "NAME");
    let id = egui::Id::new(("field-name", page, index, rev));
    let mut name = temp_text(ui, id, || field.name.clone());
    let resp = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
    set_temp_text(ui, id, name.clone());
    if (resp.lost_focus() || resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter))) && name.trim() != field.name && !name.trim().is_empty() {
        ed.rename_field(page, index, &name);
    }
    if field.kind == pdf::FIELD_TEXT {
        rules_ui(ui, ed, page, index, &field, rev);
    }
    if matches!(field.kind, pdf::FIELD_COMBO | pdf::FIELD_LIST) {
        theme::heading(ui, "CHOICES");
        let id = egui::Id::new(("field-choices", page, index, rev));
        let current: Vec<String> = field.options.iter().map(|o| o.0.clone()).collect();
        let mut text = temp_text(ui, id, || current.join("\n"));
        let resp = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(4).desired_width(f32::INFINITY).hint_text("One choice per line"));
        set_temp_text(ui, id, text.clone());
        let new: Vec<String> = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        if resp.lost_focus() && new != current {
            ed.set_field_choices(page, index, &new);
        }
        ui.label(theme::secondary(ui, "One choice per line."));
    }
    if field.kind == pdf::FIELD_SIGNATURE && field.value.is_empty() && ui.button("Sign digitally…").clicked() {
        ed.request = Some(crate::editor::Request::SignDigitally(Some((page, index))));
    }
    ui.add_space(4.0);
    ui.label(theme::secondary(ui, "With the Form field tool, drag the field to move it. With Select, click it to fill it in."));
    ui.add_space(10.0);
    if ui.button(RichText::new("Delete field").color(p.danger)).clicked() {
        ed.delete_selection();
    }
}

fn object_props(ui: &mut Ui, ed: &mut Editor, page: usize, items: Vec<usize>) {
    let p = pal(ui.ctx());
    let Some(f) = &ed.file else { return };
    let objs = f.objects(page);
    let chosen: Vec<pdf::Obj> = objs.iter().filter(|o| items.contains(&o.index)).cloned().collect();
    if chosen.is_empty() {
        ed.sel = Sel::None;
        return;
    }
    let rev = f.revs[page];
    // Bounds in user space.
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for o in &chosen {
        for &(x, y) in &o.quad {
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
    }
    let title = if chosen.len() > 1 {
        format!("{} ITEMS", chosen.len())
    } else {
        match chosen[0].kind {
            Kind::Text => "TEXT",
            Kind::Path => "SHAPE",
            Kind::Image => "IMAGE",
            Kind::Shading => "SHADING",
            Kind::Group => "GROUP",
        }
        .to_string()
    };
    theme::heading(ui, &title);
    ui.label(theme::secondary(ui, format!("Page {}", page + 1)));

    // Position and size, in points from the page's bottom left. Applied when a drag or edit ends.
    theme::heading(ui, "POSITION AND SIZE");
    let mut v = [b[0], b[1], b[2] - b[0], b[3] - b[1]];
    let mut done = None;
    egui::Grid::new(("geom", page, rev)).num_columns(4).spacing(vec2(6.0, 6.0)).show(ui, |ui| {
        for (k, label) in ["X", "Y", "W", "H"].iter().enumerate() {
            ui.label(theme::secondary(ui, *label));
            let min = if k >= 2 { 1.0 } else { -100000.0 };
            if let Some(n) = settled_number(ui, egui::Id::new(("geom", k, page, rev)), v[k], 0.5, min..=100000.0, "") {
                v[k] = n;
                done = Some(());
            }
            if k == 1 {
                ui.end_row();
            }
        }
        ui.end_row();
    });
    if done.is_some() {
        ed.reshape_selection(b, [v[0], v[1], v[0] + v[2], v[1] + v[3]]);
        return;
    }

    if chosen.len() == 1 {
        let o = chosen[0].clone();
        let Some(f) = &ed.file else { return };
        let Some(pg) = f.doc.page(page) else { return };
        match o.kind {
            Kind::Text => {
                let text = pg.text(o.index);
                let font = pg.font(o.index);
                let size = pg.font_size(o.index);
                let m = pg.matrix(o.index);
                let color = pg.fill_color(o.index).unwrap_or([0, 0, 0, 255]);
                drop(pg);
                theme::heading(ui, "TEXT");
                let id = egui::Id::new(("obj-text", page, o.index, rev));
                let mut buf = temp_text(ui, id, || text.clone());
                let resp = ui.add(egui::TextEdit::multiline(&mut buf).desired_rows(2).desired_width(f32::INFINITY));
                set_temp_text(ui, id, buf.clone());
                if resp.lost_focus() && buf.trim_end() != text.trim_end() {
                    ed.set_object_text(page, o.index, &buf);
                    return;
                }
                if let Some(fi) = font {
                    theme::heading(ui, "FONT");
                    ui.label(fi.clean_name());
                    ui.label(theme::secondary(ui, if fi.embedded { "Embedded in the file" } else { "Not embedded" }));
                }
                let scale = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
                let shown = size * scale;
                let mut new_size = None;
                ui.horizontal(|ui| {
                    ui.label("Size");
                    new_size = settled_number(ui, egui::Id::new(("text-size", page, o.index, rev)), shown, 0.1, 1.0..=400.0, " pt");
                });
                if let Some(s) = new_size {
                    ed.set_text_size(page, o.index, shown, s);
                    return;
                }
                theme::heading(ui, "COLOUR");
                let mut c = color;
                if color_row(ui, &mut c) {
                    ed.edit_object(page, o.index, Edit::Fill([c[0], c[1], c[2]]));
                    return;
                }
            }
            Kind::Path => {
                let (filled, stroked) = pg.path_mode(o.index);
                let fill = pg.fill_color(o.index).unwrap_or([0, 0, 0, 255]);
                let stroke = pg.stroke_color(o.index).unwrap_or([0, 0, 0, 255]);
                let width = pg.stroke_width(o.index);
                drop(pg);
                theme::heading(ui, "OUTLINE");
                let mut on = stroked;
                if ui.switch(&mut on, "Draw the outline").changed() {
                    ed.edit_object(page, o.index, Edit::Paint { fill: filled, stroke: on });
                    return;
                }
                if stroked {
                    let mut c = stroke;
                    if color_row(ui, &mut c) {
                        ed.edit_object(page, o.index, Edit::Stroke([c[0], c[1], c[2]]));
                        return;
                    }
                    let mut new_width = None;
                    ui.horizontal(|ui| {
                        ui.label("Width");
                        new_width = settled_number(ui, egui::Id::new(("stroke-width", page, o.index, rev)), width, 0.05, 0.0..=100.0, " pt");
                    });
                    if let Some(w) = new_width {
                        ed.edit_object(page, o.index, Edit::Width(w as f64));
                        return;
                    }
                }
                theme::heading(ui, "FILL");
                let mut on = filled;
                if ui.switch(&mut on, "Fill the shape").changed() {
                    ed.edit_object(page, o.index, Edit::Paint { fill: on, stroke: stroked });
                    return;
                }
                if filled {
                    let mut c = fill;
                    if color_row(ui, &mut c) {
                        ed.edit_object(page, o.index, Edit::Fill([c[0], c[1], c[2]]));
                        return;
                    }
                }
            }
            Kind::Image => {
                drop(pg);
                ui.add_space(4.0);
                if ui.button("Replace image…").clicked() {
                    ed.replace_image(page, o.index);
                    return;
                }
            }
            Kind::Group => {
                drop(pg);
                ui.label(theme::secondary(ui, "A group of items stored together in the file. It moves and resizes as one."));
                if ui.button("Ungroup to edit its parts").clicked() {
                    if let Some(parts) = ed.ungroup(page, o.index) {
                        ed.sel = Sel::Objects { page, items: parts };
                    }
                    return;
                }
            }
            Kind::Shading => {
                drop(pg);
            }
        }
    }

    theme::heading(ui, "ARRANGE");
    ui.horizontal_wrapped(|ui| {
        if ui.button("Bring to front").clicked() {
            ed.restack(true);
        }
        if ui.button("Send to back").clicked() {
            ed.restack(false);
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.button("↺ 90°").on_hover_text("Rotate left").clicked() {
            ed.rotate_selection_quarter(1);
        }
        if ui.button("↻ 90°").on_hover_text("Rotate right").clicked() {
            ed.rotate_selection_quarter(-1);
        }
        if ui.button("Duplicate").clicked() {
            ed.duplicate_selection();
        }
    });
    ui.add_space(10.0);
    if ui.button(RichText::new("Delete").color(p.danger)).clicked() {
        ed.delete_selection();
    }
}

/// A number field that reports a new value only once a drag or typed edit is finished, so each
/// change rewrites the page once rather than on every frame of a drag.
fn settled_number(ui: &mut Ui, id: egui::Id, current: f32, speed: f64, range: std::ops::RangeInclusive<f32>, suffix: &str) -> Option<f32> {
    let mut v = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(current);
    let r = ui.add(egui::DragValue::new(&mut v).speed(speed).range(range).max_decimals(2).suffix(suffix));
    if r.drag_stopped() || r.lost_focus() {
        ui.data_mut(|d| d.remove::<f32>(id));
        return ((v - current).abs() > 1e-4).then_some(v);
    }
    if r.dragged() || r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, v));
        return None;
    }
    ui.data_mut(|d| d.remove::<f32>(id));
    (r.changed() && (v - current).abs() > 1e-4).then_some(v)
}

// ── Comments ──

/// While a long document's annotations are still being read: say so and keep going next frame.
fn still_reading(ui: &mut Ui, f: &crate::editor::File, what: &str) -> bool {
    let left = f.preload_annots(40);
    if left > 0 {
        ui.label(theme::secondary(ui, format!("Reading {what}… {} to go", plural(left, "page", "pages"))));
        ui.ctx().request_repaint();
    }
    left > 0
}

pub fn comments_ui(ui: &mut Ui, ed: &mut Editor) {
    let Some(f) = &ed.file else { return no_document(ui) };
    if still_reading(ui, f, "comments") {
        return;
    }
    let p = pal(ui.ctx());
    let n = f.page_count();
    // Top-level comments and marks, each with its replies and latest review state.
    let mut rows = Vec::new();
    for page in 0..n {
        let annots = f.annots(page);
        for a in annots.iter().filter(|a| !matches!(a.subtype, pdf::ANNOT_WIDGET | pdf::ANNOT_POPUP | pdf::ANNOT_LINK) && a.reply_to.is_none()) {
            let mut replies: Vec<pdf::Annot> = annots.iter().filter(|r| r.reply_to == Some(a.index) && r.state.is_none()).cloned().collect();
            replies.sort_by(|x, y| x.modified.cmp(&y.modified));
            let resolved = annots
                .iter()
                .filter(|r| r.reply_to == Some(a.index) && r.state.is_some())
                .max_by(|x, y| x.modified.cmp(&y.modified).then(x.index.cmp(&y.index)))
                .is_some_and(|r| matches!(r.state.as_deref(), Some("Completed" | "Accepted")));
            rows.push((page, a.clone(), replies, resolved));
        }
    }
    let editing = ed.editing;
    let open = rows.iter().filter(|r| !r.3).count();
    ui.label(theme::secondary(
        ui,
        if rows.is_empty() {
            "No comments or marks yet. Use the Review tools to add them.".to_string()
        } else {
            format!("{}, {} open", plural(rows.len(), "comment or mark", "comments and marks"), open)
        },
    ));
    let show_resolved_id = egui::Id::new("comments-show-resolved");
    let mut show_resolved = ui.data(|d| d.get_temp::<bool>(show_resolved_id)).unwrap_or(true);
    if rows.iter().any(|r| r.3) && ui.checkbox(&mut show_resolved, "Show resolved").changed() {
        ui.data_mut(|d| d.insert_temp(show_resolved_id, show_resolved));
    }
    let mut actions: Vec<CommentAction> = Vec::new();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (page, a, replies, resolved) in rows {
            if resolved && !show_resolved {
                continue;
            }
            let selected = ed.sel == Sel::Annot { page, index: a.index };
            let frame = theme::inner_card(ui).stroke(Stroke::new(1.0, if selected { p.accent } else { p.inner_card_stroke }));
            let resp = frame
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let head = ui.horizontal(|ui| {
                        if let Some(c) = a.color {
                            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.5, Color32::from_rgb(c[0], c[1], c[2]));
                        }
                        let kind = match (a.subtype, a.intent.as_str()) {
                            (pdf::ANNOT_LINE, "LineDimension") => "Measurement",
                            (pdf::ANNOT_LINE, _) => "Arrow",
                            (pdf::ANNOT_FREETEXT, "FreeTextCallout") => "Callout",
                            _ => a.kind_name(),
                        };
                        ui.label(theme::semibold(kind));
                        ui.label(theme::secondary(ui, format!("Page {}", page + 1)));
                        if resolved {
                            ui.label(RichText::new("Resolved").color(p.accent_text).size(11.0));
                        }
                    });
                    // Only the header and text select the comment; the card's buttons and reply box
                    // must keep their own clicks.
                    let mut picked = head.response.interact(Sense::click()).clicked();
                    let byline = [a.author.clone(), annots::show_date(&a.modified)].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
                    if !byline.is_empty() {
                        ui.label(theme::secondary(ui, byline).size(11.0));
                    }
                    if !a.contents.is_empty() {
                        picked |= ui.add(egui::Label::new(&a.contents).sense(Sense::click())).clicked();
                    }
                    for r in &replies {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.add_space(10.0);
                            ui.vertical(|ui| {
                                let who = [r.author.clone(), annots::show_date(&r.modified)].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
                                ui.label(theme::secondary(ui, who).size(11.0));
                                ui.label(&r.contents);
                            });
                        });
                    }
                    if editing {
                        let id = egui::Id::new(("reply", page, a.index));
                        let mut draft: Option<String> = ui.data(|d| d.get_temp(id));
                        let mut close = false;
                        let mut start = false;
                        match &mut draft {
                            Some(text) => {
                                let resp = ui.add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(f32::INFINITY).hint_text("Write a reply"));
                                if resp.gained_focus() || ui.data(|d| d.get_temp::<bool>(id.with("focus"))).unwrap_or(false) {
                                    resp.request_focus();
                                    ui.data_mut(|d| d.remove::<bool>(id.with("focus")));
                                }
                                let focused = resp.has_focus();
                                let send = focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Enter));
                                ui.horizontal(|ui| {
                                    if (ui.add_enabled(!text.trim().is_empty(), egui::Button::new("Reply")).clicked() || send) && !text.trim().is_empty() {
                                        actions.push(CommentAction::Reply(page, a.index, text.trim().to_string()));
                                        close = true;
                                    } else if ui.button("Cancel").clicked() {
                                        close = true;
                                    }
                                });
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    if ui.small_button("Reply").clicked() {
                                        start = true;
                                        ui.data_mut(|d| d.insert_temp(id.with("focus"), true));
                                    }
                                    if ui.small_button(if resolved { "Reopen" } else { "Resolve" }).clicked() {
                                        actions.push(CommentAction::Resolve(page, a.index, !resolved));
                                    }
                                });
                            }
                        }
                        if close {
                            draft = None;
                        }
                        if start {
                            draft = Some(String::new());
                        }
                        ui.data_mut(|d| match draft {
                            Some(t) => {
                                d.insert_temp(id, t);
                            }
                            None => {
                                d.remove::<String>(id);
                            }
                        });
                    }
                    picked
                })
                .inner;
            if resp {
                ed.sel = Sel::Annot { page, index: a.index };
                ed.goto = Some(page);
            }
            ui.add_space(2.0);
        }
    });
    for act in actions {
        match act {
            CommentAction::Reply(page, index, text) => ed.add_reply(page, index, &text),
            CommentAction::Resolve(page, index, done) => ed.set_resolved(page, index, done),
        }
    }
}

enum CommentAction {
    Reply(usize, usize, String),
    Resolve(usize, usize, bool),
}

// ── Form ──

pub fn form_ui(ui: &mut Ui, ed: &mut Editor) {
    let Some(f) = &ed.file else { return no_document(ui) };
    if f.doc.has_form() && still_reading(ui, f, "form fields") {
        return;
    }
    if !f.doc.has_form() {
        ui.label(theme::secondary(ui, "This document has no form fields. Dockit can look for the blanks, boxes and lines where they belong, or you can draw them with the Form field tool (F)."));
        if ui.button("Find form fields").clicked() {
            if !ed.editing {
                ed.set_editing(true);
            }
            if ed.editing {
                ed.detect_fields();
            }
        }
        return;
    }
    let missing = ed.empty_required();
    if !missing.is_empty() {
        ui.label(RichText::new(format!("{} still empty: {}", plural(missing.len(), "required field is", "required fields are"), missing.join(", "))).color(pal(ui.ctx()).warning).size(12.0));
    }
    let f = ed.file.as_ref().unwrap();
    let n = f.page_count();
    let mut fields = Vec::new();
    for page in 0..n {
        for a in f.annots(page).iter() {
            if let Some(field) = &a.field {
                fields.push((page, a.index, a.rect, field.clone()));
            }
        }
    }
    ui.label(theme::secondary(ui, plural(fields.len(), "field", "fields")));
    let mut last_page = usize::MAX;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        theme::panel_body(ui, |ui| {
            for (page, index, rect, field) in fields {
                if page != last_page {
                    theme::heading(ui, &format!("PAGE {}", page + 1));
                    last_page = page;
                }
                let name = if field.name.is_empty() { field.kind_name().to_string() } else { field.name.clone() };
                let rev = ed.file.as_ref().map_or(0, |f| f.revs[page]);
                ui.add_enabled_ui(!field.read_only(), |ui| match field.kind {
                    pdf::FIELD_TEXT => {
                        ui.label(theme::secondary(ui, &name));
                        let id = egui::Id::new(("form-text", page, index, rev));
                        let mut buf = temp_text(ui, id, || field.value.clone());
                        let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(f32::INFINITY));
                        set_temp_text(ui, id, buf.clone());
                        if (resp.lost_focus() || resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter))) && buf != field.value {
                            ed.set_field_text(page, index, &buf);
                        }
                        if resp.gained_focus() {
                            ed.goto_if_far(page);
                        }
                    }
                    pdf::FIELD_CHECKBOX => {
                        let mut on = field.checked;
                        if ui.checkbox(&mut on, &name).changed() {
                            ed.click_field(page, index);
                        }
                    }
                    pdf::FIELD_RADIO => {
                        if ui.radio(field.checked, format!("{name} (option)")).clicked() && !field.checked {
                            ed.click_field(page, index);
                        }
                    }
                    pdf::FIELD_COMBO | pdf::FIELD_LIST => {
                        ui.label(theme::secondary(ui, &name));
                        let current = field.options.iter().find(|o| o.1).map(|o| o.0.clone()).unwrap_or_else(|| field.value.clone());
                        egui::ComboBox::from_id_salt(("form-combo", page, index)).selected_text(current).width(ui.available_width() - 8.0).show_ui(ui, |ui| {
                            for (k, (label, on)) in field.options.iter().enumerate() {
                                if ui.selectable_label(*on, label).clicked() {
                                    ed.select_option(page, index, k);
                                }
                            }
                        });
                    }
                    pdf::FIELD_SIGNATURE => {
                        ui.label(theme::secondary(ui, &name));
                        if field.value.is_empty() {
                            ui.horizontal(|ui| {
                                if ui.button("Sign here").clicked() {
                                    ed.goto = Some(page);
                                    ed.place_signature(page, ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0));
                                }
                                if ui.button("Sign digitally…").clicked() {
                                    ed.request = Some(crate::editor::Request::SignDigitally(Some((page, index))));
                                }
                            });
                        } else {
                            ui.label("Signed");
                        }
                    }
                    _ => {
                        ui.label(theme::secondary(ui, format!("{name} ({})", field.kind_name().to_lowercase())));
                    }
                });
                ui.add_space(2.0);
            }
        });
    });
}

impl Editor {
    fn goto_if_far(&mut self, page: usize) {
        if page != self.current {
            self.goto = Some(page);
        }
    }
}

/// "Pages 1-3, 5" for a page list, used in messages.
#[allow(dead_code)]
pub fn describe_pages(pages: &[usize]) -> String {
    format!("{} {}", if pages.len() == 1 { "Page" } else { "Pages" }, page_ranges(pages))
}
