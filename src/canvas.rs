//! The document view: pages stacked in a scrolling column, drawn from PDFium renders, with the
//! selection, previews and every tool's mouse handling on top.

use crate::editor::{Editor, Request, Sel, Signature, Tool};
use crate::pdf::{self, Annot, Geom, Glyph, Kind, Obj, Seg};
use crate::theme::{self, pal};
use egui::{Color32, CursorIcon, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use std::collections::HashMap;

const MARGIN: f32 = 28.0;
const GAP: f32 = 18.0;
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 8.0;
/// Pages bigger than this (in pixels) are rendered only where they are visible.
const MAX_FULL_PIXELS: f32 = 14_000_000.0;
const HANDLE: f32 = 7.0;
pub const THUMB_W: f32 = 132.0;

struct PageTex {
    tex: egui::TextureHandle,
    scale: f32,
    region: [i32; 4],
    epoch: u64,
    rev: u64,
}

struct Thumb {
    tex: egui::TextureHandle,
    epoch: u64,
    rev: u64,
    used: f64,
}

enum Drag {
    Move { page: usize, from: Pos2, bbox: Rect },
    Resize { page: usize, handle: usize, bbox: Rect, keep_aspect: bool, from: Pos2 },
    Band { page: usize, from: Pos2 },
    Shape { page: usize, from: Pos2 },
    Markup { page: usize, from: (f32, f32) },
    Ink { page: usize, pts: Vec<(f32, f32)> },
    Image { page: usize, from: Pos2 },
    AnnotMove { page: usize, from: Pos2, rect: Rect },
    Pan,
}

pub struct View {
    pages: HashMap<usize, PageTex>,
    thumbs: HashMap<usize, Thumb>,
    zoom_changed: f64,
    offset: Vec2,
    pending_offset: Option<Vec2>,
    drag: Option<Drag>,
    /// Screen rect of the document area, for drop highlights.
    pub canvas_rect: Rect,
    /// An open drop-down field: page, annotation, position, and the frame it opened on.
    field_menu: Option<(usize, usize, Pos2, u64)>,
    frame: u64,
    viewport: Vec2,
}

impl Default for View {
    fn default() -> Self {
        View {
            pages: HashMap::new(),
            thumbs: HashMap::new(),
            zoom_changed: 0.0,
            offset: Vec2::ZERO,
            pending_offset: None,
            drag: None,
            canvas_rect: Rect::NOTHING,
            field_menu: None,
            frame: 0,
            viewport: vec2(800.0, 600.0),
        }
    }
}

/// Converts between screen positions and one page's user space.
#[derive(Clone, Copy)]
struct Map {
    r: Rect,
    g: Geom,
    zoom: f32,
}

impl Map {
    fn screen(&self, p: (f32, f32)) -> Pos2 {
        let (x, y) = self.g.to_view(p.0, p.1);
        self.r.min + vec2(x, y) * self.zoom
    }

    fn user(&self, p: Pos2) -> (f32, f32) {
        let v = (p - self.r.min) / self.zoom;
        self.g.to_user(v.x, v.y)
    }

    fn quad(&self, q: &[(f32, f32); 4]) -> [Pos2; 4] {
        [self.screen(q[0]), self.screen(q[1]), self.screen(q[2]), self.screen(q[3])]
    }

    /// Screen rect of a user-space rect (l, b, r, t).
    fn rect(&self, u: [f32; 4]) -> Rect {
        Rect::from_two_pos(self.screen((u[0], u[1])), self.screen((u[2], u[3])))
    }

    /// User-space rect (l, b, r, t) of a screen rect.
    fn user_rect(&self, s: Rect) -> [f32; 4] {
        let (a, b) = (self.user(s.min), self.user(s.max));
        [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]
    }
}

fn quad_bounds(q: &[Pos2; 4]) -> Rect {
    Rect::from_points(q)
}

fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t - p).length()
}

/// Inside the quad, or within `tol` of its edges (so thin lines can be picked).
fn hit_quad(p: Pos2, q: &[Pos2; 4], tol: f32) -> bool {
    let mut sign = 0.0f32;
    let mut inside = true;
    for k in 0..4 {
        let (a, b) = (q[k], q[(k + 1) % 4]);
        let c = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if c.abs() > 1e-6 {
            if sign == 0.0 {
                sign = c.signum();
            } else if c.signum() != sign {
                inside = false;
            }
        }
    }
    inside && sign != 0.0 || (0..4).any(|k| dist_to_segment(p, q[k], q[(k + 1) % 4]) <= tol)
}

