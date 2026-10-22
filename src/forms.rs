//! Form automation: finding where a form's fields should go, and field rules (numbers, allowed
//! ranges, required fields and calculated values).
//!
//! Rules are written as the standard Acrobat form scripts (AFNumber_Format, AFRange_Validate,
//! AFSimple_Calculate), so every reader that runs form scripts honours them. Dockit reads them
//! back from those scripts and enforces them itself when a form is filled in here.

use crate::annots::{NewField, Update, text_string};
use crate::editor::{Editor, plural};
use crate::pdf::{self, Kind};
use lopdf::{Dictionary, Object};

// ── finding fields ──

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub page: usize,
    pub kind: NewField,
    /// l, b, r, t
    pub rect: [f32; 4],
    pub name: String,
}

fn overlap(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// The label for a field: the words just left of it on its line (up to a big gap), without a
/// trailing colon.
fn label_for(chars: &[pdf::Char], rect: [f32; 4]) -> String {
    let mid = (rect[1] + rect[3]) / 2.0;
    let mut row: Vec<&pdf::Char> = chars
        .iter()
        .filter(|c| c.rect[2] > c.rect[0] && c.rect[2] <= rect[0] + 1.0 && c.rect[2] > rect[0] - 260.0 && (c.rect[1] + c.rect[3]) / 2.0 > mid - 9.0 && (c.rect[1] + c.rect[3]) / 2.0 < mid + 9.0)
        .collect();
    row.sort_by(|a, b| b.rect[0].total_cmp(&a.rect[0]));
    let mut out = Vec::new();
    let mut edge = rect[0];
    for c in row {
        if edge - c.rect[2] > 18.0 {
            break;
        }
        out.push(c.ch);
        edge = c.rect[0];
    }
    out.reverse();
    let s: String = out.into_iter().collect();
    s.trim().trim_end_matches(':').trim().chars().filter(|c| !c.is_control()).collect()
}

impl Editor {
    /// Where fields seem to belong: rows of underscores, empty boxes and fill-in lines.
    pub fn find_fields(&self) -> Vec<Found> {
        let Some(f) = &self.file else { return Vec::new() };
        let mut out: Vec<Found> = Vec::new();
        for page in 0..f.page_count() {
            let Some(p) = f.doc.page(page) else { continue };
            let chars = p.chars();
            let existing: Vec<[f32; 4]> = f.annots(page).iter().filter(|a| a.subtype == pdf::ANNOT_WIDGET).map(|a| a.rect).collect();
            let has_text = |r: [f32; 4]| chars.iter().any(|c| c.ch != '_' && !c.ch.is_whitespace() && overlap(c.rect, r));
            let add = |kind: NewField, rect: [f32; 4], out: &mut Vec<Found>| {
                if existing.iter().any(|e| overlap(*e, rect)) || out.iter().any(|o| o.page == page && overlap(o.rect, rect)) {
                    return;
                }
                out.push(Found { page, kind, rect, name: label_for(&chars, rect) });
            };
            // Rows of underscores: "Name: ________".
            let mut k = 0;
            while k < chars.len() {
                if chars[k].ch != '_' {
                    k += 1;
                    continue;
                }
                let start = k;
                while k < chars.len() && chars[k].ch == '_' && chars[k].obj == chars[start].obj {
                    k += 1;
                }
                if k - start >= 4 {
                    let (x0, x1) = (chars[start].rect[0], chars[k - 1].rect[2]);
                    let base = chars[start..k].iter().map(|c| c.rect[1]).fold(f32::MAX, f32::min);
                    add(NewField::Text, [x0, base - 1.0, x1, base + 15.0], &mut out);
                }
            }
            // Empty boxes and lines drawn as shapes.
            for o in f.objects(page).iter().filter(|o| o.kind == Kind::Path) {
                let pts = p.path_points(o.index);
                let corners: Vec<(f32, f32)> = pts.iter().filter(|q| q.2 != 1).map(|q| (q.0, q.1)).collect();
                let xs = || corners.iter().map(|c| c.0);
                let ys = || corners.iter().map(|c| c.1);
                if corners.len() < 2 || pts.iter().any(|q| q.2 == 1) {
                    continue;
                }
                let r = [xs().fold(f32::MAX, f32::min), ys().fold(f32::MAX, f32::min), xs().fold(f32::MIN, f32::max), ys().fold(f32::MIN, f32::max)];
                let (w, h) = (r[2] - r[0], r[3] - r[1]);
                let moves = pts.iter().filter(|q| q.2 == 2).count();
                let axis = corners.windows(2).all(|s| (s[0].0 - s[1].0).abs() < 0.6 || (s[0].1 - s[1].1).abs() < 0.6);
                let closed = pts.last().is_some_and(|q| q.3) || corners.first().zip(corners.last()).is_some_and(|(a, b)| (a.0 - b.0).abs() < 0.6 && (a.1 - b.1).abs() < 0.6);
                let (filled, _) = p.path_mode(o.index);
                let white = !filled || p.fill_color(o.index).is_some_and(|c| c[0] > 240 && c[1] > 240 && c[2] > 240);
                // One rectangle, outline only.
                if moves == 1 && axis && closed && (4..=5).contains(&corners.len()) && white && !has_text(r) {
                    if (6.0..=24.0).contains(&w) && (6.0..=24.0).contains(&h) && (w - h).abs() < 4.0 {
                        add(NewField::Checkbox, r, &mut out);
                    } else if w >= 40.0 && (10.0..=80.0).contains(&h) {
                        add(NewField::Text, [r[0] + 1.0, r[1] + 1.0, r[2] - 1.0, r[3] - 1.0], &mut out);
                    }
                }
                // A level line to write on, with nothing written on it already.
                if moves == 1 && corners.len() == 2 && h < 1.0 && w >= 50.0 {
                    let above = [r[0], r[1], r[2], r[1] + 14.0];
                    if !has_text(above) {
                        add(NewField::Text, [r[0], r[1] + 1.0, r[2], r[1] + 17.0], &mut out);
                    }
                }
            }
        }
        out
    }

    /// Add the found fields, named after their labels.
    pub fn add_found_fields(&mut self, found: &[Found]) -> usize {
        let mut pages: Vec<usize> = found.iter().map(|f| f.page).collect();
        pages.dedup();
        self.file_update(&pages, |u| {
            let mut names = u.field_names();
            for f in found {
                let base = if f.name.is_empty() {
                    match f.kind {
                        NewField::Checkbox => "Checkbox".to_string(),
                        _ => "Text".to_string(),
                    }
                } else {
                    f.name.clone()
                };
                let name = if names.contains(&base) { (2..).map(|k| format!("{base} {k}")).find(|n| !names.contains(n)).unwrap() } else { base };
                u.add_field(f.page, f.kind, f.rect, &name)?;
                names.push(name);
            }
            Ok(found.len())
        })
        .unwrap_or(0)
    }
}

// ── rules ──

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Calc {
    Sum,
    Product,
    Average,
    Min,
    Max,
}

impl Calc {
    pub const ALL: [Calc; 5] = [Calc::Sum, Calc::Product, Calc::Average, Calc::Min, Calc::Max];

    fn code(self) -> &'static str {
        match self {
            Calc::Sum => "SUM",
            Calc::Product => "PRD",
            Calc::Average => "AVG",
            Calc::Min => "MIN",
            Calc::Max => "MAX",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Calc::Sum => "Sum",
            Calc::Product => "Product",
            Calc::Average => "Average",
            Calc::Min => "Smallest",
            Calc::Max => "Largest",
        }
    }

    fn apply(self, v: &[f64]) -> f64 {
        match self {
            Calc::Sum => v.iter().sum(),
            Calc::Product => v.iter().product(),
            Calc::Average => {
                if v.is_empty() {
                    0.0
                } else {
                    v.iter().sum::<f64>() / v.len() as f64
                }
            }
            Calc::Min => v.iter().copied().fold(f64::INFINITY, f64::min).min(if v.is_empty() { 0.0 } else { f64::INFINITY }),
            Calc::Max => v.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(if v.is_empty() { 0.0 } else { f64::NEG_INFINITY }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rules {
    /// A number, with this many decimals.
    pub number: Option<u32>,
    pub range: Option<(f64, f64)>,
    pub required: bool,
    pub calc: Option<(Calc, Vec<String>)>,
}

/// The arguments inside `name(...)` in a script.
fn args<'a>(js: &'a str, name: &str) -> Option<&'a str> {
    let at = js.find(name)? + name.len();
    let rest = js[at..].trim_start().strip_prefix('(')?;
    let mut depth = 1;
    for (k, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..k]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Field names inside AFSimple_Calculate's second argument: `new Array("a", "b")` or `"a, b"`.
fn names_in(s: &str) -> Vec<String> {
    let quoted: Vec<String> = s.split('"').skip(1).step_by(2).map(str::to_string).collect();
    if quoted.len() == 1 && quoted[0].contains(',') {
        return quoted[0].split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
    }
    quoted
}

impl Rules {
    pub fn read(f: &pdf::Field) -> Rules {
        let mut r = Rules { required: f.flags & 2 != 0, ..Default::default() };
        if let Some(a) = args(&f.format_js, "AFNumber_Format").or(args(&f.keystroke_js, "AFNumber_Keystroke")) {
            r.number = a.split(',').next().and_then(|d| d.trim().parse().ok());
        }
        if let Some(a) = args(&f.validate_js, "AFRange_Validate") {
            let p: Vec<&str> = a.split(',').map(str::trim).collect();
            if p.len() == 4 {
                let lo = if p[0] == "true" { p[1].parse().unwrap_or(f64::MIN) } else { f64::MIN };
                let hi = if p[2] == "true" { p[3].parse().unwrap_or(f64::MAX) } else { f64::MAX };
                r.range = Some((lo, hi));
            }
        }
        if let Some(a) = args(&f.calc_js, "AFSimple_Calculate") {
            let op = a.split('"').nth(1).unwrap_or("");
            let calc = Calc::ALL.into_iter().find(|c| c.code() == op);
            let rest = a.split_once(',').map(|x| x.1).unwrap_or("");
            if let Some(c) = calc {
                r.calc = Some((c, names_in(rest)));
            }
        }
        r
    }

    /// Check a typed value. Returns the value as it should be shown, or why it is not allowed.
    pub fn check(&self, label: &str, text: &str) -> Result<String, String> {
        let t = text.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        let Some(dec) = self.number else { return Ok(text.to_string()) };
        let v: f64 = t.replace(',', "").parse().map_err(|_| format!("{label} must be a number."))?;
        if let Some((lo, hi)) = self.range
            && (v < lo || v > hi)
        {
            let show = |x: f64| if x <= f64::MIN || x >= f64::MAX { String::new() } else { format!("{x}") };
            return Err(match (show(lo), show(hi)) {
                (a, b) if !a.is_empty() && !b.is_empty() => format!("{label} must be between {a} and {b}."),
                (a, _) if !a.is_empty() => format!("{label} must be at least {a}."),
                (_, b) => format!("{label} must be at most {b}."),
            });
        }
        Ok(format!("{v:.prec$}", prec = dec as usize))
    }
}

fn js_action(js: String) -> Object {
    let mut d = Dictionary::new();
    d.set("S", Object::Name(b"JavaScript".to_vec()));
    d.set("JS", text_string(&js));
    Object::Dictionary(d)
}

impl Update {
    /// Write a field's rules as its scripts and flags.
    pub fn set_rules(&mut self, page: usize, index: usize, r: &Rules) -> Result<(), String> {
        let id = self.annot_id(page, index)?;
        let mut d = self.dict(id)?;
        // The field may be the widget's parent.
        let field_id = match d.get(b"Parent") {
            Ok(Object::Reference(pid)) if !d.has(b"T") => *pid,
            _ => id,
        };
        if field_id != id {
            d = self.dict(field_id)?;
        }
        let mut aa = match d.get(b"AA") {
            Ok(Object::Dictionary(a)) => a.clone(),
            Ok(Object::Reference(r)) => self.dict(*r).unwrap_or_default(),
            _ => Dictionary::new(),
        };
        for k in [&b"K"[..], b"F", b"V", b"C"] {
            aa.remove(k);
        }
        if let Some(dec) = r.number {
            aa.set("K", js_action(format!("AFNumber_Keystroke({dec}, 0, 0, 0, \"\", true);")));
            aa.set("F", js_action(format!("AFNumber_Format({dec}, 0, 0, 0, \"\", true);")));
        }
        if let Some((lo, hi)) = r.range {
            let side = |v: f64, edge: bool| if edge { ("false".to_string(), "0".to_string()) } else { ("true".to_string(), format!("{v}")) };
            let (a, b) = side(lo, lo <= f64::MIN);
            let (c, e) = side(hi, hi >= f64::MAX);
            aa.set("V", js_action(format!("AFRange_Validate({a}, {b}, {c}, {e});")));
        }
        if let Some((op, names)) = &r.calc {
            let list = names.iter().map(|n| format!("\"{}\"", n.replace('"', ""))).collect::<Vec<_>>().join(", ");
            aa.set("C", js_action(format!("AFSimple_Calculate(\"{}\", new Array({list}));", op.code())));
        }
        if aa.is_empty() {
            d.remove(b"AA");
        } else {
            d.set("AA", Object::Dictionary(aa));
        }
        let ff = d.get(b"Ff").and_then(Object::as_i64).unwrap_or(0);
        d.set("Ff", Object::Integer(if r.required { ff | 2 } else { ff & !2 }));
        self.put(field_id, Object::Dictionary(d));
        // Calculated fields are listed, in order, in the form's /CO.
        let root_id = self.doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|_| "The document catalog is missing.".to_string())?;
        let cat = self.dict(root_id)?;
        let (form_id, mut form) = match cat.get(b"AcroForm") {
            Ok(Object::Reference(fid)) => (*fid, self.dict(*fid)?),
            _ => return Ok(()),
        };
        let mut co = match form.get(b"CO") {
            Ok(Object::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        co.retain(|o| o.as_reference().ok() != Some(field_id));
        if r.calc.is_some() {
            co.push(Object::Reference(field_id));
        }
        if co.is_empty() {
            form.remove(b"CO");
        } else {
            form.set("CO", Object::Array(co));
        }
        self.put(form_id, Object::Dictionary(form));
        Ok(())
    }
}

impl Editor {
    /// Fill in every calculated field from the fields it is worked out from.
    pub fn recalculate(&mut self) {
        for _ in 0..3 {
            let Some(f) = &self.file else { return };
            let mut values: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
            let mut calcs = Vec::new();
            for page in 0..f.page_count() {
                for a in f.annots(page).iter() {
                    let Some(fd) = &a.field else { continue };
                    values.insert(fd.name.clone(), fd.value.trim().replace(',', "").parse().unwrap_or(0.0));
                    let r = Rules::read(fd);
                    if let Some((op, names)) = r.calc {
                        calcs.push((page, a.index, fd.value.clone(), op, names, r.number));
                    }
                }
            }
            let mut changed = false;
            for (page, index, old, op, names, dec) in calcs {
                let v: Vec<f64> = names.iter().filter_map(|n| values.get(n).copied()).collect();
                let result = op.apply(&v);
                let text = match dec {
                    Some(d) => format!("{result:.prec$}", prec = d as usize),
                    None => {
                        let s = format!("{result:.6}");
                        s.trim_end_matches('0').trim_end_matches('.').to_string()
                    }
                };
                if text != old {
                    self.apply(page, |p| p.set_field_text(index, &text));
                    changed = true;
                }
            }
            if !changed {
                return;
            }
        }
    }

    /// Required fields still empty: their names.
    pub fn empty_required(&self) -> Vec<String> {
        let Some(f) = &self.file else { return Vec::new() };
        (0..f.page_count())
            .flat_map(|page| {
                f.annots(page)
                    .iter()
                    .filter_map(|a| a.field.as_ref())
                    .filter(|fd| fd.flags & 2 != 0 && fd.value.trim().is_empty() && !fd.checked && fd.kind != pdf::FIELD_PUSHBUTTON)
                    .map(|fd| fd.name.clone())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub fn set_field_rules(&mut self, page: usize, index: usize, r: &Rules) {
        if self.file_update(&[page], |u| u.set_rules(page, index, r)).is_some() {
            self.recalculate();
            self.say("Field rules saved");
        }
    }

    /// Find fields and add them, saying how many.
    pub fn detect_fields(&mut self) {
        let found = self.find_fields();
        if found.is_empty() {
            self.say("No places for fields were found. Draw them with the Form field tool (F).");
            return;
        }
        let n = self.add_found_fields(&found);
        if n > 0 {
            self.tool = crate::editor::Tool::Field;
            self.goto = Some(found[0].page);
            self.say(format!("Added {}. Check them with the Form field tool: rename, move or delete any that are wrong.", plural(n, "field", "fields")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_from_scripts() {
        let f = pdf::Field {
            name: "Total".into(),
            flags: 2,
            format_js: "AFNumber_Format(2, 0, 0, 0, \"\", true);".into(),
            validate_js: "AFRange_Validate(true, 0, false, 0);".into(),
            calc_js: "AFSimple_Calculate(\"SUM\", new Array(\"A\", \"B\"));".into(),
            ..Default::default()
        };
        let r = Rules::read(&f);
        assert_eq!(r.number, Some(2));
        assert_eq!(r.range, Some((0.0, f64::MAX)));
        assert!(r.required);
        assert_eq!(r.calc, Some((Calc::Sum, vec!["A".to_string(), "B".to_string()])));
        assert_eq!(r.check("Total", "12.5").unwrap(), "12.50");
        assert!(r.check("Total", "-1").is_err());
        assert!(r.check("Total", "abc").is_err());
        assert_eq!(names_in("\"Price, Qty\""), vec!["Price".to_string(), "Qty".to_string()]);
    }
}

#[cfg(test)]
mod form_tests {
    use super::*;
    use crate::pdf::{Doc, PathStyle, Seg};

    /// Needs pdfium.dll in target/debug (run with --ignored).
    #[test]
    #[ignore]
    fn finds_fields_and_calculates() {
        crate::pdf::init(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug").as_path()).unwrap();
        let doc = Doc::new_empty();
        assert!(doc.insert_blank(0, 400.0, 400.0));
        {
            let p = doc.page(0).unwrap();
            let font = doc.standard_font("Helvetica");
            p.add_text(font, 12.0, "Name: ____________", [1.0, 0.0, 0.0, 1.0, 40.0, 340.0], [0, 0, 0, 255]).unwrap();
            p.add_text(font, 12.0, "Agree", [1.0, 0.0, 0.0, 1.0, 62.0, 290.0], [0, 0, 0, 255]).unwrap();
            let bx = vec![Seg::Move(40.0, 288.0), Seg::Line(52.0, 288.0), Seg::Line(52.0, 300.0), Seg::Line(40.0, 300.0), Seg::Close];
            p.add_path(&bx, PathStyle { fill: None, stroke: Some([0, 0, 0, 255]), width: 1.0 }).unwrap();
            p.add_text(font, 12.0, "Signed", [1.0, 0.0, 0.0, 1.0, 40.0, 240.0], [0, 0, 0, 255]).unwrap();
            p.add_path(&[Seg::Move(90.0, 238.0), Seg::Line(300.0, 238.0)], PathStyle { fill: None, stroke: Some([0, 0, 0, 255]), width: 0.8 }).unwrap();
        }
        let mut ed = Editor::default();
        ed.open_untitled(doc);
        let found = ed.find_fields();
        println!("{found:?}");
        assert!(found.iter().any(|f| f.kind == NewField::Text && f.name == "Name"));
        assert!(found.iter().any(|f| f.kind == NewField::Checkbox && f.name.is_empty() || f.kind == NewField::Checkbox));
        assert!(found.iter().any(|f| f.kind == NewField::Text && f.name == "Signed"));
        assert_eq!(ed.add_found_fields(&found), found.len());

        // Calculated total.
        ed.style.field = NewField::Text;
        for (k, _) in ["A", "B", "Total"].iter().enumerate() {
            ed.add_field(0, [200.0, 100.0 + k as f32 * 30.0, 300.0, 120.0 + k as f32 * 30.0]);
        }
        let annots = ed.file.as_ref().unwrap().annots(0);
        let by_rect = |y: f32| annots.iter().find(|a| (a.rect[1] - y).abs() < 1.0).map(|a| a.index).unwrap();
        let (a, b, t) = (by_rect(100.0), by_rect(130.0), by_rect(160.0));
        let name = |i: usize| annots.iter().find(|x| x.index == i).unwrap().field.as_ref().unwrap().name.clone();
        let rules = Rules { number: Some(2), range: None, required: false, calc: Some((Calc::Sum, vec![name(a), name(b)])) };
        ed.set_field_rules(0, t, &rules);
        let num = Rules { number: Some(0), range: Some((0.0, 10.0)), required: true, calc: None };
        ed.set_field_rules(0, a, &num);
        ed.set_field_text(0, a, "2");
        ed.set_field_text(0, b, "3");
        let total = ed.file.as_ref().unwrap().annots(0).iter().find(|x| x.index == t).unwrap().field.clone().unwrap();
        assert_eq!(total.value, "5.00");
        ed.set_field_text(0, a, "99");
        assert!(ed.error.take().is_some(), "out of range");
        assert_eq!(Rules::read(ed.file.as_ref().unwrap().annots(0).iter().find(|x| x.index == a).unwrap().field.as_ref().unwrap()), num);
    }
}
