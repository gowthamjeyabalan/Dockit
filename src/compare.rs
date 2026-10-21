//! Comparing the open document with another version of it, page by page: the words that were
//! removed or added (a word-level diff of each page's text) and the areas that look different
//! (a pixel comparison of the two pages drawn the same size).

use crate::pdf::Doc;
use crate::theme::{self, pal};
use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Same,
    Removed,
    Added,
}

/// Runs of words, each the same, removed (only in the old version) or added (only in the new).
pub fn diff_words<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(Op, Vec<&'a str>)> {
    let script = myers(a, b);
    let mut out: Vec<(Op, Vec<&str>)> = Vec::new();
    for (op, w) in script {
        match out.last_mut() {
            Some((last, words)) if *last == op => words.push(w),
            _ => out.push((op, vec![w])),
        }
    }
    out
}

/// Myers' shortest edit script, as one operation per word. Falls back to "all removed, all added"
/// when the two are almost entirely different (to keep the work bounded).
fn myers<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(Op, &'a str)> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let limit = max.min(4000);
    let off = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'outer: for d in 0..=limit as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let i = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) { v[i + 1] } else { v[i - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }
    let Some(dmax) = found else {
        return a.iter().map(|w| (Op::Removed, *w)).chain(b.iter().map(|w| (Op::Added, *w))).collect();
    };
    // Walk back through the saved frontiers.
    let mut out = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=dmax).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let i = (k + off) as usize;
        let prev_k = if k == -d || (k != d && v[i - 1] < v[i + 1]) { k + 1 } else { k - 1 };
        let px = v[(prev_k + off) as usize];
        let py = px - prev_k;
        while x > px && y > py {
            x -= 1;
            y -= 1;
            out.push((Op::Same, a[x as usize]));
        }
        if x == px {
            y -= 1;
            out.push((Op::Added, b[y as usize]));
        } else {
            x -= 1;
            out.push((Op::Removed, a[x as usize]));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        out.push((Op::Same, a[x as usize]));
    }
    out.reverse();
    out
}

/// One page pair: page `a` of the open document and page `b` of the other version.
pub struct PageCompare {
    pub a: Option<usize>,
    pub b: Option<usize>,
    pub runs: Vec<(Op, String)>,
    pub removed: usize,
    pub added: usize,
    /// Worked out when the page is first shown.
    visual: Option<Visual>,
}

struct Visual {
    tex_a: Option<egui::TextureHandle>,
    tex_b: Option<egui::TextureHandle>,
    /// Changed areas, as fractions of the page (0..1, y down).
    regions: Vec<Rect>,
}

pub struct Compare {
    pub other_path: PathBuf,
    other: Doc,
    pub pages: Vec<PageCompare>,
    pub current: usize,
}