fn color32(c: pdf::Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn handles(r: Rect) -> [Pos2; 8] {
    let c = r.center();
    [r.left_top(), pos2(c.x, r.top()), r.right_top(), pos2(r.right(), c.y), r.right_bottom(), pos2(c.x, r.bottom()), r.left_bottom(), pos2(r.left(), c.y)]
}

fn resize_cursor(h: usize) -> CursorIcon {
    match h {
        0 | 4 => CursorIcon::ResizeNwSe,
        2 | 6 => CursorIcon::ResizeNeSw,
        1 | 5 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeHorizontal,
    }
}

/// The rect after dragging handle `h` of `r` by `d`.
fn resized(r: Rect, h: usize, d: Vec2, keep_aspect: bool) -> Rect {
    let (mut l, mut t, mut rt, mut b) = (r.left(), r.top(), r.right(), r.bottom());
    if matches!(h, 0 | 6 | 7) {
        l += d.x;
    }
    if matches!(h, 2..=4) {
        rt += d.x;
    }
    if matches!(h, 0..=2) {
        t += d.y;
    }
    if matches!(h, 4..=6) {
        b += d.y;
    }
    let min = 4.0;
    if rt - l < min {
        if matches!(h, 0 | 6 | 7) { l = rt - min } else { rt = l + min }
    }
    if b - t < min {
        if matches!(h, 0..=2) { t = b - min } else { b = t + min }
    }
    if keep_aspect && matches!(h, 0 | 2 | 4 | 6) && r.width() > 0.0 && r.height() > 0.0 {
        let s = ((rt - l) / r.width()).max((b - t) / r.height());
        let (w, hh) = (r.width() * s, r.height() * s);
        // Anchor the opposite corner.
        match h {
            0 => {
                l = rt - w;
                t = b - hh;
            }
            2 => {
                rt = l + w;
                t = b - hh;
            }
            4 => {
                rt = l + w;
                b = t + hh;
            }
            _ => {
                l = rt - w;
                b = t + hh;
            }
        }
    }
    Rect::from_min_max(pos2(l, t), pos2(rt, b))
}

/// Line boxes (l, b, r, t) covering glyphs `a..=b`, merged per line.
pub fn line_boxes(glyphs: &[Glyph], a: usize, b: usize) -> Vec<[f32; 4]> {
    let (a, b) = (a.min(b), a.max(b));
    let mut out: Vec<[f32; 4]> = Vec::new();
    for g in glyphs.get(a..=b.min(glyphs.len().saturating_sub(1))).unwrap_or(&[]) {
        let r = g.rect;
        if r[2] - r[0] <= 0.0 || r[3] - r[1] <= 0.0 {
            continue;
        }
        if let Some(cur) = out.last_mut() {
            let h = (cur[3] - cur[1]).max(r[3] - r[1]);
            let same_line = ((cur[1] + cur[3]) / 2.0 - (r[1] + r[3]) / 2.0).abs() < h * 0.5 && r[0] >= cur[0] - h;
            if same_line {
                *cur = [cur[0].min(r[0]), cur[1].min(r[1]), cur[2].max(r[2]), cur[3].max(r[3])];
                continue;
            }
        }
        out.push(r);
    }
    out
}

/// The glyph under (or nearest to, within a few points) a user-space position.
fn glyph_near(glyphs: &[Glyph], p: (f32, f32)) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (i, g) in glyphs.iter().enumerate() {
        let r = g.rect;
        if r[2] - r[0] <= 0.0 {
            continue;
        }
        let dx = (r[0] - p.0).max(p.0 - r[2]).max(0.0);
        let dy = (r[1] - p.1).max(p.1 - r[3]).max(0.0);
        let d = dx * dx + dy * dy * 4.0;
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.filter(|(d, _)| *d < 24.0 * 24.0).map(|(_, i)| i)
}

fn annot_pickable(a: &Annot) -> bool {
    !matches!(a.subtype, pdf::ANNOT_WIDGET | pdf::ANNOT_POPUP | pdf::ANNOT_LINK)
}

fn annot_movable(a: &Annot) -> bool {
    matches!(a.subtype, pdf::ANNOT_TEXT | pdf::ANNOT_FREETEXT | pdf::ANNOT_SQUARE | pdf::ANNOT_CIRCLE | pdf::ANNOT_STAMP | pdf::ANNOT_INK)
}

/// Positions of each page in content coordinates, and the content size.
fn layout(sizes: &[(f32, f32)], zoom: f32, min_width: f32) -> (Vec<Rect>, Vec2) {
    let widest = sizes.iter().map(|s| s.0).fold(0.0, f32::max) * zoom + 2.0 * MARGIN;
    let width = widest.max(min_width);
    let mut y = MARGIN;
    let mut rects = Vec::with_capacity(sizes.len());
    for &(w, h) in sizes {
        let (w, h) = (w * zoom, h * zoom);
        rects.push(Rect::from_min_size(pos2(((width - w) / 2.0).round(), y.round()), vec2(w, h)));
        y += h + GAP;
    }
    (rects, vec2(width, y - GAP + MARGIN))
}

impl Editor {
    /// Zoom to `z`, keeping the content point under `anchor` (screen) in place.
    pub fn set_zoom(&mut self, z: f32, anchor: Option<Pos2>) {
        let z = z.clamp(MIN_ZOOM, MAX_ZOOM);
        if (z - self.zoom).abs() < 1e-4 {
            return;
        }
        let k = z / self.zoom;
        let a = anchor.unwrap_or(self.view.canvas_rect.center());
        let local = a - self.view.canvas_rect.min;
        let content = self.view.offset + local;
        self.view.pending_offset = Some((content * k - local).max(Vec2::ZERO));
        self.zoom = z;
        self.view.zoom_changed = now();
    }

    /// A thumbnail of page `i`, rendered when first asked for (a few per frame).
    pub fn thumbnail(&mut self, ctx: &egui::Context, i: usize, budget: &mut u32) -> Option<egui::TextureHandle> {
        let f = self.file.as_ref()?;
        let rev = *f.revs.get(i)?;
        let t = now();
        if let Some(th) = self.view.thumbs.get_mut(&i)
            && th.epoch == self.epoch
            && th.rev == rev
        {
            th.used = t;
            return Some(th.tex.clone());
        }
        let stale = self.view.thumbs.get(&i).map(|th| th.tex.clone());
        if *budget == 0 {
            ctx.request_repaint();
            return stale;
        }
        *budget -= 1;
        let (w, h) = f.sizes[i];
        let scale = THUMB_W * ctx.pixels_per_point() / w.max(1.0);
        let img = f.doc.page(i)?.render(scale, [0, 0, (w * scale).round() as i32, (h * scale).round() as i32])?;
        let tex = ctx.load_texture(format!("thumb-{i}"), img, egui::TextureOptions::LINEAR);
        self.view.thumbs.insert(i, Thumb { tex: tex.clone(), epoch: self.epoch, rev, used: t });
        // Keep memory bounded on long documents.
        if self.view.thumbs.len() > 240 {
            let mut by_age: Vec<(f64, usize)> = self.view.thumbs.iter().map(|(k, v)| (v.used, *k)).collect();
            by_age.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (_, k) in by_age.into_iter().take(60) {
                self.view.thumbs.remove(&k);
            }
        }
        Some(tex)
    }

    pub fn reset_view_caches(&mut self) {
        self.view.pages.clear();
        self.view.thumbs.clear();
    }
}

fn now() -> f64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

/// Shown when no document is open.
fn start_screen(ui: &mut Ui, ed: &mut Editor) {
    let p = pal(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, p.bg_elevated);
    let width = 360.0f32.min(rect.width() - 32.0);
    let top = rect.top() + (rect.height() * 0.22).max(24.0);
    let area = Rect::from_min_size(pos2(rect.center().x - width / 2.0, top), vec2(width, rect.bottom() - top));
    ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
            theme::paint_mark(ui.painter(), r, p.text, p.accent);
            ui.add_space(4.0);
            ui.label(egui::RichText::new("dockit").family(theme::display_family()).size(26.0).color(p.text));
            ui.add_space(2.0);
            ui.label(theme::secondary(ui, "Edit, combine, split, sign and fill in PDFs."));
            ui.add_space(18.0);
            let open = egui::Button::new(theme::semibold("Open a PDF…").color(Color32::WHITE)).fill(p.accent).min_size(vec2(180.0, 30.0));
            if ui.add(open).clicked() {
                ed.open_dialog();
            }
            if ui.add(egui::Button::new("Combine files…").min_size(vec2(180.0, 28.0))).clicked() {
                ed.request = Some(Request::Combine);
            }
            ui.add_space(6.0);
            ui.label(theme::secondary(ui, "or drop a PDF here"));
            let recent: Vec<_> = ed.recent.iter().filter(|p| p.exists()).take(6).cloned().collect();
            if !recent.is_empty() {
                ui.add_space(18.0);
                theme::heading(ui, "Recent");
                ui.add_space(4.0);
                for path in recent {
                    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    let resp = ui.add(egui::Button::new(name).frame(false)).on_hover_text(path.display().to_string());
                    if resp.clicked() {
                        ed.open_path(&path);
                    }
                }
            }
        });
    });
}

