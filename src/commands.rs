//! Every command Dockit has, in one place: the menus, the keyboard shortcuts (which can be changed
//! in Settings) and the command palette (Ctrl+K) all run the same commands.

use crate::editor::Tool;
use egui::{Key, Modifiers};
use serde::{Deserialize, Serialize};

/// How pages are laid out in the document view.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum PageLayout {
    /// One page at a time; scrolling past its end turns the page.
    Single,
    /// Pages one under another in a long column.
    #[default]
    Continuous,
    /// Two pages side by side, a spread at a time.
    Two,
    /// Spreads one under another.
    TwoContinuous,
}

impl PageLayout {
    pub const ALL: [PageLayout; 4] = [PageLayout::Single, PageLayout::Continuous, PageLayout::Two, PageLayout::TwoContinuous];

    pub fn name(self) -> &'static str {
        match self {
            PageLayout::Single => "Single page",
            PageLayout::Continuous => "Single page, continuous",
            PageLayout::Two => "Two pages",
            PageLayout::TwoContinuous => "Two pages, continuous",
        }
    }

    pub fn two(self) -> bool {
        matches!(self, PageLayout::Two | PageLayout::TwoContinuous)
    }

    /// Shows a page (or spread) at a time rather than a scrolling column.
    pub fn paged(self) -> bool {
        matches!(self, PageLayout::Single | PageLayout::Two)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Cmd {
    // File
    Open,
    Save,
    SaveAs,
    Close,
    ExportImages,
    Quit,
    // Edit
    ToggleEdit,
    Undo,
    Redo,
    SelectAll,
    Duplicate,
    BringFront,
    SendBack,
    Ungroup,
    ApplyRedactions,
    ChangeSignature,
    // Pages
    RotateLeft,
    RotateRight,
    DuplicatePages,
    DeletePages,
    InsertBlank,
    InsertFromFile,
    ExtractPages,
    Crop,
    Stamp,
    Flatten,
    // View
    ZoomIn,
    ZoomOut,
    FitWidth,
    FitPage,
    ActualSize,
    Layout(PageLayout),
    CoverPage,
    ResetPanels,
    Palette,
    // Tools
    Combine,
    Split,
    FromImages,
    Compress,
    Ocr,
    Compare,
    Security,
    SignDigitally,
    // Settings
    Settings,
    Tool(Tool),
}

impl Cmd {
    pub fn name(self) -> String {
        match self {
            Cmd::Open => "Open…".into(),
            Cmd::Save => "Save".into(),
            Cmd::SaveAs => "Save as…".into(),
            Cmd::Close => "Close".into(),
            Cmd::ExportImages => "Export pages as images…".into(),
            Cmd::Quit => "Quit".into(),
            Cmd::ToggleEdit => "Edit PDF or finish editing".into(),
            Cmd::Undo => "Undo".into(),
            Cmd::Redo => "Redo".into(),
            Cmd::SelectAll => "Select all on page".into(),
            Cmd::Duplicate => "Duplicate".into(),
            Cmd::BringFront => "Bring to front".into(),
            Cmd::SendBack => "Send to back".into(),
            Cmd::Ungroup => "Ungroup".into(),
            Cmd::ApplyRedactions => "Apply redactions…".into(),
            Cmd::ChangeSignature => "Change signature…".into(),
            Cmd::RotateLeft => "Rotate pages left".into(),
            Cmd::RotateRight => "Rotate pages right".into(),
            Cmd::DuplicatePages => "Duplicate pages".into(),
            Cmd::DeletePages => "Delete pages".into(),
            Cmd::InsertBlank => "Insert blank page".into(),
            Cmd::InsertFromFile => "Insert pages from a file…".into(),
            Cmd::ExtractPages => "Extract pages…".into(),
            Cmd::Crop => "Crop or resize pages…".into(),
            Cmd::Stamp => "Page numbers, headers and watermark…".into(),
            Cmd::Flatten => "Flatten comments and form fields".into(),
            Cmd::ZoomIn => "Zoom in".into(),
            Cmd::ZoomOut => "Zoom out".into(),
            Cmd::FitWidth => "Fit width".into(),
            Cmd::FitPage => "Fit page".into(),
            Cmd::ActualSize => "Actual size".into(),
            Cmd::Layout(l) => l.name().into(),
            Cmd::CoverPage => "Show the cover page alone".into(),
            Cmd::ResetPanels => "Reset panel layout".into(),
            Cmd::Palette => "Command palette".into(),
            Cmd::Combine => "Combine files…".into(),
            Cmd::Split => "Split into files…".into(),
            Cmd::FromImages => "Create a PDF from images…".into(),
            Cmd::Compress => "Compress…".into(),
            Cmd::Ocr => "Recognize text (OCR)…".into(),
            Cmd::Compare => "Compare with another version…".into(),
            Cmd::Security => "Password and permissions…".into(),
            Cmd::SignDigitally => "Sign digitally…".into(),
            Cmd::Settings => "Settings".into(),
            Cmd::Tool(t) => format!("{} tool", t.name()),
        }
    }

    /// Extra words the palette matches on.
    pub fn keywords(self) -> &'static str {
        match self {
            Cmd::Open => "file load",
            Cmd::Save | Cmd::SaveAs => "file write",
            Cmd::ExportImages => "png jpg jpeg picture",
            Cmd::ToggleEdit => "edit done reading",
            Cmd::Combine => "merge join",
            Cmd::Split => "separate",
            Cmd::Compress => "smaller size optimize",
            Cmd::Ocr => "scan searchable text recognition",
            Cmd::Compare => "diff differences versions",
            Cmd::Security => "encrypt protect lock restrict",
            Cmd::SignDigitally => "certificate pfx signature",
            Cmd::Crop => "trim margins paper size",
            Cmd::Stamp => "bates header footer watermark numbers",
            Cmd::Layout(_) | Cmd::CoverPage => "view layout pages spread book",
            Cmd::Settings => "preferences options keyboard shortcuts keys appearance dark light",
            Cmd::Tool(_) => "tool",
            _ => "",
        }
    }

    /// Needs an open document.
    pub fn needs_doc(self) -> bool {
        !matches!(self, Cmd::Open | Cmd::Quit | Cmd::Combine | Cmd::FromImages | Cmd::Settings | Cmd::Palette | Cmd::ResetPanels)
    }

    /// Changes the document, so it switches to editing first.
    pub fn edits(self) -> bool {
        matches!(
            self,
            Cmd::SelectAll
                | Cmd::Duplicate
                | Cmd::BringFront
                | Cmd::SendBack
                | Cmd::Ungroup
                | Cmd::ApplyRedactions
                | Cmd::RotateLeft
                | Cmd::RotateRight
                | Cmd::DuplicatePages
                | Cmd::DeletePages
                | Cmd::InsertBlank
                | Cmd::InsertFromFile
                | Cmd::Crop
                | Cmd::Stamp
                | Cmd::Flatten
                | Cmd::Compress
                | Cmd::Ocr
                | Cmd::Tool(_)
        )
    }

    /// Works while text is being typed (file and view commands with Ctrl).
    pub fn while_typing(self) -> bool {
        matches!(
            self,
            Cmd::Open | Cmd::Save | Cmd::SaveAs | Cmd::Close | Cmd::Quit | Cmd::ZoomIn | Cmd::ZoomOut | Cmd::FitWidth | Cmd::FitPage | Cmd::ActualSize | Cmd::Palette | Cmd::Settings
        )
    }
}

