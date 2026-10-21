//! Copy and paste, stacking order, rotation, ungrouping and paragraph editing.
//!
//! Copying works on the file, not on PDFium's objects: a copy of the document has every other
//! drawing on the page removed (content surgery), and that page is then placed where it is wanted
//! as a group (a form XObject), which PDFium fills with the fonts and images it needs. The copy
//! therefore looks exactly like the original, in any document. Ungrouping turns a group back into
//! plain page content, so pasted and restacked items stay editable.

use crate::content::{self, Edit, Piece};
use crate::editor::{Editor, File, PageModel, Sel};
use crate::fonts;
use crate::pdf::{Doc, Kind};

/// Copied objects: a file whose page `page` shows only them.
pub struct Clip {
    bytes: Vec<u8>,
    page: usize,
    /// Bounds of the copy in its page's user space (l, b, r, t).
    bounds: [f32; 4],
    /// Where it came from, so pasting on the same page offsets the copy.
    origin: (Option<std::path::PathBuf>, usize),
    pastes: u32,
}

/// A run of text lines that belong together, found from their positions.
#[derive(Clone, Debug)]
pub struct Paragraph {
    /// Text objects in reading order.
    pub objects: Vec<usize>,
    pub text: String,
    /// User-space bounds (l, b, r, t).
    pub rect: [f32; 4],
    /// The first object's PDFium matrix (text matrix times CTM), font size and colour.
    first_matrix: [f32; 6],
    size: f32,
    color: [u8; 4],
    /// Left edge of the first and of the following lines, the first baseline, and the distance
    /// between baselines (all in user space).
    first_left: f32,
    left: f32,
    baseline: f32,
    spacing: f32,
    width: f32,
    /// How many lines it had.
    lines: usize,
}

/// One text object as the paragraph finder sees it.
struct Run {
    index: usize,
    x0: f32,
    x1: f32,
    base: f32,
    size: f32,
    text: String,
}

fn upright(m: &[f32; 6]) -> bool {
    m[0] > 0.0 && m[3] > 0.0 && m[1].abs() < m[0] * 0.02 && m[2].abs() < m[3] * 0.02
}

impl Editor {
    // ── copy and paste ──

    /// A copy of the document in which page `page` shows only `items`.
    fn isolate(&self, page: usize, items: &[usize]) -> Result<Vec<u8>, String> {
        let f = self.file.as_ref().ok_or("No document is open.")?;
        let m = PageModel::load(f, page)?;
        let edits: Vec<(usize, Edit)> = (0..m.parsed.objs.len()).filter(|i| !items.contains(i)).map(|i| (i, Edit::Delete)).collect();
        let new = content::edit(&m.data.content, &m.parsed, &m.starts, &edits)?;
        crate::incremental::write_update(&m.bytes, &m.data, &new, Vec::new(), Vec::new())
    }