pub fn canvas_ui(ui: &mut Ui, ed: &mut Editor) {
    ed.view.canvas_rect = ui.max_rect();
    ed.view.frame = ui.ctx().cumulative_frame_nr();
    if ed.file.is_none() {
        start_screen(ui, ed);
        return;
    }
    let ctx = ui.ctx().clone();
    let p = pal(&ctx);
    let ppp = ctx.pixels_per_point();
    let avail = ui.available_size();
    ed.view.viewport = avail;

    // Fit requests use the current page's size.
    if let Some(width) = ed.fit.take() {
        let f = ed.file.as_ref().unwrap();
        let (w, h) = f.sizes.get(ed.current).copied().unwrap_or((612.0, 792.0));
        let zw = (avail.x - 2.0 * MARGIN - 14.0) / w;
        let z = if width { zw } else { zw.min((avail.y - 2.0 * MARGIN) / h) };
        ed.zoom = z.clamp(MIN_ZOOM, MAX_ZOOM);
        ed.view.zoom_changed = now();
        ed.goto = Some(ed.current);
    }

    // Ctrl + wheel (or pinch) zooms around the pointer.
    let hover = ui.rect_contains_pointer(ui.max_rect());
    let zd = ui.input(|i| i.zoom_delta());
    if hover && (zd - 1.0).abs() > 1e-4 {
        let anchor = ui.input(|i| i.pointer.hover_pos());
        ed.set_zoom(ed.zoom * zd, anchor);
    }

    let f = ed.file.as_ref().unwrap();
    let sizes = f.sizes.clone();
    let zoom = ed.zoom;
    let (rects, content) = layout(&sizes, zoom, avail.x);

    let mut sa = egui::ScrollArea::both().id_salt("document").auto_shrink([false, false]);
    if let Some(o) = ed.view.pending_offset.take() {
        sa = sa.scroll_offset(o);
    } else if let Some(g) = ed.goto.take()
        && let Some(r) = rects.get(g)
    {
        sa = sa.vertical_scroll_offset((r.top() - MARGIN).max(0.0));
    }

    let out = sa.show_viewport(ui, |ui, viewport| {
        let (rect, resp) = ui.allocate_exact_size(content, Sense::click_and_drag());
        let origin = rect.min;
        let painter = ui.painter().clone();
        let clip = ui.clip_rect();
        painter.rect_filled(clip, 0.0, p.bg);

        let visible: Vec<usize> = (0..rects.len()).filter(|&i| rects[i].intersects(viewport)).collect();
        // The current page is the one under the middle of the view.
        let mid = viewport.center().y;
        if let Some(&c) = visible.iter().min_by(|&&a, &&b| {
            let da = (rects[a].center().y - mid).abs() - rects[a].height() / 2.0;
            let db = (rects[b].center().y - mid).abs() - rects[b].height() / 2.0;
            da.total_cmp(&db)
        }) {
            ed.current = c;
        }

        let screen = |i: usize| {
            let r = rects[i].translate(origin.to_vec2());
            // Whole pixels keep the page pictures sharp.
            Rect::from_min_size(pos2((r.min.x * ppp).round() / ppp, (r.min.y * ppp).round() / ppp), r.size())
        };
        let screen_view = viewport.translate(origin.to_vec2());

        // ── page pictures ──
        let settled = now() - ed.view.zoom_changed > 0.15;
        let mut budget = 2u32;
        for &i in &visible {
            let r = screen(i);
            painter.rect_filled(r, 0.0, Color32::WHITE);
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, p.hairline), StrokeKind::Outside);
            ensure_page(ed, &ctx, i, r, screen_view, settled, &mut budget);
            if let Some(t) = ed.view.pages.get(&i) {
                let k = (zoom * ppp) / t.scale;
                let [x0, y0, x1, y1] = t.region;
                let dest = Rect::from_min_max(r.min + vec2(x0 as f32, y0 as f32) * k / ppp, r.min + vec2(x1 as f32, y1 as f32) * k / ppp);
                painter.image(t.tex.id(), dest, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
        if !settled {
            ctx.request_repaint_after(std::time::Duration::from_millis(160));
        }
        // Drop pictures of pages far from view.
        let keep: Vec<usize> = visible.clone();
        if ed.view.pages.len() > keep.len() + 6 {
            let lo = keep.first().copied().unwrap_or(0).saturating_sub(3);
            let hi = keep.last().copied().unwrap_or(0) + 3;
            ed.view.pages.retain(|k, _| (lo..=hi).contains(k));
        }

        // ── interaction ──
        let maps: HashMap<usize, Map> =
            visible.iter().filter_map(|&i| ed.file.as_ref().and_then(|f| f.geom(i)).map(|g| (i, Map { r: screen(i), g, zoom }))).collect();
        let page_at = |pos: Pos2| maps.iter().find(|(_, m)| m.r.contains(pos)).map(|(i, m)| (*i, *m));
        let pointer = resp.hover_pos();
        let mods = ui.input(|i| i.modifiers);

        interact(ui, ed, &resp, &maps, &page_at, pointer, mods, &painter);
        overlays(ui, ed, &maps, pointer, &painter);
        editors(ui, ed, &maps);
        context_menu(ed, &resp, &page_at);
    });
    ed.view.offset = out.state.offset;
}

/// Make sure a picture of page `i` at the current zoom covers what is visible.
fn ensure_page(ed: &mut Editor, ctx: &egui::Context, i: usize, r: Rect, view: Rect, settled: bool, budget: &mut u32) {
    let Some(f) = &ed.file else { return };
    let ppp = ctx.pixels_per_point();
    let scale = ed.zoom * ppp;
    let (w, h) = f.sizes[i];
    let (fw, fh) = ((w * scale).round() as i32, (h * scale).round() as i32);
    let full = (fw as f32) * (fh as f32) <= MAX_FULL_PIXELS && fw.max(fh) <= 8192;
    let need = if full {
        [0, 0, fw, fh]
    } else {
        let vis = r.intersect(view.expand(200.0));
        let a = (vis.min - r.min) * ppp;
        let b = (vis.max - r.min) * ppp;
        [a.x.floor().max(0.0) as i32, a.y.floor().max(0.0) as i32, (b.x.ceil() as i32).min(fw), (b.y.ceil() as i32).min(fh)]
    };
    let rev = f.revs[i];
    let good = ed.view.pages.get(&i).is_some_and(|t| {
        let [a, b, c, d] = t.region;
        t.epoch == ed.epoch && t.rev == rev && (t.scale - scale).abs() < scale * 0.002 && a <= need[0] && b <= need[1] && c >= need[2] && d >= need[3]
    });
    if good {
        return;
    }
    // Content changes always redraw at once; zoom changes wait until zooming pauses.
    let stale_content = ed.view.pages.get(&i).is_none_or(|t| t.epoch != ed.epoch || t.rev != rev);
    if (!settled && !stale_content) || *budget == 0 {
        ctx.request_repaint();
        return;
    }
    *budget -= 1;
    if need[2] <= need[0] || need[3] <= need[1] {
        return;
    }
    let Some(img) = f.doc.page(i).and_then(|pg| pg.render(scale, need)) else { return };
    let tex = ctx.load_texture(format!("page-{i}"), img, egui::TextureOptions::LINEAR);
    ed.view.pages.insert(i, PageTex { tex, scale, region: need, epoch: ed.epoch, rev });
}

/// The objects hit at `pos` on a page, smallest first.
fn object_at(objs: &[Obj], m: &Map, pos: Pos2) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for o in objs {
        let q = m.quad(&o.quad);
        let b = quad_bounds(&q);
        if !b.expand(4.0).contains(pos) || !hit_quad(pos, &q, 3.0) {
            continue;
        }
        let area = b.area();
        if best.is_none_or(|(a, _)| area <= a) {
            best = Some((area, o.index));
        }
    }
    best.map(|(_, i)| i)
}