/// Every command with a shortcut, in the order Settings lists them, by menu.
pub fn sections() -> Vec<(&'static str, Vec<Cmd>)> {
    vec![
        ("File", vec![Cmd::Open, Cmd::Save, Cmd::SaveAs, Cmd::Close, Cmd::ExportImages, Cmd::Quit]),
        ("Edit", vec![Cmd::ToggleEdit, Cmd::Undo, Cmd::Redo, Cmd::SelectAll, Cmd::Duplicate, Cmd::BringFront, Cmd::SendBack, Cmd::Ungroup, Cmd::ApplyRedactions]),
        (
            "Pages",
            vec![Cmd::RotateLeft, Cmd::RotateRight, Cmd::DuplicatePages, Cmd::DeletePages, Cmd::InsertBlank, Cmd::InsertFromFile, Cmd::ExtractPages, Cmd::Crop, Cmd::Stamp, Cmd::Flatten],
        ),
        (
            "View",
            vec![
                Cmd::ZoomIn,
                Cmd::ZoomOut,
                Cmd::FitWidth,
                Cmd::FitPage,
                Cmd::ActualSize,
                Cmd::Layout(PageLayout::Single),
                Cmd::Layout(PageLayout::Continuous),
                Cmd::Layout(PageLayout::Two),
                Cmd::Layout(PageLayout::TwoContinuous),
                Cmd::CoverPage,
                Cmd::Palette,
            ],
        ),
        ("Tools", vec![Cmd::Combine, Cmd::Split, Cmd::FromImages, Cmd::Compress, Cmd::Ocr, Cmd::Compare, Cmd::Security, Cmd::SignDigitally]),
        ("Editing tools", crate::panels_tools().into_iter().map(Cmd::Tool).collect()),
        ("Other", vec![Cmd::Settings]),
    ]
}

