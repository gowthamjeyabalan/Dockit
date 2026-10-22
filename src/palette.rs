//! The command palette (Ctrl+K): type a few letters of any command and press Enter, or a page
//! number to go there.

use crate::commands::{Cmd, sections};
use crate::editor::Editor;
use crate::theme::{self, pal};
use egui::{CornerRadius, Key, Modifiers, Sense, Stroke, pos2, vec2};

#[derive(Clone, Default)]
pub struct Palette {
    pub query: String,
    pub index: usize,
    pub focus: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Found {
    Cmd(Cmd),
    Page(usize),
}

struct Row {
    found: Found,
    title: String,
    keys: String,
}

fn results(ed: &Editor, query: &str, enabled: &dyn Fn(Cmd) -> bool) -> Vec<Row> {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_owned).collect();
    let mut out = Vec::new();
    // A number goes to that page.
    if let (Ok(n), Some(f)) = (query.trim().parse::<usize>(), &ed.file)
        && n >= 1
        && n <= f.page_count()
    {
        out.push(Row { found: Found::Page(n - 1), title: format!("Go to page {n}"), keys: String::new() });
    }
    for (menu, cmds) in sections() {
        for c in cmds {
            if !enabled(c) {
                continue;
            }
            let hay = format!("{} {} {}", c.name(), c.keywords(), menu).to_lowercase();
            if words.iter().all(|w| hay.contains(w.as_str())) {
                out.push(Row { found: Found::Cmd(c), title: c.name(), keys: ed.keys.text(c) });
            }
        }
    }
    out
}

/// Draw the palette while it is open. Returns a command for the app to run.
pub fn show(ctx: &egui::Context, ed: &mut Editor, enabled: &dyn Fn(Cmd) -> bool) -> Option<Cmd> {
    let state = ed.palette.clone()?;
    let p = pal(ctx);
    let rows = results(ed, &state.query, enabled);
    let mut index = state.index.min(rows.len().saturating_sub(1));
    let mut picked = None;
    let mut close = false;
    ctx.input_mut(|i| {
        if i.consume_key(Modifiers::NONE, Key::ArrowDown) && !rows.is_empty() {
            index = (index + 1) % rows.len();
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowUp) && !rows.is_empty() {
            index = (index + rows.len() - 1) % rows.len();
        }
        if i.consume_key(Modifiers::NONE, Key::Enter) {
            picked = rows.get(index).map(|r| r.found);
        }
        if i.consume_key(Modifiers::NONE, Key::Escape) {
            close = true;
        }
    });
    let screen = ctx.content_rect();
    let w = 560.0f32.min(screen.width() - 32.0);
    let at = pos2(screen.center().x - w * 0.5, screen.top() + 70.0);
    let mut query = state.query.clone();
    let area = egui::Area::new(egui::Id::new("command-palette")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        egui::Frame::new()
            .fill(p.window_fill)
            .stroke(Stroke::new(1.0, p.window_stroke))
            .corner_radius(12)
            .inner_margin(egui::Margin::same(8))
            .shadow(ctx.global_style().visuals.popup_shadow)
            .show(ui, |ui| {
                ui.set_width(w);
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut query).hint_text("Type a command, or a page number").font(egui::FontId::proportional(15.0)).desired_width(f32::INFINITY),
                );
                if state.focus {
                    edit.request_focus();
                }
                ui.add_space(6.0);
                if rows.is_empty() {
                    ui.label(theme::secondary(ui, "Nothing matches"));
                }
                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                    for (k, r) in rows.iter().enumerate() {
                        let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                        if resp.hovered() {
                            index = k;
                        }
                        if k == index {
                            ui.painter().rect_filled(row, CornerRadius::same(6), p.bg_control_hover);
                            if state.index != index {
                                resp.scroll_to_me(None);
                            }
                        }
                        ui.painter().text(pos2(row.left() + 10.0, row.center().y), egui::Align2::LEFT_CENTER, &r.title, egui::FontId::proportional(13.5), p.text);
                        if !r.keys.is_empty() {
                            ui.painter().text(pos2(row.right() - 10.0, row.center().y), egui::Align2::RIGHT_CENTER, &r.keys, egui::FontId::proportional(12.0), p.text_secondary);
                        }
                        if resp.clicked() {
                            picked = Some(r.found);
                        }
                    }
                });
            });
    });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|q| !area.response.rect.contains(q)));
    if close || clicked_outside {
        ed.palette = None;
        return None;
    }
    if let Some(found) = picked {
        ed.palette = None;
        match found {
            Found::Cmd(c) => return Some(c),
            Found::Page(n) => ed.goto = Some(n),
        }
        return None;
    }
    if let Some(s) = ed.palette.as_mut() {
        if query != s.query {
            index = 0;
        }
        s.query = query;
        s.index = index;
        s.focus = false;
    }
    None
}
