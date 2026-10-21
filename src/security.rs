//! Passwords and permissions. Protection is a save-time setting: the document is kept unencrypted
//! while it is open and encrypted (AES-256) when it is written to disk.

use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::Editor;
use crate::theme::{self, UiExt, pal};
use egui::{RichText, Ui};
use lopdf::{EncryptionState, EncryptionVersion, Permissions};
use std::collections::BTreeMap;
use std::sync::Arc;

/// What a reader may do without the owner password.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Allow {
    pub print: bool,
    pub copy: bool,
    pub edit: bool,
    pub comment: bool,
    pub fill: bool,
    pub assemble: bool,
}

impl Allow {
    pub const ALL: Allow = Allow { print: true, copy: true, edit: true, comment: true, fill: true, assemble: true };

    /// From a PDF permission word (P), as PDFium reports it.
    pub fn from_bits(p: u32) -> Allow {
        let on = |bit: u32| p & (1 << (bit - 1)) != 0;
        Allow { print: on(3), edit: on(4), copy: on(5), comment: on(6), fill: on(9), assemble: on(11) }
    }

    fn permissions(self) -> Permissions {
        let mut p = Permissions::COPYABLE_FOR_ACCESSIBILITY;
        if self.print {
            p |= Permissions::PRINTABLE | Permissions::PRINTABLE_IN_HIGH_QUALITY;
        }
        if self.edit {
            p |= Permissions::MODIFIABLE;
        }
        if self.copy {
            p |= Permissions::COPYABLE;
        }
        if self.comment {
            p |= Permissions::ANNOTABLE;
        }
        if self.fill {
            p |= Permissions::FILLABLE;
        }
        if self.assemble {
            p |= Permissions::ASSEMBLABLE;
        }
        p
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Protection {
    /// Needed to open the document; empty for none.
    pub open: String,
    /// Needed to change the permissions (and, with restrictions, to edit).
    pub owner: String,
    pub allow: Allow,
}

/// How the document is protected when it is saved.
#[derive(Clone, PartialEq, Debug, Default)]
pub enum Security {
    /// As it was opened: an encrypted file stays encrypted the same way.
    #[default]
    Keep,
    Remove,
    Protect(Protection),
}

/// Random bytes from the system's generator.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
        BCryptGenRandom(std::ptr::null_mut(), b.as_mut_ptr(), n as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    }
    b
}

/// Encrypt an unencrypted PDF with AES-256.
pub fn encrypt(plain: &[u8], p: &Protection) -> Result<Vec<u8>, String> {
    let mut doc = lopdf::Document::load_mem(plain).map_err(|e| format!("The file could not be prepared for protection ({e})."))?;
    let key = random_bytes(32);
    let filter: Arc<dyn lopdf::encryption::crypt_filters::CryptFilter> = Arc::new(lopdf::encryption::crypt_filters::Aes256CryptFilter);
    // Without an owner password of its own, a random one keeps the restrictions in force.
    let owner = if p.owner.is_empty() {
        if p.allow == Allow::ALL { p.open.clone() } else { random_bytes(16).iter().map(|b| format!("{b:02x}")).collect() }
    } else {
        p.owner.clone()
    };
    let state = EncryptionState::try_from(EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: &owner,
        user_password: &p.open,
        permissions: p.allow.permissions(),
    })
    .map_err(|e| format!("The protection could not be set up ({e})."))?;
    doc.encrypt(&state).map_err(|e| format!("The file could not be encrypted ({e})."))?;
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| format!("The protected file could not be written ({e})."))?;
    Ok(out)
}

// ── the dialog ──

pub struct SecurityDialog {
    lock_open: bool,
    open: String,
    open_again: String,
    restrict: bool,
    owner: String,
    allow: Allow,
    error: Option<String>,
}

impl SecurityDialog {
    pub fn new(ed: &Editor) -> Self {
        let mut d = SecurityDialog {
            lock_open: false,
            open: String::new(),
            open_again: String::new(),
            restrict: false,
            owner: String::new(),
            allow: Allow::ALL,
            error: None,
        };
        if let Some(f) = &ed.file
            && let Security::Protect(p) = &f.security
        {
            d.lock_open = !p.open.is_empty();
            d.open = p.open.clone();
            d.open_again = p.open.clone();
            d.restrict = p.allow != Allow::ALL;
            d.owner = p.owner.clone();
            d.allow = p.allow;
        }
        d
    }
}

fn describe(ed: &Editor) -> String {
    let Some(f) = &ed.file else { return String::new() };
    match &f.security {
        Security::Protect(p) if !p.open.is_empty() => "It will be saved with a password to open it.".into(),
        Security::Protect(_) => "It will be saved with restrictions.".into(),
        Security::Remove => "It will be saved without protection.".into(),
        Security::Keep if f.doc.encrypted() => "This document is protected. It stays protected unless you change it here.".into(),
        Security::Keep => "This document is not protected.".into(),
    }
}