/// A key with the modifiers held for it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Combo {
    /// egui's name for the key, such as "S" or "F5".
    pub key: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl Combo {
    pub fn new(key: Key, ctrl: bool, shift: bool) -> Self {
        Combo { key: key.name().to_owned(), ctrl, shift, alt: false }
    }

    /// The key pressed with `m`, as a combo. `+` counts as `=`, so Ctrl+Plus and Ctrl+= agree.
    pub fn from_press(key: Key, m: Modifiers) -> Self {
        let key = if key == Key::Plus { Key::Equals } else { key };
        Combo { key: key.name().to_owned(), ctrl: m.command || m.ctrl, shift: m.shift, alt: m.alt }
    }

    pub fn label(&self) -> String {
        let key = match Key::from_name(&self.key) {
            Some(Key::Equals) => "+".to_owned(),
            Some(k) => k.symbol_or_name().to_owned(),
            None => self.key.clone(),
        };
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        s.push_str(&key);
        s
    }
}

/// The shortcut each command starts with.
pub fn default_combo(c: Cmd) -> Option<Combo> {
    use Key::*;
    let ctrl = |k| Some(Combo::new(k, true, false));
    let ctrl_shift = |k| Some(Combo::new(k, true, true));
    match c {
        Cmd::Open => ctrl(O),
        Cmd::Save => ctrl(S),
        Cmd::SaveAs => ctrl_shift(S),
        Cmd::Close => ctrl(W),
        Cmd::Quit => ctrl(Q),
        Cmd::ToggleEdit => ctrl(E),
        Cmd::Undo => ctrl(Z),
        Cmd::Redo => ctrl(Y),
        Cmd::SelectAll => ctrl(A),
        Cmd::Duplicate => ctrl(D),
        Cmd::BringFront => ctrl(CloseBracket),
        Cmd::SendBack => ctrl(OpenBracket),
        Cmd::Ungroup => ctrl_shift(G),
        Cmd::ZoomIn => ctrl(Equals),
        Cmd::ZoomOut => ctrl(Minus),
        Cmd::FitWidth => ctrl(Num0),
        Cmd::ActualSize => ctrl(Num1),
        Cmd::FitPage => ctrl(Num2),
        Cmd::Layout(PageLayout::Single) => ctrl_shift(Num1),
        Cmd::Layout(PageLayout::Continuous) => ctrl_shift(Num2),
        Cmd::Layout(PageLayout::Two) => ctrl_shift(Num3),
        Cmd::Layout(PageLayout::TwoContinuous) => ctrl_shift(Num4),
        Cmd::Palette => ctrl(K),
        Cmd::Settings => ctrl(Comma),
        Cmd::Tool(t) => Some(Combo::new(t.key(), false, false)),
        _ => None,
    }
}

/// The shortcuts in use: the defaults, with whatever was changed in Settings.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Keymap {
    /// Only the changed ones; `None` means the command has no shortcut.
    changed: Vec<(Cmd, Option<Combo>)>,
}

impl Keymap {
    pub fn get(&self, c: Cmd) -> Option<Combo> {
        match self.changed.iter().find(|(x, _)| *x == c) {
            Some((_, k)) => k.clone(),
            None => default_combo(c),
        }
    }

    pub fn set(&mut self, c: Cmd, k: Option<Combo>) {
        self.changed.retain(|(x, _)| *x != c);
        if k != default_combo(c) {
            self.changed.push((c, k));
        }
    }

    pub fn is_default(&self, c: Cmd) -> bool {
        !self.changed.iter().any(|(x, _)| *x == c)
    }

    pub fn reset_all(&mut self) {
        self.changed.clear();
    }

    /// The other command that already uses `k`, if any.
    pub fn user_of(&self, k: &Combo, except: Cmd) -> Option<Cmd> {
        sections().into_iter().flat_map(|(_, v)| v).find(|c| *c != except && self.get(*c).as_ref() == Some(k))
    }

    /// The command for a key pressed with `m`.
    pub fn command_for(&self, key: Key, m: Modifiers) -> Option<Cmd> {
        let k = Combo::from_press(key, m);
        sections().into_iter().flat_map(|(_, v)| v).find(|c| self.get(*c).as_ref() == Some(&k))
    }

    /// The shortcut as text, or empty.
    pub fn text(&self, c: Cmd) -> String {
        self.get(c).map(|k| k.label()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_defaults_clash() {
        let all: Vec<Cmd> = sections().into_iter().flat_map(|(_, v)| v).collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                if let (Some(x), Some(y)) = (default_combo(*a), default_combo(*b)) {
                    assert_ne!(x, y, "{a:?} and {b:?} share {}", x.label());
                }
            }
        }
    }

    #[test]
    fn changing_a_shortcut() {
        let mut k = Keymap::default();
        let c = Combo::new(Key::F7, false, false);
        k.set(Cmd::Ocr, Some(c.clone()));
        assert_eq!(k.command_for(Key::F7, Modifiers::NONE), Some(Cmd::Ocr));
        assert_eq!(k.user_of(&c, Cmd::Compress), Some(Cmd::Ocr));
        k.reset_all();
        assert!(k.is_default(Cmd::Ocr));
        assert_eq!(k.command_for(Key::Plus, Modifiers::COMMAND), Some(Cmd::ZoomIn));
    }
}