fn annot_at(annots: &[Annot], m: &Map, pos: Pos2, widgets: bool) -> Option<usize> {
    annots
        .iter()
        .rev()
        .find(|a| if widgets { a.subtype == pdf::ANNOT_WIDGET && a.field.is_some() } else { annot_pickable(a) } && m.rect(a.rect).expand(2.0).contains(pos))
        .map(|a| a.index)
}

/// Screen bounds of the selected objects.
fn selection_bounds(ed: &Editor, maps: &HashMap<usize, Map>) -> Option<(usize, Rect)> {
    let f = ed.file.as_ref()?;
    match &ed.sel {
        Sel::Objects { page, items } => {
            let m = maps.get(page)?;
            let objs = f.objects(*page);
            let mut r = Rect::NOTHING;
            for o in objs.iter().filter(|o| items.contains(&o.index)) {
                r = r.union(quad_bounds(&m.quad(&o.quad)));
            }
            r.is_positive().then_some((*page, r))
        }
        Sel::Annot { page, index } => {
            let m = maps.get(page)?;
            let a = f.annots(*page).iter().find(|a| a.index == *index)?.clone();
            Some((*page, m.rect(a.rect)))
        }
        Sel::None => None,
    }
}

fn keep_aspect_default(ed: &Editor) -> bool {
    let Some(f) = &ed.file else { return false };
    let Sel::Objects { page, items } = &ed.sel else { return false };
    let objs = f.objects(*page);
    objs.iter().filter(|o| items.contains(&o.index)).any(|o| o.kind != Kind::Path)
}

#[allow(clippy::too_many_arguments)]
fn interact(
    ui: &mut Ui,
    ed: &mut Editor,
    resp: &egui::Response,
    maps: &HashMap<usize, Map>,
    page_at: &dyn Fn(Pos2) -> Option<(usize, Map)>,
    pointer: Option<Pos2>,
    mods: egui::Modifiers,
    painter: &egui::Painter,
) {
    let ctx = ui.ctx().clone();
    let press = ui.input(|i| i.pointer.press_origin());

    // Middle button or Space + drag pans.
    let pan = ui.input(|i| i.pointer.middle_down() || (i.key_down(Key::Space) && i.pointer.primary_down())) && !ctx.egui_wants_keyboard_input();
    if pan && resp.hovered() {
        let d = ui.input(|i| i.pointer.delta());
        ed.view.pending_offset = Some((ed.view.offset - d).max(Vec2::ZERO));
        ed.view.drag = Some(Drag::Pan);
        ctx.set_cursor_icon(CursorIcon::Grabbing);
        return;
    }
    if matches!(ed.view.drag, Some(Drag::Pan)) {
        ed.view.drag = None;
    }

    let sel_bounds = selection_bounds(ed, maps);
    let tool = ed.tool;

    // ── hover feedback ──
    if let (Some(pos), None) = (pointer, &ed.view.drag)
        && let Some((page, m)) = page_at(pos)
    {
        let f = ed.file.as_ref().unwrap();
        match tool {
            Tool::Select => {
                let on_handle = matches!(ed.sel, Sel::Objects { .. })
                    && sel_bounds.is_some_and(|(sp, r)| sp == page && handles(r).iter().any(|h| h.distance(pos) <= HANDLE));
                if on_handle {
                    let (_, r) = sel_bounds.unwrap();
                    let h = handles(r).iter().position(|h| h.distance(pos) <= HANDLE).unwrap();
                    ctx.set_cursor_icon(resize_cursor(h));
                } else if let Some(fi) = annot_at(&f.annots(page), &m, pos, true) {
                    let kind = f.annots(page).iter().find(|a| a.index == fi).and_then(|a| a.field.as_ref().map(|x| x.kind)).unwrap_or(0);
                    ctx.set_cursor_icon(if kind == pdf::FIELD_TEXT { CursorIcon::Text } else { CursorIcon::PointingHand });
                } else if let Some(ai) = annot_at(&f.annots(page), &m, pos, false) {
                    if let Some(a) = f.annots(page).iter().find(|a| a.index == ai) {
                        painter.rect_stroke(m.rect(a.rect), 2.0, Stroke::new(1.0, p_accent(ui).gamma_multiply(0.7)), StrokeKind::Outside);
                    }
                } else if let Some(oi) = object_at(&f.objects(page), &m, pos)
                    && let Some(o) = f.objects(page).iter().find(|o| o.index == oi)
                {
                    let q = m.quad(&o.quad);
                    painter.add(egui::Shape::closed_line(q.to_vec(), Stroke::new(1.0, p_accent(ui).gamma_multiply(0.7))));
                    if o.kind == Kind::Text {
                        ctx.set_cursor_icon(CursorIcon::Default);
                    }
                }
            }
            Tool::Text => {
                let over_text = object_at(&f.objects(page), &m, pos).is_some_and(|oi| f.objects(page).iter().any(|o| o.index == oi && o.kind == Kind::Text));
                ctx.set_cursor_icon(CursorIcon::Text);
                if over_text {
                    let oi = object_at(&f.objects(page), &m, pos).unwrap();
                    if let Some(o) = f.objects(page).iter().find(|o| o.index == oi) {
                        painter.add(egui::Shape::closed_line(m.quad(&o.quad).to_vec(), Stroke::new(1.0, p_accent(ui).gamma_multiply(0.7))));
                    }
                }
            }
            Tool::Highlight => ctx.set_cursor_icon(CursorIcon::Text),
            _ => ctx.set_cursor_icon(CursorIcon::Crosshair),
        }
    }

    // ── clicks ──
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        click(ed, page_at(pos), pos, mods);
    }
    if resp.double_clicked()
        && tool == Tool::Select
        && let Some((page, m)) = resp.interact_pointer_pos().and_then(page_at)
    {
        let pos = resp.interact_pointer_pos().unwrap();
        let f = ed.file.as_ref().unwrap();
        if let Some(ai) = annot_at(&f.annots(page), &m, pos, false) {
            if f.annots(page).iter().any(|a| a.index == ai && a.subtype == pdf::ANNOT_TEXT) {
                ed.edit_note(page, ai);
            }
        } else if let Some(oi) = object_at(&f.objects(page), &m, pos)
            && f.objects(page).iter().any(|o| o.index == oi && o.kind == Kind::Text)
        {
            ed.begin_text_edit(page, oi);
        }
    }

    // ── drags ──
    if resp.drag_started()
        && let Some(start) = press
    {
        ed.view.drag = start_drag(ed, page_at(start), start, sel_bounds);
    }
    let Some(pos) = pointer.or(resp.interact_pointer_pos()) else { return };
    if resp.dragged() {
        if let Some(Drag::Ink { page, pts }) = &mut ed.view.drag
            && let Some(m) = maps.get(page)
        {
            let u = m.user(pos);
            if pts.last().is_none_or(|l| (l.0 - u.0).hypot(l.1 - u.1) * m.zoom > 1.5) {
                pts.push(u);
            }
        }
        preview(ui, ed, maps, pos, mods, painter);
    }
    if resp.drag_stopped()
        && let Some(d) = ed.view.drag.take()
    {
        finish_drag(ed, d, maps, pos, mods);
    }
}