fn words(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

impl Compare {
    pub fn new(doc: &Doc, other_path: PathBuf) -> Result<Compare, String> {
        let bytes = std::fs::read(&other_path).map_err(|e| format!("{} could not be read ({e}).", other_path.display()))?;
        let other = Doc::open(bytes, None).map_err(|e| match e {
            crate::pdf::OpenError::Password => "That file is password protected. Open it in Dockit and save it without protection to compare it.".to_string(),
            crate::pdf::OpenError::Failed(m) => format!("That file could not be opened. {m}"),
        })?;
        let (na, nb) = (doc.page_count(), other.page_count());
        let mut pages = Vec::new();
        for i in 0..na.max(nb) {
            let ta = (i < na).then(|| doc.page(i).map(|p| p.page_text()).unwrap_or_default()).unwrap_or_default();
            let tb = (i < nb).then(|| other.page(i).map(|p| p.page_text()).unwrap_or_default()).unwrap_or_default();
            let runs: Vec<(Op, String)> = diff_words(&words(&ta), &words(&tb)).into_iter().map(|(op, w)| (op, w.join(" "))).collect();
            let count = |op: Op| runs.iter().filter(|r| r.0 == op).map(|r| r.1.split_whitespace().count()).sum();
            pages.push(PageCompare { a: (i < na).then_some(i), b: (i < nb).then_some(i), removed: count(Op::Removed), added: count(Op::Added), runs, visual: None });
        }
        Ok(Compare { other_path, other, pages, current: 0 })
    }

    fn changed_pages(&self) -> usize {
        self.pages.iter().filter(|p| p.removed + p.added > 0 || p.a.is_none() || p.b.is_none()).count()
    }
}

/// Pixel comparison of two pages drawn at the same size: boxes around the areas that differ.
fn regions(a: &egui::ColorImage, b: &egui::ColorImage) -> Vec<Rect> {
    if a.size != b.size {
        return vec![Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))];
    }
    let [w, h] = a.size;
    const CELL: usize = 6;
    let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut hot = vec![false; cw * ch];
    for y in 0..h {
        for x in 0..w {
            let (p, q) = (a.pixels[y * w + x], b.pixels[y * w + x]);
            let d = p.r().abs_diff(q.r()) as u32 + p.g().abs_diff(q.g()) as u32 + p.b().abs_diff(q.b()) as u32;
            if d > 90 {
                hot[(y / CELL) * cw + x / CELL] = true;
            }
        }
    }
    // Group touching cells (with a one-cell gap allowed) into boxes.
    let mut seen = vec![false; cw * ch];
    let mut out = Vec::new();
    for start in 0..cw * ch {
        if !hot[start] || seen[start] {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(c) = stack.pop() {
            let (cx, cy) = (c % cw, c / cw);
            (x0, y0, x1, y1) = (x0.min(cx), y0.min(cy), x1.max(cx), y1.max(cy));
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= cw as i32 || ny >= ch as i32 {
                        continue;
                    }
                    let k = ny as usize * cw + nx as usize;
                    if hot[k] && !seen[k] {
                        seen[k] = true;
                        stack.push(k);
                    }
                }
            }
        }
        let f = |v: usize, total: usize| (v * CELL) as f32 / total as f32;
        out.push(Rect::from_min_max(pos2(f(x0, w), f(y0, h)), pos2(f(x1 + 1, w).min(1.0), f(y1 + 1, h).min(1.0))));
    }
    out
}

fn render(doc: &Doc, page: usize, width: f32) -> Option<egui::ColorImage> {
    let (w, h) = doc.page_size(page);
    let scale = width / w.max(1.0);
    doc.page(page)?.render(scale, [0, 0, (w * scale).round() as i32, (h * scale).round() as i32])
}

/// The comparison window. Returns false when it is closed.
pub fn compare_window(ctx: &egui::Context, doc: Option<&Doc>, c: &mut Compare) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let changed = c.changed_pages();
    let other_name = c.other_path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    egui::Window::new("Compare")
        .open(&mut open)
        .default_size(vec2(980.0, 680.0))
        .collapsible(false)
        .resizable(true)
        .show(ctx, |ui| {
            ui.label(theme::secondary(
                ui,
                if changed == 0 { format!("No differences found with {other_name}.") } else { format!("{} from {other_name}.", crate::editor::plural(changed, "page differs", "pages differ")) },
            ));
            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                // Page list.
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    egui::ScrollArea::vertical().id_salt("cmp-pages").show(ui, |ui| {
                        for (k, pg) in c.pages.iter().enumerate() {
                            let label = match (pg.a, pg.b) {
                                (Some(i), Some(_)) => format!("Page {}", i + 1),
                                (Some(i), None) => format!("Page {} (removed)", i + 1),
                                (None, Some(i)) => format!("Page {} (added)", i + 1),
                                _ => continue,
                            };
                            let detail = if pg.removed + pg.added == 0 { "same text".to_string() } else { format!("−{} +{}", pg.removed, pg.added) };
                            let text = RichText::new(format!("{label}\n{detail}")).color(if pg.removed + pg.added > 0 || pg.a.is_none() || pg.b.is_none() { p.text } else { p.text_secondary });
                            if ui.selectable_label(c.current == k, text).clicked() {
                                c.current = k;
                            }
                        }
                    });
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(pg) = c.pages.get_mut(c.current) else { return };
                    if pg.visual.is_none() {
                        let width = 420.0 * ctx.pixels_per_point();
                        let ia = pg.a.and_then(|i| doc.and_then(|d| render(d, i, width)));
                        let ib = pg.b.and_then(|i| render(&c.other, i, width));
                        let regions = match (&ia, &ib) {
                            (Some(a), Some(b)) => regions(a, b),
                            _ => Vec::new(),
                        };
                        let tex = |img: Option<egui::ColorImage>, name: &str| img.map(|i| ctx.load_texture(name, i, egui::TextureOptions::LINEAR));
                        pg.visual = Some(Visual { tex_a: tex(ia, "cmp-a"), tex_b: tex(ib, "cmp-b"), regions });
                    }
                    let v = pg.visual.as_ref().unwrap();
                    ui.horizontal_top(|ui| {
                        for (label, tex) in [("This document", &v.tex_a), (other_name.as_str(), &v.tex_b)] {
                            ui.vertical(|ui| {
                                ui.label(theme::semibold(label));
                                match tex {
                                    Some(t) => {
                                        let size = t.size_vec2() / ctx.pixels_per_point();
                                        let (r, _) = ui.allocate_exact_size(size, Sense::hover());
                                        ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                        for g in &v.regions {
                                            let rr = Rect::from_min_max(r.min + g.min.to_vec2() * r.size(), r.min + g.max.to_vec2() * r.size()).expand(2.0);
                                            ui.painter().rect_filled(rr, 2.0, p.danger.gamma_multiply(0.12));
                                            ui.painter().rect_stroke(rr, 2.0, Stroke::new(1.5, p.danger), StrokeKind::Outside);
                                        }
                                    }
                                    None => {
                                        let (r, _) = ui.allocate_exact_size(vec2(420.0, 120.0), Sense::hover());
                                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "No such page", egui::FontId::proportional(13.0), p.text_secondary);
                                    }
                                }
                            });
                        }
                    });
                    ui.add_space(6.0);
                    theme::heading(ui, "TEXT CHANGES");
                    egui::ScrollArea::vertical().id_salt("cmp-text").max_height(160.0).show(ui, |ui| text_changes(ui, &pg.runs));
                });
            });
        });
    open
}

