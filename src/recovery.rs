//! Autosave and crash recovery.
//!
//! Every half minute, each document with unsaved changes has its latest state written to
//! %LOCALAPPDATA%\Dockit\Recovery (in the background, encrypted the same way if it is protected).
//! Copies of documents that are saved or closed are removed, and all of them go when Dockit quits
//! normally. Copies left behind by a Dockit that is no longer running are offered back at start.

use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::{Editor, File};
use crate::theme::{self, pal};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const EVERY: Duration = Duration::from_secs(30);

pub fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Dockit").join("Recovery")
}

/// What is stored next to each recovered copy.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Meta {
    pub pid: u32,
    pub uid: u64,
    /// Where the document came from, if it had been saved.
    pub path: Option<PathBuf>,
    pub name: String,
    /// When the copy was written, as "21 Oct 2026, 14:05".
    pub when: String,
}

fn stem(pid: u32, uid: u64) -> String {
    format!("{pid}-{uid:016x}")
}

/// Write one document's copy now (on a background thread).
fn write(f: &File) {
    let Some(bytes) = f.current_state() else { return };
    let meta = Meta {
        pid: std::process::id(),
        uid: f.uid,
        path: f.path.clone(),
        name: f.name(),
        when: crate::annots::show_date(&crate::annots::pdf_date()),
    };
    let protection = match &f.security {
        crate::security::Security::Protect(p) => Some(p.clone()),
        _ => None,
    };
    let bytes = bytes.to_vec();
    std::thread::spawn(move || {
        let bytes = match protection {
            // A protected document is never written out unprotected.
            Some(p) => match crate::security::encrypt(&bytes, &p) {
                Ok(b) => b,
                Err(_) => return,
            },
            None => bytes,
        };
        let d = dir();
        if std::fs::create_dir_all(&d).is_err() {
            return;
        }
        let s = stem(meta.pid, meta.uid);
        let tmp = d.join(format!("{s}.tmp"));
        if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, d.join(format!("{s}.pdf"))).is_ok() {
            let _ = std::fs::write(d.join(format!("{s}.json")), serde_json::to_vec(&meta).unwrap_or_default());
        }
    });
}

fn remove(pid: u32, uid: u64) {
    let d = dir();
    let s = stem(pid, uid);
    let _ = std::fs::remove_file(d.join(format!("{s}.json")));
    let _ = std::fs::remove_file(d.join(format!("{s}.pdf")));
}

/// Keeps the copies of this Dockit's open documents up to date.
pub struct Autosave {
    last: Instant,
    /// Documents with a copy on disk, and the history state it holds.
    written: Vec<(u64, usize)>,
}

impl Default for Autosave {
    fn default() -> Self {
        Autosave { last: Instant::now(), written: Vec::new() }
    }
}

impl Autosave {
    /// Call every frame with all open editors.
    pub fn tick<'a>(&mut self, editors: impl Iterator<Item = &'a Editor>) {
        if self.last.elapsed() < EVERY {
            return;
        }
        self.last = Instant::now();
        let pid = std::process::id();
        let mut keep = Vec::new();
        for ed in editors {
            let Some(f) = &ed.file else { continue };
            if !ed.needs_save() {
                continue;
            }
            let state = f.state();
            if !self.written.iter().any(|&(u, s)| u == f.uid && s == state) {
                write(f);
            }
            keep.push((f.uid, state));
        }
        // Saved or closed documents need no copy any more.
        for (uid, _) in self.written.iter().filter(|(u, _)| !keep.iter().any(|(k, _)| k == u)) {
            remove(pid, *uid);
        }
        self.written = keep;
    }

    /// Dockit is closing normally: no copies are needed.
    pub fn clear(&mut self) {
        let pid = std::process::id();
        for (uid, _) in self.written.drain(..) {
            remove(pid, uid);
        }
    }
}

fn running(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let alive = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(h);
        alive
    }
    #[cfg(not(windows))]
    false
}

/// Copies left by a Dockit that did not close normally.
pub fn orphans() -> Vec<Meta> {
    let Ok(entries) = std::fs::read_dir(dir()) else { return Vec::new() };
    let mut out: Vec<Meta> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice::<Meta>(&std::fs::read(e.path()).ok()?).ok())
        .filter(|m| m.pid != std::process::id() && !running(m.pid) && pdf_of(m).exists())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn pdf_of(m: &Meta) -> PathBuf {
    dir().join(format!("{}.pdf", stem(m.pid, m.uid)))
}

impl Editor {
    /// Open a recovered copy in this (empty) editor as an unsaved version of its document.
    pub fn open_recovered(&mut self, m: &Meta) {
        let file = pdf_of(m);
        let Ok(bytes) = std::fs::read(&file) else {
            self.error = Some(format!("The recovered copy of {} could not be read.", m.name));
            return;
        };
        let shown = m.path.clone().unwrap_or_else(|| PathBuf::from(&m.name));
        self.open_bytes(&shown, bytes, None);
        if let Some(f) = &mut self.file {
            if m.path.is_none() {
                f.path = None;
            }
            f.mark_unsaved();
            self.say(format!("Recovered {} from {}. Save it to keep it.", m.name, m.when));
        }
        remove(m.pid, m.uid);
    }
}

pub fn discard(m: &Meta) {
    remove(m.pid, m.uid);
}

// ── the dialog shown at start ──

pub struct RecoveryDialog {
    pub items: Vec<Meta>,
}

pub enum RecoveryAction {
    Open(Vec<Meta>),
    Later,
}

pub fn recovery_window(ctx: &egui::Context, d: &mut RecoveryDialog) -> Option<RecoveryAction> {
    let p = pal(ctx);
    let mut action = None;
    let mut discard_ix = None;
    let mut open_ix = None;
    let (_, close) = modal(ctx, "recovery", 440.0, |ui| {
        title(ui, "Recover unsaved work");
        ui.label(theme::secondary(ui, "Dockit closed before these documents were saved. Their latest changes were kept."));
        ui.add_space(6.0);
        for (k, m) in d.items.iter().enumerate() {
            theme::inner_card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        // Leave room for the buttons.
                        ui.set_max_width(ui.available_width() - 140.0);
                        ui.label(theme::semibold(&m.name));
                        let from = m.path.as_ref().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_else(|| "Never saved".into());
                        ui.add(egui::Label::new(theme::secondary(ui, format!("{} · {from}", m.when)).size(11.0)).truncate()).on_hover_text(from);
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Open").clicked() {
                            open_ix = Some(k);
                        }
                        if ui.button(egui::RichText::new("Discard").color(p.danger)).clicked() {
                            discard_ix = Some(k);
                        }
                    });
                });
            });
            ui.add_space(2.0);
        }
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Open all", true) {
                action = Some(RecoveryAction::Open(d.items.clone()));
            }
            if secondary_button(ui, "Later") {
                action = Some(RecoveryAction::Later);
            }
        });
    });
    if let Some(k) = discard_ix {
        discard(&d.items.remove(k));
    }
    if let Some(k) = open_ix {
        let m = d.items.remove(k);
        return Some(RecoveryAction::Open(vec![m]));
    }
    if d.items.is_empty() && action.is_none() {
        return Some(RecoveryAction::Later);
    }
    if close && action.is_none() {
        return Some(RecoveryAction::Later);
    }
    action
}
