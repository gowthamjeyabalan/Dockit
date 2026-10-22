//! Find (Ctrl+F): a bar over the document that searches the text of every page, a few pages per
//! frame so long documents stay responsive, highlights every match and steps through them.

use crate::editor::Editor;
use crate::theme::pal;
use egui::{Key, Modifiers, RichText, pos2, vec2};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Find {
    pub query: String,
    pub match_case: bool,
    pub whole_word: bool,
    /// Matches so far, in page order: (page, the rects it covers).
    pub results: Vec<(usize, Vec<[f32; 4]>)>,
    /// The next page to search.
    scanned: usize,
    /// The query the results are for.
    searched: String,
    flags: (bool, bool),
    pub current: Option<usize>,
    pub focus: bool,
}

impl Find {
    pub fn open() -> Self {
        Find { focus: true, ..Default::default() }
    }

    fn done(&self, pages: usize) -> bool {
        self.scanned >= pages
    }
}

impl Editor {
    /// Search more pages for the find bar's query, for up to a few milliseconds.
    pub fn find_step(&mut self) {
        let Some(n) = self.file.as_ref().map(|f| f.page_count()) else { return };
        let Some(fd) = &mut self.find else { return };
        if fd.query != fd.searched || (fd.match_case, fd.whole_word) != fd.flags {
            fd.results.clear();
            fd.scanned = 0;
            fd.current = None;
            fd.searched = fd.query.clone();
            fd.flags = (fd.match_case, fd.whole_word);
        }
        if fd.query.trim().is_empty() || fd.done(n) {
            return;
        }
        let start = Instant::now();
        let f = self.file.as_ref().unwrap();
        while fd.scanned < n && start.elapsed() < Duration::from_millis(8) {
            let page = fd.scanned;
            if let Some(p) = f.doc.page(page) {
                for rects in p.find(&fd.query, fd.match_case, fd.whole_word) {
                    fd.results.push((page, rects));
                }
            }
            fd.scanned += 1;
        }
        // The first match from where the reader is.
        if fd.current.is_none() && !fd.results.is_empty() {
            let here = self.current;
            if let Some(k) = fd.results.iter().position(|r| r.0 >= here).or(if fd.done(n) { Some(0) } else { None }) {
                fd.current = Some(k);
                let (page, rects) = fd.results[k].clone();
                self.reveal = Some((page, rects[0]));
            }
        }
    }

    /// Go to the next (or previous) match.
    pub fn find_next(&mut self, back: bool) {
        let Some(fd) = &mut self.find else { return };
        if fd.results.is_empty() {
            return;
        }
        let len = fd.results.len();
        let k = match fd.current {
            Some(c) if back => (c + len - 1) % len,
            Some(c) => (c + 1) % len,
            None => 0,
        };
        fd.current = Some(k);
        let (page, rects) = fd.results[k].clone();
        self.reveal = Some((page, rects[0]));
    }
}

/// The find bar, over the top right of the document. Returns false when it is closed.
pub fn find_bar(ctx: &egui::Context, ed: &mut Editor) -> bool {
    let Some(rect) = Some(ed.view.canvas_rect).filter(|r| r.is_positive()) else { return true };
    let p = pal(ctx);
    let pages = ed.file.as_ref().map_or(0, |f| f.page_count());
    ed.find_step();
    let Some(fd) = &mut ed.find else { return false };
    let mut close = false;
    let mut step: Option<bool> = None;
    let w = 420.0f32.min(rect.width() - 24.0);
    egui::Area::new(egui::Id::new("find-bar")).order(egui::Order::Foreground).fixed_pos(pos2(rect.right() - w - 18.0, rect.top() + 10.0)).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(w);
            ui.horizontal(|ui| {
                // Enter goes to the next match, Shift+Enter to the previous, Esc closes.
                let (enter, shift_enter, esc) = ui.input_mut(|i| {
                    (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::SHIFT, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape))
                });
                let edit = ui.add(egui::TextEdit::singleline(&mut fd.query).hint_text("Find in document").desired_width(w - 250.0));
                if fd.focus {
                    edit.request_focus();
                    fd.focus = false;
                }
                if edit.has_focus() || edit.lost_focus() {
                    if enter {
                        step = Some(false);
                        edit.request_focus();
                    }
                    if shift_enter {
                        step = Some(true);
                        edit.request_focus();
                    }
                }
                if esc {
                    close = true;
                }
                let count = if fd.query.trim().is_empty() {
                    String::new()
                } else if fd.results.is_empty() {
                    if fd.done(pages) { "None".into() } else { "…".into() }
                } else {
                    format!("{} of {}{}", fd.current.map_or(0, |c| c + 1), fd.results.len(), if fd.done(pages) { "" } else { "+" })
                };
                ui.label(RichText::new(count).size(12.0).color(if fd.results.is_empty() && fd.done(pages) && !fd.query.trim().is_empty() { p.danger } else { p.text_secondary }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("×").frame(false)).on_hover_text("Close (Esc)").clicked() {
                        close = true;
                    }
                    if ui.add_enabled(!fd.results.is_empty(), egui::Button::new("↓").min_size(vec2(24.0, 0.0))).on_hover_text("Next (Enter)").clicked() {
                        step = Some(false);
                    }
                    if ui.add_enabled(!fd.results.is_empty(), egui::Button::new("↑").min_size(vec2(24.0, 0.0))).on_hover_text("Previous (Shift+Enter)").clicked() {
                        step = Some(true);
                    }
                    ui.add(egui::Button::selectable(fd.whole_word, "W").min_size(vec2(22.0, 0.0))).on_hover_text("Whole words").clicked().then(|| fd.whole_word = !fd.whole_word);
                    ui.add(egui::Button::selectable(fd.match_case, "Aa").min_size(vec2(26.0, 0.0))).on_hover_text("Match case").clicked().then(|| fd.match_case = !fd.match_case);
                });
            });
        });
    });
    let searching = !fd.query.trim().is_empty() && !fd.done(pages);
    if let Some(back) = step {
        ed.find_next(back);
    }
    if searching {
        ctx.request_repaint();
    }
    !close
}