fn p_accent(ui: &Ui) -> Color32 {
    pal(ui.ctx()).accent
}

fn click(ed: &mut Editor, at: Option<(usize, Map)>, pos: Pos2, mods: egui::Modifiers) {
    ed.view.field_menu = None;
    let Some((page, m)) = at else {
        ed.commit_edits();
        ed.clear_selection();
        return;
    };
    let u = m.user(pos);
    match ed.tool {
        Tool::Select => {
            ed.commit_edits();
            let f = ed.file.as_ref().unwrap();
            let annots = f.annots(page);
            if let Some(fi) = annot_at(&annots, &m, pos, true) {
                let field = annots.iter().find(|a| a.index == fi).and_then(|a| a.field.clone()).unwrap_or_default();
                if field.read_only() {
                    ed.say(format!("{} is read-only", if field.name.is_empty() { "This field".into() } else { field.name.clone() }));
                    return;
                }
                match field.kind {
                    pdf::FIELD_TEXT => ed.begin_field_edit(page, fi),
                    pdf::FIELD_CHECKBOX | pdf::FIELD_RADIO => ed.click_field(page, fi),
                    pdf::FIELD_COMBO | pdf::FIELD_LIST => ed.view.field_menu = Some((page, fi, pos, ed.view.frame)),
                    pdf::FIELD_SIGNATURE => {
                        let r = annots.iter().find(|a| a.index == fi).map(|a| a.rect).unwrap_or([u.0, u.1, u.0, u.1]);
                        ed.place_signature(page, ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0));
                    }
                    _ => {}
                }
                ed.clear_selection();
                return;
            }
            if let Some(ai) = annot_at(&annots, &m, pos, false) {
                ed.sel = Sel::Annot { page, index: ai };
                return;
            }
            match object_at(&f.objects(page), &m, pos) {
                Some(oi) => {
                    if (mods.shift || mods.command)
                        && let Sel::Objects { page: sp, items } = &mut ed.sel
                        && *sp == page
                    {
                        if let Some(k) = items.iter().position(|&x| x == oi) {
                            items.remove(k);
                        } else {
                            items.push(oi);
                        }
                        if items.is_empty() {
                            ed.sel = Sel::None;
                        }
                        return;
                    }
                    ed.sel = Sel::Objects { page, items: vec![oi] };
                }
                None => ed.clear_selection(),
            }
        }
        Tool::Text => {
            let f = ed.file.as_ref().unwrap();
            let objs = f.objects(page);
            match object_at(&objs, &m, pos).filter(|oi| objs.iter().any(|o| o.index == *oi && o.kind == Kind::Text)) {
                Some(oi) => ed.begin_text_edit(page, oi),
                None => {
                    // The click marks the top of the new text; its baseline sits one cap height lower.
                    let size = ed.style.text_size * m.zoom;
                    let base = m.user(pos + vec2(0.0, size * 0.8));
                    ed.begin_new_text(page, base);
                }
            }
        }
        Tool::Note => ed.begin_note(page, u),
        Tool::Stamp => ed.add_stamp(page, u),
        Tool::Sign => ed.place_signature(page, u),
        Tool::Image => {
            if ed.image.is_none() && !ed.pick_image() {
                return;
            }
            let img = ed.image.as_ref().unwrap();
            let w = (img.w as f32 * 0.75).min(m.g.w * 0.5) / m.g.unit().max(1e-3);
            let h = w * img.h as f32 / img.w.max(1) as f32;
            ed.place_image(page, [u.0 - w / 2.0, u.1 - h / 2.0, w, h]);
        }
        _ => {}
    }
}

fn start_drag(ed: &mut Editor, at: Option<(usize, Map)>, start: Pos2, sel_bounds: Option<(usize, Rect)>) -> Option<Drag> {
    let (page, m) = at?;
    ed.view.field_menu = None;
    match ed.tool {
        Tool::Select => {
            ed.commit_edits();
            if let (Sel::Objects { .. }, Some((sp, r))) = (&ed.sel, sel_bounds)
                && sp == page
            {
                if let Some(h) = handles(r).iter().position(|h| h.distance(start) <= HANDLE) {
                    let keep = keep_aspect_default(ed);
                    return Some(Drag::Resize { page, handle: h, bbox: r, keep_aspect: keep, from: start });
                }
                if r.expand(2.0).contains(start) {
                    return Some(Drag::Move { page, from: start, bbox: r });
                }
            }
            let f = ed.file.as_ref().unwrap();
            let annots = f.annots(page);
            if annot_at(&annots, &m, start, true).is_some() {
                return None;
            }
            if let Some(ai) = annot_at(&annots, &m, start, false) {
                let a = annots.iter().find(|a| a.index == ai)?.clone();
                ed.sel = Sel::Annot { page, index: ai };
                return annot_movable(&a).then(|| Drag::AnnotMove { page, from: start, rect: m.rect(a.rect) });
            }
            if let Some(oi) = object_at(&f.objects(page), &m, start) {
                let o = f.objects(page).iter().find(|o| o.index == oi)?.clone();
                ed.sel = Sel::Objects { page, items: vec![oi] };
                return Some(Drag::Move { page, from: start, bbox: quad_bounds(&m.quad(&o.quad)) });
            }
            Some(Drag::Band { page, from: start })
        }
        Tool::Rect | Tool::Ellipse | Tool::Line => Some(Drag::Shape { page, from: start }),
        Tool::Highlight => Some(Drag::Markup { page, from: m.user(start) }),
        Tool::Ink => Some(Drag::Ink { page, pts: vec![m.user(start)] }),
        Tool::Image => {
            ed.image.as_ref()?;
            Some(Drag::Image { page, from: start })
        }
        _ => None,
    }
}