/// The page's words with removals struck through in red and additions in green; long unchanged
/// stretches are shortened to their ends.
fn text_changes(ui: &mut Ui, runs: &[(Op, String)]) {
    let p = pal(ui.ctx());
    if runs.iter().all(|r| r.0 == Op::Same) {
        ui.label(theme::secondary(ui, "The text on this page is the same."));
        return;
    }
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::proportional(13.0);
    let green = Color32::from_rgb(52, 168, 83);
    for (k, (op, text)) in runs.iter().enumerate() {
        let shown = match op {
            Op::Same => {
                let w: Vec<&str> = text.split_whitespace().collect();
                let first = k == 0;
                let last = k + 1 == runs.len();
                if w.len() > 12 {
                    let head = if first { String::new() } else { w[..5].join(" ") };
                    let tail = if last { String::new() } else { w[w.len() - 5..].join(" ") };
                    format!("{head} … {tail}")
                } else {
                    text.clone()
                }
            }
            _ => text.clone(),
        };
        let fmt = match op {
            Op::Same => egui::TextFormat { font_id: font.clone(), color: p.text_secondary, ..Default::default() },
            Op::Removed => egui::TextFormat { font_id: font.clone(), color: p.danger, strikethrough: Stroke::new(1.0, p.danger), ..Default::default() },
            Op::Added => egui::TextFormat { font_id: font.clone(), color: green, background: green.gamma_multiply(0.12), ..Default::default() },
        };
        job.append(&format!("{} ", shown.trim()), 0.0, fmt);
    }
    job.wrap.max_width = ui.available_width();
    ui.label(job);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_diff() {
        let a = words("the quick brown fox jumps");
        let b = words("the slow brown fox leaps high");
        let d = diff_words(&a, &b);
        let removed: Vec<&str> = d.iter().filter(|r| r.0 == Op::Removed).flat_map(|r| r.1.clone()).collect();
        let added: Vec<&str> = d.iter().filter(|r| r.0 == Op::Added).flat_map(|r| r.1.clone()).collect();
        assert_eq!(removed, vec!["quick", "jumps"]);
        assert_eq!(added, vec!["slow", "leaps", "high"]);
        assert!(diff_words(&a, &a).iter().all(|r| r.0 == Op::Same));
        assert_eq!(diff_words(&[], &b).len(), 1);
    }
}