pub fn security_window(ctx: &egui::Context, ed: &mut Editor, d: &mut SecurityDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut apply = false;
    let mut remove = false;
    let protected = ed.file.as_ref().is_some_and(|f| f.doc.encrypted() || matches!(f.security, Security::Protect(_)));
    let (_, close) = modal(ctx, "security", 420.0, |ui| {
        title(ui, "Password and permissions");
        ui.label(theme::secondary(ui, describe(ed)));
        ui.add_space(6.0);
        ui.switch(&mut d.lock_open, "Require a password to open");
        if d.lock_open {
            password_field(ui, &mut d.open, "Password");
            password_field(ui, &mut d.open_again, "Type it again");
        }
        ui.add_space(4.0);
        ui.switch(&mut d.restrict, "Restrict what others can do");
        if d.restrict {
            password_field(ui, &mut d.owner, "Owner password, to change these later");
            theme::heading(ui, "ALLOW");
            let a = &mut d.allow;
            ui.checkbox(&mut a.print, "Printing");
            ui.checkbox(&mut a.copy, "Copying text and images");
            ui.checkbox(&mut a.edit, "Changing the content");
            ui.checkbox(&mut a.comment, "Comments");
            ui.checkbox(&mut a.fill, "Filling in forms and signing");
            ui.checkbox(&mut a.assemble, "Adding, removing and rotating pages");
        }
        ui.add_space(4.0);
        ui.label(theme::secondary(ui, "Protection is applied when you save. Dockit uses AES-256 encryption."));
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if protected && ui.button(RichText::new("Remove protection").color(p.danger)).clicked() {
                remove = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, "Apply", d.lock_open || d.restrict) {
                    apply = true;
                }
                if secondary_button(ui, "Cancel") {
                    open = false;
                }
            });
        });
    });
    if close {
        open = false;
    }
    if remove {
        set_security(ed, Security::Remove, "Protection will be removed when you save.");
        return false;
    }
    if apply {
        if d.lock_open && d.open.is_empty() {
            d.error = Some("Type a password, or turn off the password to open.".into());
        } else if d.lock_open && d.open != d.open_again {
            d.error = Some("The two passwords are not the same.".into());
        } else if d.restrict && d.owner.is_empty() {
            d.error = Some("Type an owner password, so the restrictions can be changed later.".into());
        } else if d.restrict && d.lock_open && d.owner == d.open {
            d.error = Some("The owner password must be different from the password to open.".into());
        } else {
            let prot = Protection {
                open: if d.lock_open { d.open.clone() } else { String::new() },
                owner: if d.restrict { d.owner.clone() } else { String::new() },
                allow: if d.restrict { d.allow } else { Allow::ALL },
            };
            set_security(ed, Security::Protect(prot), "Protection will be applied when you save.");
            return false;
        }
    }
    open
}

fn password_field(ui: &mut Ui, s: &mut String, hint: &str) {
    ui.add(egui::TextEdit::singleline(s).password(true).desired_width(f32::INFINITY).hint_text(hint));
}

fn set_security(ed: &mut Editor, s: Security, msg: &str) {
    if let Some(f) = &mut ed.file {
        f.security = s;
        f.mark_unsaved();
    }
    ed.say(msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_bits_round_trip() {
        let a = Allow { print: true, copy: false, edit: false, comment: true, fill: true, assemble: false };
        let bits = a.permissions().bits() as u32;
        assert_eq!(Allow::from_bits(bits), a);
    }

    /// Needs pdfium.dll in target/debug (run with --ignored).
    #[test]
    #[ignore]
    fn encrypted_file_needs_its_password() {
        use crate::pdf::{Doc, OpenError};
        crate::pdf::init(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug").as_path()).unwrap();
        let doc = Doc::new_empty();
        assert!(doc.insert_blank(0, 200.0, 200.0));
        let plain = doc.save().unwrap();
        let p = Protection { open: "open sesame".into(), owner: "boss".into(), allow: Allow { edit: false, ..Allow::ALL } };
        let bytes = encrypt(&plain, &p).unwrap();
        assert!(matches!(Doc::open(bytes.clone(), None), Err(OpenError::Password)));
        assert!(matches!(Doc::open(bytes.clone(), Some("wrong")), Err(OpenError::Password)));
        let user = Doc::open(bytes.clone(), Some("open sesame")).unwrap();
        assert!(user.encrypted());
        assert!(!Allow::from_bits(user.permissions()).edit);
        let owner = Doc::open(bytes, Some("boss")).unwrap();
        assert!(Allow::from_bits(owner.permissions()).edit);
    }
}