/// The shape rect for a drag, squared or snapped to 45° with Shift.
fn shape_end(from: Pos2, to: Pos2, tool: Tool, shift: bool) -> Pos2 {
    if !shift {
        return to;
    }
    let d = to - from;
    match tool {
        Tool::Line => {
            let a = (d.y.atan2(d.x) / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
            from + vec2(a.cos(), a.sin()) * d.length()
        }
        _ => {
            let s = d.x.abs().max(d.y.abs());
            from + vec2(s * d.x.signum(), s * d.y.signum())
        }
    }
}

fn image_rect(from: Pos2, to: Pos2, aspect: f32) -> Rect {
    let d = to - from;
    let w = d.x.abs().max(d.y.abs() * aspect);
    let h = w / aspect;
    Rect::from_two_pos(from, from + vec2(w * d.x.signum().max(-1.0), h * if d.y < 0.0 { -1.0 } else { 1.0 }))
}

#[allow(clippy::too_many_arguments)]
fn preview(ui: &Ui, ed: &Editor, maps: &HashMap<usize, Map>, pos: Pos2, mods: egui::Modifiers, painter: &egui::Painter) {
    let p = pal(ui.ctx());
    let accent = Stroke::new(1.5, p.accent);
    let Some(drag) = &ed.view.drag else { return };
    match drag {
        Drag::Move { page, from, bbox } => {
            let d = pos - *from;
            ghost(ed, maps, *page, *bbox, bbox.translate(d), painter);
            painter.rect_stroke(bbox.translate(d), 0.0, accent, StrokeKind::Outside);
        }
        Drag::Resize { page, handle, bbox, keep_aspect, from } => {
            let r = resized(*bbox, *handle, pos - *from, *keep_aspect != mods.shift);
            ghost(ed, maps, *page, *bbox, r, painter);
            painter.rect_stroke(r, 0.0, accent, StrokeKind::Outside);
        }
        Drag::AnnotMove { rect, from, .. } => {
            painter.rect_stroke(rect.translate(pos - *from), 2.0, accent, StrokeKind::Outside);
        }
        Drag::Band { from, .. } => {
            let r = Rect::from_two_pos(*from, pos);
            painter.rect_filled(r, 0.0, p.accent.gamma_multiply(0.12));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, p.accent), StrokeKind::Inside);
        }
        Drag::Shape { page, from } => {
            let Some(m) = maps.get(page) else { return };
            let to = shape_end(*from, pos, ed.tool, mods.shift);
            let s = &ed.style;
            let stroke = Stroke::new((s.width * m.zoom).max(1.0), color32(s.stroke));
            match ed.tool {
                Tool::Line => {
                    painter.line_segment([*from, to], stroke);
                }
                Tool::Rect => {
                    let r = Rect::from_two_pos(*from, to);
                    if let Some(fc) = s.fill {
                        painter.rect_filled(r, 0.0, color32(fc));
                    }
                    painter.rect_stroke(r, 0.0, stroke, StrokeKind::Middle);
                }
                _ => {
                    let r = Rect::from_two_pos(*from, to);
                    let shape = egui::epaint::EllipseShape {
                        center: r.center(),
                        radius: r.size() / 2.0,
                        fill: s.fill.map(color32).unwrap_or(Color32::TRANSPARENT),
                        stroke,
                        angle: 0.0,
                    };
                    painter.add(shape);
                }
            }
        }
        Drag::Markup { page, from } => {
            let (Some(m), Some(f)) = (maps.get(page), ed.file.as_ref()) else { return };
            let glyphs = f.glyphs(*page);
            if let (Some(a), Some(b)) = (glyph_near(&glyphs, *from), glyph_near(&glyphs, m.user(pos))) {
                for bx in line_boxes(&glyphs, a, b) {
                    painter.rect_filled(m.rect(bx), 0.0, color32(ed.style.highlight).gamma_multiply(0.4));
                }
            }
        }
        Drag::Ink { page, pts } => {
            let Some(m) = maps.get(page) else { return };
            let line: Vec<Pos2> = pts.iter().map(|&u| m.screen(u)).collect();
            painter.add(egui::Shape::line(line, Stroke::new((ed.style.ink_width * m.zoom).max(1.0), color32(ed.style.ink))));
        }
        Drag::Image { from, .. } => {
            if let Some(img) = &ed.image {
                let r = image_rect(*from, pos, img.w as f32 / img.h.max(1) as f32);
                painter.rect_filled(r, 0.0, p.accent.gamma_multiply(0.1));
                painter.rect_stroke(r, 0.0, accent, StrokeKind::Inside);
            }
        }
        Drag::Pan => {}
    }
}

/// While dragging, draw the selected region's pixels at their new place.
fn ghost(ed: &Editor, maps: &HashMap<usize, Map>, page: usize, src: Rect, dst: Rect, painter: &egui::Painter) {
    let (Some(m), Some(t)) = (maps.get(&page), ed.view.pages.get(&page)) else { return };
    let ppp = painter.ctx().pixels_per_point();
    let k = t.scale / (m.zoom * ppp);
    let [x0, y0, x1, y1] = t.region;
    let (rw, rh) = ((x1 - x0) as f32, (y1 - y0) as f32);
    let to_uv = |p: Pos2| {
        let px = (p - m.r.min) * ppp * k;
        pos2((px.x - x0 as f32) / rw, (px.y - y0 as f32) / rh)
    };
    let uv = Rect::from_min_max(to_uv(src.min), to_uv(src.max));
    painter.image(t.tex.id(), dst, uv, Color32::from_white_alpha(200));
}

fn finish_drag(ed: &mut Editor, d: Drag, maps: &HashMap<usize, Map>, pos: Pos2, mods: egui::Modifiers) {
    let press_moved = |from: Pos2| (pos - from).length() > 2.0;
    match d {
        Drag::Move { page, from, .. } => {
            let Some(m) = maps.get(&page) else { return };
            if press_moved(from) {
                let (a, b) = (m.user(from), m.user(pos));
                ed.move_selection(b.0 - a.0, b.1 - a.1);
            }
        }
        Drag::Resize { page, handle, bbox, keep_aspect, from } => {
            let Some(m) = maps.get(&page) else { return };
            let r = resized(bbox, handle, pos - from, keep_aspect != mods.shift);
            ed.reshape_selection(m.user_rect(bbox), m.user_rect(r));
        }
        Drag::AnnotMove { page, from, .. } => {
            let Some(m) = maps.get(&page) else { return };
            if press_moved(from) {
                let (a, b) = (m.user(from), m.user(pos));
                ed.move_selection(b.0 - a.0, b.1 - a.1);
            }
        }
        Drag::Band { page, from } => {
            let (Some(m), Some(f)) = (maps.get(&page), ed.file.as_ref()) else { return };
            let band = Rect::from_two_pos(from, pos);
            let items: Vec<usize> = f.objects(page).iter().filter(|o| band.contains_rect(quad_bounds(&m.quad(&o.quad)))).map(|o| o.index).collect();
            ed.sel = if items.is_empty() { Sel::None } else { Sel::Objects { page, items } };
        }
        Drag::Shape { page, from } => {
            let Some(m) = maps.get(&page) else { return };
            let to = shape_end(from, pos, ed.tool, mods.shift);
            if (to - from).length() < 3.0 {
                return;
            }
            let s = ed.style.clone();
            let style =
                pdf::PathStyle { fill: if ed.tool == Tool::Line { None } else { s.fill }, stroke: Some(s.stroke), width: s.width / m.g.unit().max(1e-3) };
            let segs = match ed.tool {
                Tool::Line => {
                    let (a, b) = (m.user(from), m.user(to));
                    vec![Seg::Move(a.0, a.1), Seg::Line(b.0, b.1)]
                }
                Tool::Rect => {
                    let r = Rect::from_two_pos(from, to);
                    let c = [m.user(r.left_top()), m.user(r.right_top()), m.user(r.right_bottom()), m.user(r.left_bottom())];
                    vec![Seg::Move(c[0].0, c[0].1), Seg::Line(c[1].0, c[1].1), Seg::Line(c[2].0, c[2].1), Seg::Line(c[3].0, c[3].1), Seg::Close]
                }
                _ => {
                    let u = m.user_rect(Rect::from_two_pos(from, to));
                    pdf::ellipse(u[0], u[1], u[2], u[3])
                }
            };
            ed.add_path(page, segs, style);
        }
        Drag::Markup { page, from } => {
            let (Some(m), Some(f)) = (maps.get(&page), ed.file.as_ref()) else { return };
            let glyphs = f.glyphs(page);
            match (glyph_near(&glyphs, from), glyph_near(&glyphs, m.user(pos))) {
                (Some(a), Some(b)) => {
                    let boxes = line_boxes(&glyphs, a, b);
                    if !boxes.is_empty() {
                        ed.add_markup(page, boxes);
                    }
                }
                _ => ed.say(if glyphs.is_empty() { "This page has no text layer to mark (it may be a scan)." } else { "No text there to mark." }),
            }
        }
        Drag::Ink { page, pts } => {
            if pts.len() > 1 {
                ed.add_ink(page, pts);
            }
        }
        Drag::Image { page, from } => {
            let Some(m) = maps.get(&page) else { return };
            let Some(img) = &ed.image else { return };
            let r = image_rect(from, pos, img.w as f32 / img.h.max(1) as f32);
            if r.width() < 6.0 {
                return;
            }
            let u = m.user_rect(r);
            ed.place_image(page, [u[0], u[1], u[2] - u[0], u[3] - u[1]]);
        }
        Drag::Pan => {}
    }
}

