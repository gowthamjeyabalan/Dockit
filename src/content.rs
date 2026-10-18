//! Page content streams, edited with surgery rather than regeneration.
//!
//! PDFium can rewrite a page's content from its object list, but that rewrite loses things real
//! files use (CMYK and other colour spaces, some font set-ups, optional content). So edits to
//! existing objects are made here instead: the content is tokenized with byte positions, each
//! drawing command is matched to the PDFium page object it produces, and only the commands of the
//! edited objects are rewritten. Every other byte is copied unchanged.

use crate::pdf::Kind;
use std::collections::HashMap;

/// An affine matrix [a b c d e f] in PDF's row-vector convention (p' = p × M).
pub type M = [f64; 6];

pub const IDENTITY: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` then `b`.
pub fn mul(a: &M, b: &M) -> M {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

pub fn inv(m: &M) -> M {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return IDENTITY;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    [a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)]
}

pub fn apply(m: &M, x: f64, y: f64) -> (f64, f64) {
    (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
}

// ── tokens ──

#[derive(Clone, Debug)]
pub enum Val {
    Num(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Arr(Vec<Val>),
    Dict,
    Other,
}

impl Val {
    fn num(&self) -> Option<f64> {
        match self {
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }
}

/// One operator with its operands. `start..end` covers the operands and the operator.
#[derive(Clone, Debug)]
pub struct Op {
    pub name: Vec<u8>,
    pub args: Vec<Val>,
    pub start: usize,
    pub end: usize,
}

fn is_white(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delim(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

enum Tok {
    Val(Val),
    Keyword(Vec<u8>),
    ArrEnd,
    DictEnd,
}

impl<'a> Lexer<'a> {
    fn skip_space(&mut self) {
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if is_white(c) {
                self.i += 1;
            } else if c == b'%' {
                while self.i < self.s.len() && !matches!(self.s[self.i], b'\n' | b'\r') {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    fn regular(&mut self) -> &'a [u8] {
        let st = self.i;
        while self.i < self.s.len() && !is_white(self.s[self.i]) && !is_delim(self.s[self.i]) {
            self.i += 1;
        }
        &self.s[st..self.i]
    }

    fn literal(&mut self) -> Vec<u8> {
        // At '('.
        self.i += 1;
        let mut out = Vec::new();
        let mut depth = 1;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            self.i += 1;
            match c {
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else { break };
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.s.get(self.i) == Some(&b'\n') {
                                self.i += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.s.get(self.i) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.i += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out
    }

    fn hex(&mut self) -> Vec<u8> {
        // At '<'.
        self.i += 1;
        let mut digits = Vec::new();
        while self.i < self.s.len() && self.s[self.i] != b'>' {
            let c = self.s[self.i];
            if c.is_ascii_hexdigit() {
                digits.push(c);
            }
            self.i += 1;
        }
        self.i += 1;
        if digits.len() % 2 == 1 {
            digits.push(b'0');
        }
        digits.chunks(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap_or("0"), 16).unwrap_or(0)).collect()
    }

    fn token(&mut self) -> Option<Tok> {
        self.skip_space();
        let c = *self.s.get(self.i)?;
        Some(match c {
            b'(' => Tok::Val(Val::Str(self.literal())),
            b'<' if self.s.get(self.i + 1) == Some(&b'<') => {
                self.i += 2;
                // Skip the dictionary's contents (only marked-content properties appear inline).
                loop {
                    match self.token() {
                        None | Some(Tok::DictEnd) => break,
                        _ => {}
                    }
                }
                Tok::Val(Val::Dict)
            }
            b'<' => Tok::Val(Val::Str(self.hex())),
            b'>' if self.s.get(self.i + 1) == Some(&b'>') => {
                self.i += 2;
                Tok::DictEnd
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    match self.token() {
                        None | Some(Tok::ArrEnd) => break,
                        Some(Tok::Val(v)) => items.push(v),
                        Some(Tok::Keyword(_)) => items.push(Val::Other),
                        Some(Tok::DictEnd) => {}
                    }
                }
                Tok::Val(Val::Arr(items))
            }
            b']' => {
                self.i += 1;
                Tok::ArrEnd
            }
            b'/' => {
                self.i += 1;
                let raw = self.regular();
                let mut name = Vec::with_capacity(raw.len());
                let mut k = 0;
                while k < raw.len() {
                    if raw[k] == b'#'
                        && k + 2 < raw.len()
                        && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&raw[k + 1..k + 3]).unwrap_or("x"), 16)
                    {
                        name.push(v);
                        k += 3;
                        continue;
                    }
                    name.push(raw[k]);
                    k += 1;
                }
                Tok::Val(Val::Name(name))
            }
            b')' | b'>' | b'{' | b'}' => {
                self.i += 1;
                Tok::Val(Val::Other)
            }
            _ => {
                let w = self.regular();
                if w.is_empty() {
                    self.i += 1;
                    return Some(Tok::Val(Val::Other));
                }
                let first = w[0];
                if first.is_ascii_digit() || matches!(first, b'+' | b'-' | b'.') {
                    match std::str::from_utf8(w).ok().and_then(|t| t.parse::<f64>().ok()) {
                        Some(n) => Tok::Val(Val::Num(n)),
                        None => Tok::Val(Val::Num(0.0)),
                    }
                } else if matches!(w, b"true" | b"false" | b"null") {
                    Tok::Val(Val::Other)
                } else {
                    Tok::Keyword(w.to_vec())
                }
            }
        })
    }

    /// After `ID`, skip inline image data up to the `EI` that ends it.
    fn skip_inline_data(&mut self) {
        // One white-space byte follows ID.
        self.i += 1;
        let s = self.s;
        while self.i + 1 < s.len() {
            if s[self.i] == b'E'
                && s[self.i + 1] == b'I'
                && self.i > 0
                && is_white(s[self.i - 1])
                && s.get(self.i + 2).is_none_or(|&c| is_white(c) || is_delim(c))
            {
                self.i += 2;
                return;
            }
            self.i += 1;
        }
        self.i = s.len();
    }
}

pub fn tokenize(s: &[u8]) -> Vec<Op> {
    let mut lx = Lexer { s, i: 0 };
    let mut ops = Vec::new();
    let mut args: Vec<Val> = Vec::new();
    let mut start: Option<usize> = None;
    loop {
        lx.skip_space();
        let at = lx.i;
        let Some(t) = lx.token() else { break };
        match t {
            Tok::Val(v) => {
                start.get_or_insert(at);
                args.push(v);
            }
            Tok::Keyword(k) => {
                let st = start.take().unwrap_or(at);
                if k == b"BI" {
                    // Inline image: BI <key value>... ID <data> EI.
                    loop {
                        lx.skip_space();
                        match lx.token() {
                            None => break,
                            Some(Tok::Keyword(w)) if w == b"ID" => {
                                lx.skip_inline_data();
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                ops.push(Op { name: k, args: std::mem::take(&mut args), start: st, end: lx.i });
            }
            Tok::ArrEnd | Tok::DictEnd => {}
        }
    }
    ops
}

// ── interpretation ──

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XKind {
    Image,
    Form,
    Other,
}

/// What a font resource needs for encoding text.
#[derive(Clone, Debug, Default)]
pub struct FontMeta {
    /// Type0 fonts use two-byte codes (Identity and the common CJK CMaps).
    pub two_byte: bool,
    /// A standard, non-embedded font with WinAnsi encoding: text can be encoded from cp1252.
    pub winansi: bool,
}

#[derive(Clone, Debug)]
pub struct TextInfo {
    /// Line matrix at the show operator (after any T* that ' and " do).
    pub tlm: M,
    pub font: Vec<u8>,
    pub size: f64,
    /// Horizontal scaling (Tz / 100).
    pub th: f64,
    pub rise: f64,
    /// The next text object drawn straight after this one, with no positioning in between.
    pub next_cont: Option<usize>,
    /// Character codes shown, all strings joined.
    pub codes: Vec<u8>,
    /// The typical TJ adjustment between words, for fonts that draw no space characters.
    pub word_kern: f64,
}

#[derive(Clone, Debug)]
pub struct ObjOps {
    pub kind: Kind,
    pub first: usize,
    pub last: usize,
    pub ctm: M,
    pub clip: bool,
    pub fill_state: Vec<u8>,
    pub stroke_state: Vec<u8>,
    pub text: Option<TextInfo>,
}

pub struct Parsed {
    pub ops: Vec<Op>,
    pub objs: Vec<ObjOps>,
}

#[derive(Clone)]
struct State {
    ctm: M,
    fill: Vec<u8>,
    stroke: Vec<u8>,
    font: Option<Vec<u8>>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
}

pub fn parse(src: &[u8], xobjects: &HashMap<Vec<u8>, XKind>) -> Parsed {
    let ops = tokenize(src);
    let mut objs: Vec<ObjOps> = Vec::new();
    let mut st = State { ctm: IDENTITY, fill: Vec::new(), stroke: Vec::new(), font: None, size: 0.0, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, rise: 0.0 };
    let mut stack: Vec<State> = Vec::new();
    let mut tlm = IDENTITY;
    // Set after a show operator until the next positioning operator.
    let mut moved_by_show = false;
    let mut last_text_obj: Option<usize> = None;
    let mut path_start: Option<usize> = None;
    let mut clip = false;
    let mut last_cs: Vec<u8> = Vec::new();
    let mut last_cs_stroke: Vec<u8> = Vec::new();
    let raw = |op: &Op| src[op.start..op.end].to_vec();

    for (i, op) in ops.iter().enumerate() {
        let n = |k: usize| op.args.get(k).and_then(Val::num).unwrap_or(0.0);
        let six = || -> M { [n(0), n(1), n(2), n(3), n(4), n(5)] };
        match op.name.as_slice() {
            b"q" => stack.push(st.clone()),
            b"Q" => {
                if let Some(s) = stack.pop() {
                    st = s;
                }
            }
            b"cm" => st.ctm = mul(&six(), &st.ctm),
            b"BT" => {
                tlm = IDENTITY;
                moved_by_show = false;
                last_text_obj = None;
            }
            b"Tf" => {
                st.font = match op.args.first() {
                    Some(Val::Name(nm)) => Some(nm.clone()),
                    _ => st.font.clone(),
                };
                st.size = n(1);
            }
            b"Tc" => st.tc = n(0),
            b"Tw" => st.tw = n(0),
            b"Tz" => st.th = n(0) / 100.0,
            b"TL" => st.tl = n(0),
            b"Ts" => st.rise = n(0),
            b"Td" | b"TD" => {
                if op.name == b"TD" {
                    st.tl = -n(1);
                }
                tlm = mul(&[1.0, 0.0, 0.0, 1.0, n(0), n(1)], &tlm);
                moved_by_show = false;
            }
            b"Tm" => {
                tlm = six();
                moved_by_show = false;
            }
            b"T*" => {
                tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -st.tl], &tlm);
                moved_by_show = false;
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                if op.name == b"\"" {
                    st.tw = n(0);
                    st.tc = n(1);
                }
                if matches!(op.name.as_slice(), b"'" | b"\"") {
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -st.tl], &tlm);
                    moved_by_show = false;
                }
                let strings: Vec<&Vec<u8>> = match op.args.last() {
                    Some(Val::Str(s)) => vec![s],
                    Some(Val::Arr(items)) => items.iter().filter_map(|v| if let Val::Str(s) = v { Some(s) } else { None }).collect(),
                    _ => Vec::new(),
                };
                let has_glyphs = strings.iter().any(|s| !s.is_empty());
                // PDFium makes one text object per show operator that has a font and some text.
                if has_glyphs && st.font.is_some() {
                    let idx = objs.len();
                    if moved_by_show
                        && let Some(prev) = last_text_obj
                        && let Some(t) = &mut objs[prev].text
                    {
                        t.next_cont = Some(idx);
                    }
                    objs.push(ObjOps {
                        kind: Kind::Text,
                        first: i,
                        last: i,
                        ctm: st.ctm,
                        clip: false,
                        fill_state: st.fill.clone(),
                        stroke_state: st.stroke.clone(),
                        text: Some(TextInfo {
                            tlm,
                            font: st.font.clone().unwrap_or_default(),
                            size: st.size,
                            th: st.th,
                            rise: st.rise,
                            next_cont: None,
                            codes: strings.iter().flat_map(|s| s.iter().copied()).collect(),
                            word_kern: word_kern(op.args.last()),
                        }),
                    });
                    last_text_obj = Some(idx);
                }
                moved_by_show = true;
            }
            // Colour state, kept as the exact operators so it can be restored.
            b"g" | b"rg" | b"k" => st.fill = raw(op),
            b"G" | b"RG" | b"K" => st.stroke = raw(op),
            b"cs" => {
                last_cs = raw(op);
                st.fill = last_cs.clone();
            }
            b"CS" => {
                last_cs_stroke = raw(op);
                st.stroke = last_cs_stroke.clone();
            }
            b"sc" | b"scn" => {
                let mut v = last_cs.clone();
                v.push(b' ');
                v.extend(raw(op));
                st.fill = v;
            }
            b"SC" | b"SCN" => {
                let mut v = last_cs_stroke.clone();
                v.push(b' ');
                v.extend(raw(op));
                st.stroke = v;
            }
            b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re" => {
                path_start.get_or_insert(i);
            }
            b"W" | b"W*" => clip = true,
            b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                if let Some(first) = path_start.take() {
                    objs.push(ObjOps {
                        kind: Kind::Path,
                        first,
                        last: i,
                        ctm: st.ctm,
                        clip,
                        fill_state: st.fill.clone(),
                        stroke_state: st.stroke.clone(),
                        text: None,
                    });
                }
                clip = false;
            }
            b"n" => {
                path_start = None;
                clip = false;
            }
            b"Do" => {
                let kind = match op.args.first() {
                    Some(Val::Name(nm)) => xobjects.get(nm).copied(),
                    _ => None,
                };
                let kind = match kind {
                    Some(XKind::Image) => Some(Kind::Image),
                    Some(XKind::Form) => Some(Kind::Group),
                    _ => None,
                };
                if let Some(kind) = kind {
                    objs.push(ObjOps {
                        kind,
                        first: i,
                        last: i,
                        ctm: st.ctm,
                        clip: false,
                        fill_state: st.fill.clone(),
                        stroke_state: st.stroke.clone(),
                        text: None,
                    });
                }
            }
            b"BI" => objs.push(ObjOps {
                kind: Kind::Image,
                first: i,
                last: i,
                ctm: st.ctm,
                clip: false,
                fill_state: st.fill.clone(),
                stroke_state: st.stroke.clone(),
                text: None,
            }),
            b"sh" => objs.push(ObjOps {
                kind: Kind::Shading,
                first: i,
                last: i,
                ctm: st.ctm,
                clip: false,
                fill_state: st.fill.clone(),
                stroke_state: st.stroke.clone(),
                text: None,
            }),
            _ => {}
        }
    }
    Parsed { ops, objs }
}

// ── edits ──

#[derive(Clone, Debug)]
pub enum Edit {
    Delete,
    /// An affine change in user space.
    Transform(M),
    Fill([u8; 3]),
    Stroke([u8; 3]),
    Width(f64),
    Paint {
        fill: bool,
        stroke: bool,
    },
    /// Show this instead (same font).
    Text(Vec<Piece>),
    /// Draw a different XObject resource.
    XObject(Vec<u8>),
}

pub fn num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

fn mat(m: &M) -> String {
    m.iter().map(|v| num(*v)).collect::<Vec<_>>().join(" ")
}

fn hex(codes: &[u8]) -> String {
    let mut s = String::with_capacity(codes.len() * 2 + 2);
    s.push('<');
    for c in codes {
        s.push_str(&format!("{c:02X}"));
    }
    s.push('>');
    s
}

fn rgb(c: [u8; 3], op: &str) -> String {
    format!("{} {} {} {op}", num(c[0] as f64 / 255.0), num(c[1] as f64 / 255.0), num(c[2] as f64 / 255.0))
}

/// Part of a line of text: character codes, or a TJ adjustment (thousandths of the font size).
#[derive(Clone, Debug)]
pub enum Piece {
    Codes(Vec<u8>),
    Kern(f64),
}

fn show_pieces(p: &[Piece]) -> String {
    match p {
        [Piece::Codes(c)] => format!("{} Tj", hex(c)),
        _ => {
            let items: Vec<String> = p
                .iter()
                .map(|x| match x {
                    Piece::Codes(c) => hex(c),
                    Piece::Kern(k) => num(*k),
                })
                .collect();
            format!("[{}] TJ", items.join(" "))
        }
    }
}

/// The middle of the large negative TJ numbers in a show operand (they are word gaps).
fn word_kern(v: Option<&Val>) -> f64 {
    let Some(Val::Arr(items)) = v else { return 0.0 };
    let mut gaps: Vec<f64> = items.iter().filter_map(Val::num).filter(|n| *n <= -150.0).collect();
    if gaps.is_empty() {
        return 0.0;
    }
    gaps.sort_by(f64::total_cmp);
    gaps[gaps.len() / 2]
}

/// The start of a PDFium text object without the text rise.
fn origin(start: &M, rise: f64) -> (f64, f64) {
    (start[4] - rise * start[2], start[5] - rise * start[3])
}

/// Rewrite the content. `starts[k]` is PDFium's matrix for text object k (text matrix times CTM,
/// with the user-space position of the first glyph); other kinds may be None.
pub fn edit(src: &[u8], parsed: &Parsed, starts: &[Option<M>], edits: &[(usize, Edit)]) -> Result<Vec<u8>, String> {
    let ops = &parsed.ops;
    let mut replace: HashMap<usize, String> = HashMap::new();
    let mut before: HashMap<usize, String> = HashMap::new();
    let mut after: HashMap<usize, String> = HashMap::new();
    let push = |m: &mut HashMap<usize, String>, k: usize, s: &str| {
        let e = m.entry(k).or_default();
        if !e.is_empty() && !s.is_empty() {
            e.push(' ');
        }
        e.push_str(s);
    };
    let original = |k: usize| String::from_utf8_lossy(&src[ops[k].start..ops[k].end]).into_owned();

    // Group the edits by object.
    let mut by_obj: HashMap<usize, Vec<&Edit>> = HashMap::new();
    for (k, e) in edits {
        if *k >= parsed.objs.len() {
            return Err("The edit refers to an object that is not on the page.".into());
        }
        by_obj.entry(*k).or_default().push(e);
    }

    for (&k, list) in &by_obj {
        let o = &parsed.objs[k];
        let delete = list.iter().any(|e| matches!(e, Edit::Delete));
        let transform = list.iter().find_map(|e| if let Edit::Transform(t) = e { Some(*t) } else { None });
        let fill = list.iter().find_map(|e| if let Edit::Fill(c) = e { Some(*c) } else { None });
        let stroke = list.iter().find_map(|e| if let Edit::Stroke(c) = e { Some(*c) } else { None });
        let width = list.iter().find_map(|e| if let Edit::Width(w) = e { Some(*w) } else { None });
        let paint = list.iter().find_map(|e| if let Edit::Paint { fill, stroke } = e { Some((*fill, *stroke)) } else { None });
        let codes = list.iter().find_map(|e| if let Edit::Text(c) = e { Some(c.clone()) } else { None });
        let xobject = list.iter().find_map(|e| if let Edit::XObject(n) = e { Some(n.clone()) } else { None });

        match o.kind {
            Kind::Text => {
                let t = o.text.as_ref().unwrap();
                let op = &ops[o.first];
                // ' and " also move to the next line (and " sets spacing): keep those effects.
                let lead = match op.name.as_slice() {
                    b"'" => "T*".to_string(),
                    b"\"" => {
                        format!("{} Tw {} Tc T*", num(op.args.first().and_then(Val::num).unwrap_or(0.0)), num(op.args.get(1).and_then(Val::num).unwrap_or(0.0)))
                    }
                    _ => String::new(),
                };
                let show = match (&codes, op.name.as_slice()) {
                    (Some(c), _) => show_pieces(c),
                    (None, b"Tj" | b"TJ") => original(o.first),
                    (None, _) => match op.args.last() {
                        Some(Val::Str(s)) => format!("{} Tj", hex(s)),
                        _ => String::new(),
                    },
                };
                let mut body = lead.clone();
                let mut moved_text = false;
                if !delete {
                    if let Some(fc) = fill {
                        body.push(' ');
                        body.push_str(&rgb(fc, "rg"));
                    }
                    if let Some(sc) = stroke {
                        body.push(' ');
                        body.push_str(&rgb(sc, "RG"));
                    }
                    if let Some(tm) = transform {
                        let start = starts.get(k).copied().flatten().ok_or("The text position is unknown.")?;
                        let (ox, oy) = origin(&start, t.rise);
                        let x = [start[0], start[1], start[2], start[3], ox, oy];
                        let tm_new = mul(&mul(&x, &tm), &inv(&o.ctm));
                        body.push_str(&format!(" {} Tm", mat(&tm_new)));
                        moved_text = true;
                    }
                    body.push(' ');
                    body.push_str(&show);
                    if fill.is_some() {
                        body.push(' ');
                        body.push_str(if o.fill_state.is_empty() { "0 g" } else { std::str::from_utf8(&o.fill_state).unwrap_or("0 g") });
                    }
                    if stroke.is_some() {
                        body.push(' ');
                        body.push_str(if o.stroke_state.is_empty() { "0 G" } else { std::str::from_utf8(&o.stroke_state).unwrap_or("0 G") });
                    }
                    if moved_text {
                        // Tm also set the line matrix; put it back for the positioning that follows.
                        body.push_str(&format!(" {} Tm", mat(&t.tlm)));
                    }
                }
                replace.insert(o.first, body.trim().to_string());
                // Text that continued from this object's end needs its start restored.
                let advance_changed = delete || codes.is_some() || moved_text;
                if let (true, Some(j)) = (advance_changed, t.next_cont) {
                    let tj = parsed.objs[j].text.as_ref().unwrap();
                    let start = starts.get(j).copied().flatten().ok_or("The text position is unknown.")?;
                    let (px, py) = origin(&start, tj.rise);
                    let lm = mul(&tj.tlm, &parsed.objs[j].ctm);
                    let (tx, _) = apply(&inv(&lm), px, py);
                    let scale = tj.size * tj.th;
                    let fix = if scale.abs() > 1e-9 {
                        format!("{} Tm [{}] TJ", mat(&tj.tlm), num(-tx * 1000.0 / scale))
                    } else {
                        let x = [start[0], start[1], start[2], start[3], px, py];
                        format!("{} Tm", mat(&mul(&x, &inv(&parsed.objs[j].ctm))))
                    };
                    push(&mut before, parsed.objs[j].first, &fix);
                }
            }
            Kind::Path => {
                let last = &ops[o.last];
                let even_odd = matches!(last.name.as_slice(), b"f*" | b"B*" | b"b*");
                let closes = matches!(last.name.as_slice(), b"s" | b"b" | b"b*");
                let (was_fill, was_stroke) = match last.name.as_slice() {
                    b"S" | b"s" => (false, true),
                    b"f" | b"F" | b"f*" => (true, false),
                    _ => (true, true),
                };
                if delete {
                    replace.insert(o.last, "n".into());
                    continue;
                }
                let (pf, ps) = paint.unwrap_or((was_fill, was_stroke));
                let paint_op = match (pf, ps) {
                    (true, true) => match (closes, even_odd) {
                        (true, true) => "b*",
                        (true, false) => "b",
                        (false, true) => "B*",
                        (false, false) => "B",
                    },
                    (true, false) => {
                        if even_odd {
                            "f*"
                        } else {
                            "f"
                        }
                    }
                    (false, true) => {
                        if closes {
                            "s"
                        } else {
                            "S"
                        }
                    }
                    (false, false) => "n",
                };
                let mut prefix = String::new();
                if let Some(t) = transform {
                    prefix.push_str(&format!("{} cm", mat(&mul(&mul(&o.ctm, &t), &inv(&o.ctm)))));
                }
                if let Some(c) = fill {
                    prefix.push(' ');
                    prefix.push_str(&rgb(c, "rg"));
                }
                if let Some(c) = stroke {
                    prefix.push(' ');
                    prefix.push_str(&rgb(c, "RG"));
                }
                if let Some(w) = width {
                    prefix.push_str(&format!(" {} w", num(w)));
                }
                let prefix = prefix.trim().to_string();
                if o.clip {
                    // Keep the clip where it was; draw the changed copy after it.
                    let construction: Vec<String> = (o.first..o.last).filter(|&i| !matches!(ops[i].name.as_slice(), b"W" | b"W*")).map(original).collect();
                    replace.insert(o.last, format!("n q {prefix} {} {paint_op} Q", construction.join(" ")));
                } else if prefix.is_empty() {
                    replace.insert(o.last, paint_op.into());
                } else {
                    push(&mut before, o.first, &format!("q {prefix}"));
                    replace.insert(o.last, paint_op.into());
                    let a = after.entry(o.last).or_default();
                    a.push_str(" Q");
                }
            }
            Kind::Image | Kind::Group | Kind::Shading => {
                if delete {
                    replace.insert(o.first, String::new());
                    continue;
                }
                let draw = match (&xobject, o.kind) {
                    (Some(name), Kind::Image) => format!("/{} Do", String::from_utf8_lossy(name)),
                    _ => original(o.first),
                };
                match transform {
                    Some(t) => {
                        replace.insert(o.first, format!("q {} cm {draw} Q", mat(&mul(&mul(&o.ctm, &t), &inv(&o.ctm)))));
                    }
                    None => {
                        replace.insert(o.first, draw);
                    }
                }
            }
        }
    }

    // Write: original bytes between operators, edited operators where needed.
    let mut out = Vec::with_capacity(src.len() + 256);
    let mut pos = 0;
    for (i, op) in ops.iter().enumerate() {
        out.extend_from_slice(&src[pos..op.start]);
        if let Some(b) = before.get(&i) {
            out.extend_from_slice(b.as_bytes());
            out.push(b'\n');
        }
        match replace.get(&i) {
            Some(r) => out.extend_from_slice(r.as_bytes()),
            None => out.extend_from_slice(&src[op.start..op.end]),
        }
        if let Some(a) = after.get(&i) {
            out.extend_from_slice(a.as_bytes());
        }
        pos = op.end;
    }
    out.extend_from_slice(&src[pos..]);
    Ok(out)
}

/// Characters this font can show, learned by pairing each text object's codes with PDFium's
/// reading of the same text. Returns char → codes.
pub fn learn_encoding(parsed: &Parsed, texts: &[Option<String>], font: &[u8], meta: &FontMeta) -> HashMap<char, Vec<u8>> {
    let mut map = HashMap::new();
    let width = if meta.two_byte { 2 } else { 1 };
    for (k, o) in parsed.objs.iter().enumerate() {
        let (Some(t), Some(Some(text))) = (&o.text, texts.get(k)) else { continue };
        if t.font != font {
            continue;
        }
        let chars: Vec<char> = text.chars().collect();
        let codes: Vec<&[u8]> = t.codes.chunks(width).collect();
        // Only trust objects where every code maps to exactly one character.
        let chars: Vec<char> = if chars.len() == codes.len() + 1 && chars.last() == Some(&' ') { chars[..codes.len()].to_vec() } else { chars };
        // Word gaps drawn as TJ adjustments read back as spaces that have no code.
        let chars: Vec<char> = if chars.len() != codes.len() { chars.into_iter().filter(|c| *c != ' ').collect() } else { chars };
        if chars.len() != codes.len() {
            continue;
        }
        for (c, code) in chars.into_iter().zip(codes) {
            map.entry(c).or_insert_with(|| code.to_vec());
        }
    }
    map
}

/// Encode `text` for a font: from learned codes, or from cp1252 for standard WinAnsi fonts.
/// Spaces the font has no code for become TJ gaps of `word_kern` (or a third of the size).
pub fn encode(text: &str, learned: &HashMap<char, Vec<u8>>, meta: &FontMeta, word_kern: f64) -> Option<Vec<Piece>> {
    let mut out: Vec<Piece> = Vec::new();
    for c in text.chars() {
        let code = if let Some(code) = learned.get(&c) {
            code.clone()
        } else if meta.winansi {
            vec![cp1252(c)?]
        } else if c == ' ' {
            out.push(Piece::Kern(if word_kern < 0.0 { word_kern } else { -333.0 }));
            continue;
        } else {
            return None;
        };
        match out.last_mut() {
            Some(Piece::Codes(v)) => v.extend_from_slice(&code),
            _ => out.push(Piece::Codes(code)),
        }
    }
    Some(out)
}

fn cp1252(c: char) -> Option<u8> {
    let u = c as u32;
    if (0x20..0x7F).contains(&u) || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    const HIGH: [(char, u8); 27] = [
        ('€', 0x80),
        ('‚', 0x82),
        ('ƒ', 0x83),
        ('„', 0x84),
        ('…', 0x85),
        ('†', 0x86),
        ('‡', 0x87),
        ('ˆ', 0x88),
        ('‰', 0x89),
        ('Š', 0x8A),
        ('‹', 0x8B),
        ('Œ', 0x8C),
        ('Ž', 0x8E),
        ('‘', 0x91),
        ('’', 0x92),
        ('“', 0x93),
        ('”', 0x94),
        ('•', 0x95),
        ('–', 0x96),
        ('—', 0x97),
        ('˜', 0x98),
        ('™', 0x99),
        ('š', 0x9A),
        ('›', 0x9B),
        ('œ', 0x9C),
        ('ž', 0x9E),
        ('Ÿ', 0x9F),
    ];
    HIGH.iter().find(|(h, _)| *h == c).map(|(_, b)| *b)
}
