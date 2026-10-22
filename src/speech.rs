//! Read aloud with Windows' own voices (SAPI): the selected text, or page after page from the one
//! being read, with pause, stop and speed. A small bar at the bottom of the document controls it.

use crate::editor::Editor;
use crate::theme::pal;
use egui::{RichText, pos2, vec2};
use windows::Win32::Media::Speech::{ISpVoice, SPF_ASYNC, SPF_IS_NOT_XML, SPF_PURGEBEFORESPEAK, SPRS_DONE, SPVOICESTATUS, SpVoice};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::HSTRING;

pub struct Reader {
    voice: ISpVoice,
    /// Reading pages one after another, from this one; None for a selection.
    page: Option<usize>,
    paused: bool,
    /// Speed, -10 (slow) to 10 (fast).
    rate: i32,
}

impl Reader {
    pub fn new() -> Result<Reader, String> {
        let voice: ISpVoice = unsafe { CoCreateInstance(&SpVoice, None, CLSCTX_ALL) }.map_err(|e| format!("Windows' voices are not available ({}).", e.message()))?;
        Ok(Reader { voice, page: None, paused: false, rate: 0 })
    }

    fn speak(&mut self, text: &str) {
        let flags = (SPF_ASYNC.0 | SPF_PURGEBEFORESPEAK.0 | SPF_IS_NOT_XML.0) as u32;
        unsafe {
            let _ = self.voice.SetRate(self.rate);
            let _ = self.voice.Speak(&HSTRING::from(text), flags, None);
        }
        self.paused = false;
    }

    fn done(&self) -> bool {
        let mut st = SPVOICESTATUS::default();
        unsafe { self.voice.GetStatus(&mut st, std::ptr::null_mut()) }.is_ok() && st.dwRunningState == SPRS_DONE.0 as u32
    }

    fn toggle_pause(&mut self) {
        unsafe {
            let _ = if self.paused { self.voice.Resume() } else { self.voice.Pause() };
        }
        self.paused = !self.paused;
    }

    pub fn stop(&mut self) {
        unsafe {
            if self.paused {
                let _ = self.voice.Resume();
            }
            // Speaking nothing with "purge" cuts off what is being said.
            let _ = self.voice.Speak(&HSTRING::from(""), (SPF_ASYNC.0 | SPF_PURGEBEFORESPEAK.0) as u32, None);
        }
    }

    /// Start reading: the selected text, or from the current page on.
    pub fn start(&mut self, ed: &Editor) -> bool {
        if let Some(t) = ed.selected_text() {
            self.page = None;
            self.speak(&t);
            return true;
        }
        let Some(f) = &ed.file else { return false };
        let page = ed.current;
        let text = f.doc.page(page).map(|p| p.page_text()).unwrap_or_default();
        self.page = Some(page);
        self.speak(&text);
        true
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The reading bar. Returns false when reading is over or stopped.
pub fn reader_bar(ctx: &egui::Context, ed: &mut Editor, r: &mut Reader) -> bool {
    let rect = ed.view.canvas_rect;
    if !rect.is_positive() {
        return true;
    }
    // A page finished: on to the next one, which the view follows.
    if !r.paused && r.done() {
        let n = ed.file.as_ref().map_or(0, |f| f.page_count());
        match r.page {
            Some(p) if p + 1 < n => {
                let text = ed.file.as_ref().and_then(|f| f.doc.page(p + 1)).map(|pg| pg.page_text()).unwrap_or_default();
                r.page = Some(p + 1);
                ed.goto = Some(p + 1);
                r.speak(&text);
            }
            _ => return false,
        }
    }
    let p = pal(ctx);
    let mut open = true;
    let w = 330.0;
    egui::Area::new(egui::Id::new("read-aloud")).order(egui::Order::Foreground).fixed_pos(pos2(rect.center().x - w / 2.0, rect.bottom() - 58.0)).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_width(w);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(if r.paused { "▶" } else { "⏸" }).min_size(vec2(28.0, 0.0))).on_hover_text(if r.paused { "Go on" } else { "Pause" }).clicked() {
                    r.toggle_pause();
                }
                if ui.add(egui::Button::new("■").min_size(vec2(28.0, 0.0))).on_hover_text("Stop").clicked() {
                    open = false;
                }
                let what = match r.page {
                    Some(pg) => format!("Reading page {}", pg + 1),
                    None => "Reading the selection".into(),
                };
                ui.label(RichText::new(what).size(12.5).color(p.text_secondary));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut rate = r.rate;
                    ui.spacing_mut().slider_width = 70.0;
                    if ui.add(egui::Slider::new(&mut rate, -6..=8).show_value(false)).on_hover_text("Speed").changed() {
                        r.rate = rate;
                        unsafe {
                            let _ = r.voice.SetRate(rate);
                        }
                    }
                    ui.label(RichText::new("Speed").size(12.0).color(p.text_secondary));
                });
            });
        });
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    open
}