/// Selection outlines, handles and the placement previews that follow the pointer.
fn overlays(ui: &Ui, ed: &mut Editor, maps: &HashMap<usize, Map>, pointer: Option<Pos2>, painter: &egui::Painter) {
    let p = pal(ui.ctx());
    let dragging = ed.view.drag.is_some() && !matches!(ed.view.drag, Some(Drag::Pan));
    if let (Some((page, r)), false) = (selection_bounds(ed, maps), dragging) {
        let f = ed.file.as_ref().unwrap();
        if let (Sel::Objects { items, .. }, Some(m)) = (&ed.sel, maps.get(&page)) {
            // Each object's own outline, then the shared box with handles.
            for o in f.objects(page).iter().filter(|o| items.contains(&o.index)) {
                painter.add(egui::Shape::closed_line(m.quad(&o.quad).to_vec(), Stroke::new(1.0, p.accent.gamma_multiply(0.6))));
            }
            painter.rect_stroke(r, 0.0, Stroke::new(1.5, p.accent), StrokeKind::Outside);
            if ed.text_edit.is_none() {
                for h in handles(r) {
                    let hr = Rect::from_center_size(h, vec2(HANDLE, HANDLE));
                    painter.rect_filled(hr, 1.5, Color32::WHITE);
                    painter.rect_stroke(hr, 1.5, Stroke::new(1.2, p.accent), StrokeKind::Inside);
                }
            }
        } else {
            painter.rect_stroke(r.expand(1.0), 3.0, Stroke::new(1.5, p.accent), StrokeKind::Outside);
        }
    }
    // Placement previews.
    let Some(pos) = pointer else { return };
    if dragging || !maps.values().any(|m| m.r.contains(pos)) {
        return;
    }
    let m = maps.values().find(|m| m.r.contains(pos)).copied().unwrap();
    let ghost_stroke = Stroke::new(1.0, p.accent);
    match ed.tool {
        Tool::Image => {
            if let Some(img) = &ed.image {
                let w = (img.w as f32 * 0.75).min(m.g.w * 0.5) * m.zoom;
                let h = w * img.h as f32 / img.w.max(1) as f32;
                let r = Rect::from_center_size(pos, vec2(w, h));
                painter.rect_filled(r, 0.0, p.accent.gamma_multiply(0.08));
                painter.rect_stroke(r, 0.0, ghost_stroke, StrokeKind::Inside);
            }
        }
        Tool::Sign => {
            if let Some(sig) = &ed.signature {
                let w = 170.0 * m.zoom;
                let h = match sig {
                    Signature::Drawn { aspect, .. } => w / aspect.max(0.1),
                    _ => w * 0.35,
                };
                painter.rect_stroke(Rect::from_center_size(pos, vec2(w, h)), 3.0, ghost_stroke, StrokeKind::Inside);
            }
        }
        Tool::Stamp => {
            let label = ed.style.stamp.trim().to_uppercase();
            let h = 34.0;
            let w = (pdf::estimate_width(&label, h * 0.48) + h * 0.9) * m.zoom;
            let r = Rect::from_center_size(pos, vec2(w, h * m.zoom));
            let c = color32(ed.style.stamp_color);
            painter.rect_stroke(r, h * 0.18 * m.zoom, Stroke::new(1.5, c.gamma_multiply(0.6)), StrokeKind::Inside);
            painter.text(r.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::new(h * 0.48 * m.zoom, theme::semibold_family()), c.gamma_multiply(0.6));
        }
        _ => {}
    }
}