    fn selection_bounds_user(f: &File, page: usize, items: &[usize]) -> [f32; 4] {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for o in f.objects(page).iter().filter(|o| items.contains(&o.index)) {
            for &(x, y) in &o.quad {
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
        b
    }

    pub fn copy_selection(&mut self) -> bool {
        let Sel::Objects { page, items } = self.sel.clone() else { return false };
        match self.isolate(page, &items) {
            Ok(bytes) => {
                let f = self.file.as_ref().unwrap();
                let bounds = Self::selection_bounds_user(f, page, &items);
                self.clip = Some(Clip { bytes, page, bounds, origin: (f.path.clone(), page), pastes: 0 });
                self.say(format!("Copied {}", crate::editor::plural(items.len(), "item", "items")));
                true
            }
            Err(e) => {
                self.error = Some(format!("This could not be copied. {e}"));
                false
            }
        }
    }

    pub fn cut_selection(&mut self) {
        if self.copy_selection() {
            self.delete_selection();
        }
    }

    pub fn can_paste(&self) -> bool {
        self.clip.is_some() && self.file.is_some()
    }

    /// Paste the copied items onto `page`: in the same place, a little offset when that place
    /// already shows the original, or centred when they would fall off the page.
    pub fn paste(&mut self, page: usize) {
        let Some(clip) = &mut self.clip else { return };
        let Some(f) = &self.file else { return };
        let Ok(src) = Doc::open(clip.bytes.clone(), None) else {
            self.error = Some("The copied items could not be read.".into());
            return;
        };
        let same_place = clip.origin == (f.path.clone(), page);
        if same_place {
            clip.pastes += 1;
        }
        let (mut dx, mut dy) = if same_place { (12.0 * clip.pastes as f32, -12.0 * clip.pastes as f32) } else { (0.0, 0.0) };
        let b = clip.bounds;
        if let Some(p) = f.doc.page(page) {
            let g = p.geom();
            drop(p);
            let (u0, u1) = (g.to_user(0.0, 0.0), g.to_user(g.w, f.sizes[page].1));
            let (pl, pb, pr, pt) = (u0.0.min(u1.0), u0.1.min(u1.1), u0.0.max(u1.0), u0.1.max(u1.1));
            let inside = b[0] + dx >= pl && b[2] + dx <= pr && b[1] + dy >= pb && b[3] + dy <= pt;
            if !inside {
                dx = (pl + pr) / 2.0 - (b[0] + b[2]) / 2.0;
                dy = (pb + pt) / 2.0 - (b[1] + b[3]) / 2.0;
            }
        }
        let src_page = clip.page;
        let added = self.apply(page, |p| p.insert_page_as_group(&src, src_page, [1.0, 0.0, 0.0, 1.0, dx, dy]));
        match added.flatten() {
            Some(i) => {
                // Pasted items stay editable: the group is opened back into page content.
                let items = self.ungroup(page, i).unwrap_or(vec![i]);
                self.sel = Sel::Objects { page, items };
                self.say("Pasted");
            }
            None => self.error = Some("The copied items could not be pasted here.".into()),
        }
    }

    pub fn duplicate_selection(&mut self) {
        let Some(page) = self.sel.page() else { return };
        let keep = self.clip.take();
        if self.copy_selection() {
            self.paste(page);
        }
        if keep.is_some() {
            self.clip = keep;
        }
    }

    // ── stacking order ──

    /// Move the selection in front of (or behind) everything else on its page.
    pub fn restack(&mut self, to_front: bool) {
        let Sel::Objects { page, items } = self.sel.clone() else { return };
        let bytes = match self.isolate(page, &items) {
            Ok(b) => b,
            Err(e) => {
                self.error = Some(format!("The order could not be changed. {e}"));
                return;
            }
        };
        let Ok(src) = Doc::open(bytes, None) else { return };
        // Remove the originals, draw the copy on top, and for "to the back" move it first.
        if !self.surgery(page, items.iter().map(|&i| (i, Edit::Delete)).collect(), Vec::new()) {
            return;
        }
        let Some(Some(mut i)) = self.apply(page, |p| p.insert_page_as_group(&src, page, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0])) else { return };
        if !to_front {
            if !self.surgery(page, vec![(i, Edit::ToStart)], Vec::new()) {
                return;
            }
            i = 0;
        }
        let items = self.ungroup(page, i).unwrap_or(vec![i]);
        self.sel = Sel::Objects { page, items };
        self.say(if to_front { "Brought to the front" } else { "Sent to the back" });
    }

    // ── groups ──

    /// Open a group into plain page content. Returns the indices of the objects it held.
    pub fn ungroup(&mut self, page: usize, obj: usize) -> Option<Vec<usize>> {
        let before = self.file.as_ref()?.objects(page).len();
        let count = |f: &File| f.doc.page(page).map(|p| p.object_count()).unwrap_or(0);
        let total_before = count(self.file.as_ref()?);
        let ok = self.surgery_with(page, |m| {
            let o = m.parsed.objs.get(obj).ok_or("The group was not found.")?;
            if o.kind != Kind::Group {
                return Err("This is not a group.".into());
            }
            let name = match m.parsed.ops[o.first].args.first() {
                Some(content::Val::Name(n)) => n.clone(),
                _ => return Err("The group could not be read.".into()),
            };
            let (bytes, extra) = crate::incremental::ungroup(&m.data, &name)?;
            Ok((vec![(obj, Edit::Inline(bytes))], Vec::new(), extra))
        });
        if !ok {
            return None;
        }
        let _ = before;
        let total_after = count(self.file.as_ref()?);
        // The group was one object; its contents take its place in the drawing order.
        let n = (total_after + 1).saturating_sub(total_before);
        Some((obj..obj + n).collect())
    }

    // ── rotation ──

    /// Rotate the selection by `degrees` (counter-clockwise in user space) about `center`.
    pub fn rotate_selection(&mut self, degrees: f64, center: (f64, f64)) {
        let Sel::Objects { page, items } = self.sel.clone() else { return };
        let (s, c) = degrees.to_radians().sin_cos();
        let (cx, cy) = center;
        let t = [c, s, -s, c, cx - (cx * c - cy * s), cy - (cx * s + cy * c)];
        if self.surgery(page, items.iter().map(|&i| (i, Edit::Transform(t))).collect(), Vec::new()) {
            self.sel = Sel::Objects { page, items };
        }
    }

    /// Rotate the selection a quarter turn about its centre (positive: counter-clockwise as seen).
    pub fn rotate_selection_quarter(&mut self, turns: i32) {
        let Sel::Objects { page, items } = self.sel.clone() else { return };
        let Some(f) = &self.file else { return };
        let b = Self::selection_bounds_user(f, page, &items);
        let Some(g) = f.geom(page) else { return };
        // A quarter turn on screen; the page's axes may be flipped relative to the view.
        let o = g.to_view(0.0, 0.0);
        let ex = g.to_view(1.0, 0.0);
        let ey = g.to_view(0.0, 1.0);
        let det = (ex.0 - o.0) * (ey.1 - o.1) - (ex.1 - o.1) * (ey.0 - o.0);
        let deg = 90.0 * turns as f64 * -(det.signum() as f64);
        self.rotate_selection(deg, (((b[0] + b[2]) / 2.0) as f64, ((b[1] + b[3]) / 2.0) as f64));
    }

    // ── paragraphs ──

    /// The paragraph that text object `obj` belongs to, found from line positions. None when the
    /// text is rotated or skewed (then only the single object is edited).
    pub fn paragraph_of(&self, page: usize, obj: usize) -> Option<Paragraph> {
        let f = self.file.as_ref()?;
        let p = f.doc.page(page)?;
        let texts = p.texts();
        let objs = f.objects(page);
        let mut runs: Vec<Run> = Vec::new();
        for o in objs.iter().filter(|o| o.kind == Kind::Text) {
            let m = p.matrix(o.index);
            if !upright(&m) {
                if o.index == obj {
                    return None;
                }
                continue;
            }
            let size = p.font_size(o.index) * m[3];
            let (x0, x1) = o.quad.iter().fold((f32::MAX, f32::MIN), |a, q| (a.0.min(q.0), a.1.max(q.0)));
            runs.push(Run { index: o.index, x0: m[4].min(x0), x1, base: m[5], size, text: texts.get(o.index).cloned().flatten().unwrap_or_default() });
        }
        let first_matrix = p.matrix(obj);
        let color = p.fill_color(obj).unwrap_or([0, 0, 0, 255]);
        let font_size = p.font_size(obj);
        drop(p);
        let me = runs.iter().find(|r| r.index == obj)?;
        let size = me.size.max(1.0);

        // Lines: runs on the same baseline with similar size, in left-to-right order.
        let same_line = |a: &Run, b: &Run| (a.base - b.base).abs() < size * 0.3 && (a.size / b.size).clamp(0.0, 10.0) > 0.7 && (a.size / b.size) < 1.4;
        let mut lines: Vec<Vec<&Run>> = Vec::new();
        for r in &runs {
            match lines.iter_mut().find(|l| same_line(l[0], r)) {
                Some(l) => l.push(r),
                None => lines.push(vec![r]),
            }
        }
        for l in &mut lines {
            l.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        }
        // Keep only the stretch of the clicked line that is continuous (no wide gaps).
        let split = |l: &Vec<&Run>| -> Vec<Vec<usize>> {
            let mut parts: Vec<Vec<usize>> = vec![vec![l[0].index]];
            for w in l.windows(2) {
                if w[1].x0 - w[0].x1 > size * 2.5 {
                    parts.push(Vec::new());
                }
                parts.last_mut().unwrap().push(w[1].index);
            }
            parts
        };
        let run = |i: usize| runs.iter().find(|r| r.index == i).unwrap();
        let mut segs: Vec<Vec<usize>> = lines.iter().flat_map(split).collect();
        // Top to bottom.
        segs.sort_by(|a, b| run(b[0]).base.total_cmp(&run(a[0]).base));
        let seg_of = segs.iter().position(|s| s.contains(&obj))?;
        let extent =
            |s: &Vec<usize>| (s.iter().map(|&i| run(i).x0).fold(f32::MAX, f32::min), s.iter().map(|&i| run(i).x1).fold(f32::MIN, f32::max), run(s[0]).base);
        let fits = |s: &Vec<usize>, prev: &Vec<usize>, spacing: Option<f32>, para_left: f32| -> bool {
            let (l, r, b) = extent(s);
            let (pl, pr, pb) = extent(prev);
            let gap = pb - b;
            let size_ok = s.iter().all(|&i| (run(i).size / size - 1.0).abs() < 0.15);
            let gap_ok = gap > size * 0.9 && gap < size * 2.0 && spacing.is_none_or(|sp| (gap - sp).abs() < sp * 0.2);
            let aligned = (l - para_left).abs() < size * 1.5 || (l - pl).abs() < size * 1.5;
            size_ok && gap_ok && aligned && r > pl && l < pr
        };
        // Grow upwards, then downwards.
        let mut top = seg_of;
        let mut spacing: Option<f32> = None;
        while top > 0 && fits(&segs[top], &segs[top - 1], spacing, extent(&segs[top]).0) {
            spacing.get_or_insert(extent(&segs[top - 1]).2 - extent(&segs[top]).2);
            top -= 1;
        }
        let mut bottom = seg_of;
        let para_left = extent(&segs[(top + 1).min(segs.len() - 1)]).0;
        while bottom + 1 < segs.len() && fits(&segs[bottom + 1], &segs[bottom], spacing, para_left) {
            spacing.get_or_insert(extent(&segs[bottom]).2 - extent(&segs[bottom + 1]).2);
            bottom += 1;
        }
        let chosen = &segs[top..=bottom];
        let objects: Vec<usize> = chosen.iter().flatten().copied().collect();
        // The text: runs joined (with a space where there is a gap), lines joined by spaces.
        let mut text = String::new();
        for (k, s) in chosen.iter().enumerate() {
            let mut prev_x1: Option<f32> = None;
            for &i in s {
                let r = run(i);
                if let Some(px) = prev_x1
                    && r.x0 - px > size * 0.15
                    && !text.ends_with(' ')
                {
                    text.push(' ');
                }
                text.push_str(r.text.trim_end_matches(['\r', '\n']));
                prev_x1 = Some(r.x1);
            }
            if k + 1 < chosen.len() {
                if text.ends_with('-') {
                    // A word broken over two lines.
                } else if !text.ends_with(' ') {
                    text.push(' ');
                }
            }
        }
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let first = &chosen[0];
        let (first_left, _, baseline) = extent(first);
        let left = if chosen.len() > 1 { chosen[1..].iter().map(|s| extent(s).0).fold(f32::MAX, f32::min) } else { first_left };
        let right = chosen.iter().map(|s| extent(s).1).fold(f32::MIN, f32::max);
        let spacing = spacing.unwrap_or(size * 1.25);
        let last_base = extent(chosen.last().unwrap()).2;
        let first_run = run(first[0]);
        let first_matrix = if first_run.index == obj { first_matrix } else { f.doc.page(page).map(|p| p.matrix(first_run.index)).unwrap_or(first_matrix) };
        let _ = font_size;
        Some(Paragraph {
            objects,
            text,
            rect: [first_left.min(left), last_base - size * 0.3, right, baseline + size * 0.9],
            first_matrix,
            size,
            color,
            first_left,
            left,
            baseline,
            spacing,
            width: right - first_left.min(left),
            lines: chosen.len(),
        })
    }

    /// Replace a paragraph with `text`, rewrapped to the paragraph's width. Keeps the first line's
    /// font when it can show every character; otherwise uses the closest installed font.
    pub fn commit_paragraph(&mut self, page: usize, para: &Paragraph, text: &str) {
        let text = text.trim_end();
        if text == para.text {
            return;
        }
        let Some(f) = &self.file else { return };
        let first = para.objects[0];
        let m = para.first_matrix;
        let sx = (m[0] * m[0] + m[1] * m[1]).sqrt().max(1e-6);
        let Ok(model) = PageModel::load(f, page) else {
            self.error = Some("This paragraph could not be changed.".into());
            return;
        };
        let Some(t) = model.parsed.objs.get(first).and_then(|o| o.text.clone()) else { return };
        let font_size = t.size as f32;
        let (tc, th) = (t.tc as f32, t.th as f32);
        let space_kern = if t.word_kern < 0.0 { t.word_kern } else { -333.0 };

        // First try the original font: every word must be encodable.
        let Some((learned, meta, kern)) = model.encoding(f, page, first) else { return };
        let enc = |s: &str| content::encode(s, &learned, &meta, kern);
        let encodable = text.split_whitespace().all(|w| enc(w).is_some());
        let width_in = |word: &str, p: &crate::pdf::Page| -> f32 { (p.text_width(first, word, font_size) + tc * word.chars().count() as f32) * th * sx };
        let space_w = if enc(" ").is_some_and(|pc| pc.iter().all(|x| matches!(x, Piece::Codes(_)))) {
            f.doc.page(page).map(|p| width_in(" ", &p)).unwrap_or(font_size * 0.3 * sx)
        } else {
            -space_kern as f32 / 1000.0 * font_size * th * sx
        };

        if encodable {
            let Some(p) = f.doc.page(page) else { return };
            let lines = wrap(text, para.width, para.first_left - para.left, space_w, |w| width_in(w, &p));
            drop(p);
            let mut out = Vec::new();
            for (k, line) in lines.iter().enumerate() {
                let x = if k == 0 { para.first_left } else { para.left };
                let y = para.baseline - k as f32 * para.spacing;
                let Some(pieces) = enc(line) else { return self.fallback_paragraph(page, para, text) };
                out.push(([m[0] as f64, m[1] as f64, m[2] as f64, m[3] as f64, x as f64, y as f64], pieces));
            }
            let mut edits: Vec<(usize, Edit)> = vec![(first, Edit::Lines(out))];
            edits.extend(para.objects.iter().skip(1).map(|&i| (i, Edit::Delete)));
            drop(model);
            let grew = lines.len() > para.lines;
            if self.surgery(page, edits, Vec::new()) {
                self.sel = Sel::None;
                if grew {
                    self.say("The paragraph is now longer than before and may overlap what follows it.");
                }
            }
        } else {
            drop(model);
            self.fallback_paragraph(page, para, text);
        }
    }

    /// Redraw a paragraph in the closest installed font.
    fn fallback_paragraph(&mut self, page: usize, para: &Paragraph, text: &str) {
        let Some(f) = &self.file else { return };
        let info = f.doc.page(page).and_then(|p| p.font(para.objects[0]));
        let Some(path) = info.as_ref().and_then(fonts::closest).or_else(|| fonts::generic("sans", false, false)) else {
            self.error = Some("No installed font can show this text.".into());
            return;
        };
        if !self.surgery(page, para.objects.iter().map(|&i| (i, Edit::Delete)).collect(), Vec::new()) {
            return;
        }
        let Some(font) = self.file.as_ref().and_then(|f| f.doc.truetype_font(path)) else { return };
        let m = para.first_matrix;
        let sx = (m[0] * m[0] + m[1] * m[1]).sqrt().max(1e-6);
        let size = para.size / m[3].max(1e-6);
        let measure = |w: &str| Doc::font_text_width(font, w, size) * sx;
        let lines = wrap(text, para.width, para.first_left - para.left, measure(" "), measure);
        let color = para.color;
        let added = self.apply(page, |p| {
            let mut ix = Vec::new();
            for (k, line) in lines.iter().enumerate() {
                let x = if k == 0 { para.first_left } else { para.left };
                let y = para.baseline - k as f32 * para.spacing;
                if let Some(i) = p.add_text(font, size, line, [m[0], m[1], m[2], m[3], x, y], color) {
                    ix.push(i);
                }
            }
            ix
        });
        if let Some(items) = added.filter(|v| !v.is_empty()) {
            self.sel = Sel::Objects { page, items };
        }
        let from = info.as_ref().map(|i| i.clean_name().to_string()).unwrap_or_else(|| "The original font".into());
        self.say(format!("{from} cannot show this text, so the paragraph now uses {}.", path.file_stem().unwrap_or_default().to_string_lossy()));
    }
}

/// Greedy word wrap to `width`. `indent` is how far the first line starts right of the others.
/// Line breaks in the text are kept.
fn wrap(text: &str, width: f32, indent: f32, space: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for (pi, para) in text.split('\n').enumerate() {
        let mut line = String::new();
        let mut w = 0.0;
        let mut limit = if pi == 0 { width - indent.max(0.0) } else { width };
        for word in para.split_whitespace() {
            let ww = measure(word);
            if !line.is_empty() && w + space + ww > limit + 0.5 {
                lines.push(std::mem::take(&mut line));
                w = 0.0;
                limit = width;
            }
            if !line.is_empty() {
                line.push(' ');
                w += space;
            }
            line.push_str(word);
            w += ww;
        }
        lines.push(line);
    }
    lines
}