/// Text boxes and pop-ups for typing on the page.
fn editors(ui: &mut Ui, ed: &mut Editor, maps: &HashMap<usize, Map>) {
    let p = pal(ui.ctx());
    let enter = |ui: &Ui| ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);
    let escape = |ui: &Ui| ui.input(|i| i.key_pressed(Key::Escape));

    // Page text.
    if let Some(t) = &mut ed.text_edit
        && let Some(m) = maps.get(&t.page)
    {
        let size = (t.size * m.zoom).clamp(6.0, 200.0);
        let rect = match t.object {
            Some(oi) => {
                let objs = ed.file.as_ref().unwrap().objects(t.page);
                objs.iter().find(|o| o.index == oi).map(|o| quad_bounds(&m.quad(&o.quad))).unwrap_or(Rect::from_min_size(m.r.min, vec2(200.0, size)))
            }
            None => {
                let base = m.screen(t.at);
                Rect::from_min_size(base - vec2(0.0, size * 0.95), vec2(40.0, size * 1.25))
            }
        };
        let lines = t.text.lines().count().max(1) as f32;
        let width = rect.width().max(size * 0.6 * t.text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32 + 24.0).max(120.0);
        let r = Rect::from_min_size(rect.min - vec2(4.0, 3.0), vec2(width + 8.0, (size * 1.25 * lines).max(rect.height()) + 6.0));
        ui.painter().rect_filled(r, 3.0, Color32::WHITE);
        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, p.accent), StrokeKind::Outside);
        let edit = egui::TextEdit::multiline(&mut t.text)
            .id(egui::Id::new("page-text-edit"))
            .font(egui::FontId::proportional(size))
            .text_color(Color32::from_rgb(20, 22, 28))
            .frame(egui::Frame::NONE)
            .desired_rows(1)
            .return_key(Some(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, Key::Enter)))
            .margin(egui::Margin::symmetric(4, 2));
        let resp = ui.put(r, edit);
        if t.focus {
            resp.request_focus();
            t.focus = false;
        }
        if escape(ui) {
            ed.text_edit = None;
        } else if (resp.has_focus() && enter(ui)) || resp.lost_focus() {
            ed.commit_text();
            ui.ctx().memory_mut(|m| m.stop_text_input());
        }
    }

    // Form text fields.
    if let Some(fe) = &mut ed.field_edit
        && let Some(m) = maps.get(&fe.page)
    {
        let rect = ed.file.as_ref().unwrap().annots(fe.page).iter().find(|a| a.index == fe.index).map(|a| m.rect(a.rect));
        if let Some(rect) = rect {
            let size = if fe.multiline { (12.0 * m.zoom).clamp(8.0, 40.0) } else { (rect.height() * 0.62).clamp(8.0, 40.0) };
            ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.5, p.accent), StrokeKind::Outside);
            let edit = if fe.multiline { egui::TextEdit::multiline(&mut fe.text) } else { egui::TextEdit::singleline(&mut fe.text) };
            let edit = edit
                .id(egui::Id::new("field-edit"))
                .font(egui::FontId::proportional(size))
                .text_color(Color32::from_rgb(20, 22, 28))
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(3, 1));
            let resp = ui.put(rect, edit);
            if fe.focus {
                resp.request_focus();
                fe.focus = false;
            }
            if escape(ui) {
                ed.field_edit = None;
            } else if (!fe.multiline && resp.has_focus() && enter(ui)) || resp.lost_focus() {
                ed.commit_field();
                ui.ctx().memory_mut(|m| m.stop_text_input());
            }
        }
    }

    // Drop-down and list fields.
    if let Some((page, index, at, opened)) = ed.view.field_menu {
        let field = ed.file.as_ref().and_then(|f| f.annots(page).iter().find(|a| a.index == index).and_then(|a| a.field.clone()));
        if let Some(field) = field {
            let mut chosen = None;
            let area = egui::Area::new(egui::Id::new("field-menu")).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(160.0);
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        for (k, (label, on)) in field.options.iter().enumerate() {
                            if ui.selectable_label(*on, label).clicked() {
                                chosen = Some(k);
                            }
                        }
                        if field.options.is_empty() {
                            ui.label(theme::secondary(ui, "This list has no options"));
                        }
                    });
                });
            });
            if let Some(k) = chosen {
                ed.select_option(page, index, k);
                ed.view.field_menu = None;
            } else if (area.response.clicked_elsewhere() && ed.view.frame > opened) || escape(ui) {
                ed.view.field_menu = None;
            }
        } else {
            ed.view.field_menu = None;
        }
    }

    // Note pop-up.
    if let Some(n) = &mut ed.note
        && let Some(m) = maps.get(&n.page)
    {
        let at = m.screen(n.at) + vec2(26.0, 0.0);
        let mut done = None;
        egui::Area::new(egui::Id::new("note-editor")).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(240.0);
                ui.label(theme::semibold(if n.index.is_some() { "Edit comment" } else { "New comment" }));
                // Read Ctrl+Enter before the text box, which would take it as a new line.
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Enter)) {
                    done = Some(true);
                }
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut n.text)
                        .id(egui::Id::new("note-text"))
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("Type a comment"),
                );
                if n.focus {
                    resp.request_focus();
                    n.focus = false;
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ok = egui::Button::new(theme::semibold(if n.index.is_some() { "Save" } else { "Add" }).color(Color32::WHITE)).fill(p.accent);
                        if ui.add(ok).clicked() {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(false);
                        }
                    });
                });
            });
        });
        if escape(ui) {
            done = Some(false);
        }
        match done {
            Some(true) => {
                ed.commit_note();
                ui.ctx().memory_mut(|m| m.stop_text_input());
            }
            Some(false) => ed.note = None,
            None => {}
        }
    }
}

fn context_menu(ed: &mut Editor, resp: &egui::Response, page_at: &dyn Fn(Pos2) -> Option<(usize, Map)>) {
    if resp.secondary_clicked()
        && let Some((page, m)) = resp.interact_pointer_pos().and_then(page_at)
    {
        let pos = resp.interact_pointer_pos().unwrap();
        // Right-clicking something unselected selects it first.
        let inside = selection_bounds_for_page(ed, page, &m).is_some_and(|r| r.contains(pos));
        if !inside && ed.tool == Tool::Select {
            let f = ed.file.as_ref().unwrap();
            if let Some(ai) = annot_at(&f.annots(page), &m, pos, false) {
                ed.sel = Sel::Annot { page, index: ai };
            } else if let Some(oi) = object_at(&f.objects(page), &m, pos) {
                ed.sel = Sel::Objects { page, items: vec![oi] };
            } else {
                ed.sel = Sel::None;
            }
        }
        ed.current = page;
    }
    resp.context_menu(|ui| {
        match ed.sel.clone() {
            Sel::Objects { page, items } => {
                if items.len() == 1
                    && ed.file.as_ref().is_some_and(|f| f.objects(page).iter().any(|o| o.index == items[0] && o.kind == Kind::Text))
                    && ui.button("Edit text").clicked()
                {
                    ed.begin_text_edit(page, items[0]);
                    ui.close();
                }
                if ui.button(egui::RichText::new("Delete").color(pal(ui.ctx()).danger)).clicked() {
                    ed.delete_selection();
                    ui.close();
                }
                ui.separator();
            }
            Sel::Annot { page, index } => {
                let is_note = ed.file.as_ref().is_some_and(|f| f.annots(page).iter().any(|a| a.index == index && a.subtype == pdf::ANNOT_TEXT));
                if is_note && ui.button("Edit comment").clicked() {
                    ed.edit_note(page, index);
                    ui.close();
                }
                if ui.button(egui::RichText::new("Delete").color(pal(ui.ctx()).danger)).clicked() {
                    ed.delete_selection();
                    ui.close();
                }
                ui.separator();
            }
            Sel::None => {}
        }
        let page = ed.current;
        ui.label(theme::secondary(ui, format!("Page {}", page + 1)));
        if ui.button("Rotate page right").clicked() {
            ed.page_sel = vec![page];
            ed.rotate_pages(1);
            ui.close();
        }
        if ui.button("Insert blank page after").clicked() {
            ed.insert_blank(page);
            ui.close();
        }
        if ui.button(egui::RichText::new("Delete page").color(pal(ui.ctx()).danger)).clicked() {
            ed.page_sel = vec![page];
            ed.delete_pages();
            ui.close();
        }
    });
}

fn selection_bounds_for_page(ed: &Editor, page: usize, m: &Map) -> Option<Rect> {
    let mut maps = HashMap::new();
    maps.insert(page, *m);
    selection_bounds(ed, &maps).map(|(_, r)| r)
}

/// Swatches for picking a colour, plus a custom picker. Returns true if `c` changed.
pub fn color_row(ui: &mut Ui, c: &mut pdf::Rgba) -> bool {
    const SWATCHES: [[u8; 3]; 9] =
        [[20, 22, 28], [110, 110, 115], [255, 255, 255], [10, 132, 255], [225, 87, 89], [89, 161, 79], [242, 142, 43], [255, 214, 10], [176, 122, 161]];
    let p = pal(ui.ctx());
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(5.0, 5.0);
        for s in SWATCHES {
            let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
            let col = Color32::from_rgb(s[0], s[1], s[2]);
            ui.painter().circle_filled(r.center(), 8.0, col);
            ui.painter().circle_stroke(r.center(), 8.0, Stroke::new(1.0, p.hairline));
            if c[..3] == s {
                ui.painter().circle_stroke(r.center(), 10.0, Stroke::new(1.5, p.accent));
            }
            if resp.clicked() {
                *c = [s[0], s[1], s[2], c[3].max(1)];
                changed = true;
            }
        }
        let mut col = Color32::from_rgb(c[0], c[1], c[2]);
        if ui.color_edit_button_srgba(&mut col).changed() {
            *c = [col.r(), col.g(), col.b(), c[3].max(1)];
            changed = true;
        }
    });
    changed
}
