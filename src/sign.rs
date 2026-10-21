//! Digital signatures with a certificate (.pfx), using Windows' own cryptography.
//!
//! Signing appends one incremental update holding the signature field, its appearance and a
//! signature dictionary whose /Contents is a zero-filled placeholder. The /ByteRange is then filled
//! in to cover the whole file except that placeholder, those bytes are signed (CMS, detached,
//! SHA-256: the "adbe.pkcs7.detached" form every PDF reader checks), and the signature is written
//! into the placeholder. Nothing else in the file changes, so earlier signatures stay valid.

use crate::annots::{self, NewField, Update};
use crate::dialogs::{modal, primary, secondary_button, title};
use crate::editor::Editor;
use crate::pdf::Doc;
use crate::theme::{self, pal};
use egui::{RichText, Ui};
use lopdf::{Dictionary, Object, StringFormat};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Security::Cryptography::*;

const SHA256: &[u8] = b"2.16.840.1.101.3.4.2.1\0";
const SHA256_RSA: &[u8] = b"1.2.840.113549.1.1.11\0";
const ENCODING: u32 = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
/// Room for the signature (bytes): a CMS blob with a certificate chain fits easily.
const RESERVE: usize = 20_000;
/// Placeholder byte range, wide enough for the real numbers.
const RANGE_PLACEHOLDER: &[u8] = b"[0 1000000000 1000000000 1000000000]";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn cert_name(cert: *const CERT_CONTEXT) -> String {
    unsafe {
        let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, null(), null_mut(), 0);
        let mut buf = vec![0u16; n.max(1) as usize];
        CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, null(), buf.as_mut_ptr(), n);
        String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
    }
}

/// A certificate with its private key, loaded from a .pfx file.
pub struct Signer {
    store: HCERTSTORE,
    cert: *const CERT_CONTEXT,
    pub name: String,
}

impl Drop for Signer {
    fn drop(&mut self) {
        unsafe {
            CertFreeCertificateContext(self.cert);
            CertCloseStore(self.store, 0);
        }
    }
}

impl Signer {
    pub fn load(path: &Path, password: &str) -> Result<Signer, String> {
        let mut bytes = std::fs::read(path).map_err(|e| format!("{} could not be read ({e}).", path.display()))?;
        let blob = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_mut_ptr() };
        let pw = wide(password);
        unsafe {
            let store = PFXImportCertStore(&blob, pw.as_ptr(), PKCS12_NO_PERSIST_KEY | PKCS12_ALWAYS_CNG_KSP);
            if store.is_null() {
                return Err("The certificate could not be opened. Check the password, and that it is a .pfx or .p12 file.".into());
            }
            // The certificate whose private key came with the file.
            let mut cur: *mut CERT_CONTEXT = null_mut();
            let mut last = String::new();
            loop {
                cur = CertEnumCertificatesInStore(store, cur);
                if cur.is_null() {
                    CertCloseStore(store, 0);
                    return Err(format!("The file has no certificate with a private key, so it cannot sign ({last})."));
                }
                let (mut key, mut spec, mut free) = (0usize, 0u32, 0);
                if CryptAcquireCertificatePrivateKey(
                    cur,
                    // The key from the file is held in memory only, as a handle cached on the certificate.
                    CRYPT_ACQUIRE_CACHE_FLAG | CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_SILENT_FLAG,
                    null(),
                    &mut key,
                    &mut spec,
                    &mut free,
                ) != 0 {
                    if free != 0 {
                        if spec == CERT_NCRYPT_KEY_SPEC {
                            NCryptFreeObject(key);
                        } else {
                            CryptReleaseContext(key, 0);
                        }
                    }
                    break;
                }
                last = std::io::Error::last_os_error().to_string();

            }
            Ok(Signer { store, cert: cur, name: cert_name(cur) })
        }
    }

    /// A detached CMS (PKCS #7) signature of `data`, with SHA-256 and the signer's certificate.
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        unsafe {
            let mut cert = self.cert as *mut CERT_CONTEXT;
            let para = CRYPT_SIGN_MESSAGE_PARA {
                cbSize: std::mem::size_of::<CRYPT_SIGN_MESSAGE_PARA>() as u32,
                dwMsgEncodingType: ENCODING,
                pSigningCert: self.cert,
                HashAlgorithm: CRYPT_ALGORITHM_IDENTIFIER { pszObjId: SHA256.as_ptr() as *mut u8, Parameters: CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() } },
                pvHashAuxInfo: null_mut(),
                cMsgCert: 1,
                rgpMsgCert: &mut cert,
                cMsgCrl: 0,
                rgpMsgCrl: null_mut(),
                cAuthAttr: 0,
                rgAuthAttr: null_mut(),
                cUnauthAttr: 0,
                rgUnauthAttr: null_mut(),
                dwFlags: 0,
                dwInnerContentType: 0,
            };
            let ptrs = [data.as_ptr()];
            let lens = [data.len() as u32];
            let mut n = 0u32;
            if CryptSignMessage(&para, 1, 1, ptrs.as_ptr(), lens.as_ptr(), null_mut(), &mut n) == 0 {
                return Err(format!("Windows could not sign with this certificate (error {}).", std::io::Error::last_os_error()));
            }
            let mut out = vec![0u8; n as usize];
            if CryptSignMessage(&para, 1, 1, ptrs.as_ptr(), lens.as_ptr(), out.as_mut_ptr(), &mut n) == 0 {
                return Err(format!("Windows could not sign with this certificate (error {}).", std::io::Error::last_os_error()));
            }
            out.truncate(n as usize);
            Ok(out)
        }
    }
}

/// Make a self-signed certificate for trying signatures out, saved as a .pfx with `password`.
/// Its key is made in Windows' key store only long enough to export it, then deleted.
pub fn create_test_certificate(name: &str, email: &str, password: &str, out: &Path) -> Result<(), String> {
    let fail = |what: &str| format!("The certificate could not be created ({what}: {}).", std::io::Error::last_os_error());
    unsafe {
        let mut prov = 0usize;
        if NCryptOpenStorageProvider(&mut prov, MS_KEY_STORAGE_PROVIDER, 0) != 0 {
            return Err(fail("key storage"));
        }
        let key_name = wide(&format!("Dockit test key {}", annots::pdf_date().trim_start_matches("D:")));
        let mut key = 0usize;
        if NCryptCreatePersistedKey(prov, &mut key, BCRYPT_RSA_ALGORITHM, key_name.as_ptr(), 0, NCRYPT_OVERWRITE_KEY_FLAG) != 0 {
            NCryptFreeObject(prov);
            return Err(fail("new key"));
        }
        let bits: u32 = 2048;
        let policy: u32 = NCRYPT_ALLOW_EXPORT_FLAG | NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG;
        NCryptSetProperty(key, NCRYPT_LENGTH_PROPERTY, &bits as *const u32 as *const u8, 4, 0);
        NCryptSetProperty(key, NCRYPT_EXPORT_POLICY_PROPERTY, &policy as *const u32 as *const u8, 4, 0);
        let result = (|| {
            if NCryptFinalizeKey(key, 0) != 0 {
                return Err(fail("key"));
            }
            let clean = |s: &str| s.replace(['"', ',', '+', '=', ';', '<', '>'], " ");
            let mut subject = format!("CN=\"{}\"", clean(name));
            if !email.trim().is_empty() {
                subject += &format!(", E=\"{}\"", clean(email.trim()));
            }
            let x500 = wide(&subject);
            let mut n = 0u32;
            if CertStrToNameW(X509_ASN_ENCODING, x500.as_ptr(), CERT_X500_NAME_STR, null(), null_mut(), &mut n, null_mut()) == 0 {
                return Err(fail("name"));
            }
            let mut encoded = vec![0u8; n as usize];
            CertStrToNameW(X509_ASN_ENCODING, x500.as_ptr(), CERT_X500_NAME_STR, null(), encoded.as_mut_ptr(), &mut n, null_mut());
            let name_blob = CRYPT_INTEGER_BLOB { cbData: n, pbData: encoded.as_mut_ptr() };
            let mut prov_name = wide("Microsoft Software Key Storage Provider");
            let mut container = key_name.clone();
            let info = CRYPT_KEY_PROV_INFO {
                pwszContainerName: container.as_mut_ptr(),
                pwszProvName: prov_name.as_mut_ptr(),
                dwProvType: 0,
                dwFlags: 0,
                cProvParam: 0,
                rgProvParam: null_mut(),
                dwKeySpec: 0,
            };
            let alg = CRYPT_ALGORITHM_IDENTIFIER { pszObjId: SHA256_RSA.as_ptr() as *mut u8, Parameters: CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() } };
            let mut end = std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>();
            windows_sys::Win32::System::SystemInformation::GetSystemTime(&mut end);
            end.wYear += 5;
            if end.wMonth == 2 && end.wDay == 29 {
                end.wDay = 28;
            }
            let cert = CertCreateSelfSignCertificate(key, &name_blob, 0, &info, &alg, null(), &end, null());
            if cert.is_null() {
                return Err(fail("certificate"));
            }
            let store = CertOpenStore(CERT_STORE_PROV_MEMORY, 0, 0, 0, null());
            CertAddCertificateContextToStore(store, cert, CERT_STORE_ADD_ALWAYS, null_mut());
            let pw = wide(password);
            let mut blob = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
            let flags = EXPORT_PRIVATE_KEYS | REPORT_NO_PRIVATE_KEY | REPORT_NOT_ABLE_TO_EXPORT_PRIVATE_KEY;
            let mut r = Err(fail("export"));
            if PFXExportCertStoreEx(store, &mut blob, pw.as_ptr(), null(), flags) != 0 {
                let mut buf = vec![0u8; blob.cbData as usize];
                blob.pbData = buf.as_mut_ptr();
                if PFXExportCertStoreEx(store, &mut blob, pw.as_ptr(), null(), flags) != 0 {
                    buf.truncate(blob.cbData as usize);
                    r = std::fs::write(out, &buf).map_err(|e| format!("{} could not be written ({e}).", out.display()));
                }
            }
            CertFreeCertificateContext(cert);
            CertCloseStore(store, 0);
            r
        })();
        // The key now lives only in the .pfx file.
        if NCryptDeleteKey(key, 0) != 0 {
            NCryptFreeObject(key);
        }
        NCryptFreeObject(prov);
        result
    }
}

// ── checking signatures ──

#[derive(Clone, Debug)]
pub struct SigInfo {
    pub signer: String,
    pub reason: String,
    pub time: String,
    /// The signature matches the bytes it covers.
    pub valid: bool,
    /// It covers the whole file: nothing was added after signing.
    pub whole: bool,
    /// Windows trusts the certificate (it chains to a trusted root).
    pub trusted: bool,
}

/// Check every signature in a file, as its bytes are on disk.
pub fn verify_all(bytes: &[u8], doc: &Doc) -> Vec<SigInfo> {
    doc.signatures()
        .into_iter()
        .map(|s| {
            let mut info = SigInfo { signer: String::new(), reason: s.reason, time: annots::show_date(&s.time), valid: false, whole: false, trusted: false };
            let r = &s.byte_range;
            if r.len() != 4 || r.iter().any(|&v| v < 0) {
                return info;
            }
            let (a, b, c, d) = (r[0] as usize, r[1] as usize, r[2] as usize, r[3] as usize);
            if a + b > bytes.len() || c + d > bytes.len() {
                return info;
            }
            let parts = [&bytes[a..a + b], &bytes[c..c + d]];
            info.whole = a == 0 && c + d == bytes.len();
            // Trailing zeros pad the reserved space; CMS parsing ignores what follows the blob.
            let sig = &s.contents;
            unsafe {
                let para = CRYPT_VERIFY_MESSAGE_PARA {
                    cbSize: std::mem::size_of::<CRYPT_VERIFY_MESSAGE_PARA>() as u32,
                    dwMsgAndCertEncodingType: ENCODING,
                    hCryptProv: 0,
                    pfnGetSignerCertificate: None,
                    pvGetArg: null_mut(),
                };
                let ptrs = [parts[0].as_ptr(), parts[1].as_ptr()];
                let lens = [parts[0].len() as u32, parts[1].len() as u32];
                let mut signer: *mut CERT_CONTEXT = null_mut();
                info.valid = CryptVerifyDetachedMessageSignature(&para, 0, sig.as_ptr(), sig.len() as u32, 2, ptrs.as_ptr(), lens.as_ptr(), &mut signer) != 0;
                if !signer.is_null() {
                    info.signer = cert_name(signer);
                    info.trusted = chain_trusted(signer);
                    CertFreeCertificateContext(signer);
                }
            }
            info
        })
        .collect()
}

unsafe fn chain_trusted(cert: *const CERT_CONTEXT) -> bool {
    unsafe {
        let mut para = std::mem::zeroed::<CERT_CHAIN_PARA>();
        para.cbSize = std::mem::size_of::<CERT_CHAIN_PARA>() as u32;
        let mut chain: *mut CERT_CHAIN_CONTEXT = null_mut();
        if CertGetCertificateChain(null_mut(), cert, null(), null_mut(), &para, 0, null(), &mut chain) == 0 || chain.is_null() {
            return false;
        }
        let ok = (*chain).TrustStatus.dwErrorStatus == 0;
        CertFreeCertificateChain(chain);
        ok
    }
}

// ── signing the PDF ──

pub struct SignRequest {
    pub page: usize,
    /// An existing signature field to sign, or None for a new one in `rect`.
    pub field: Option<usize>,
    pub rect: [f32; 4],
    pub reason: String,
    pub location: String,
}

/// The visible part of a signature, drawn in a w × h box.
fn appearance(w: f32, h: f32, signer: &str, date: &str, reason: &str) -> lopdf::Stream {
    let mut lines = vec![format!("Digitally signed by {signer}"), format!("Date: {date}")];
    if !reason.trim().is_empty() {
        lines.push(format!("Reason: {}", reason.trim()));
    }
    let size = (h / (lines.len() as f32 * 1.35 + 0.6)).min(10.0).max(4.0);
    let mut c = format!("q 0.95 0.97 1 rg 0.04 0.52 1 RG 0.75 w 0.4 0.4 {} {} re B Q\n", w - 0.8, h - 0.8);
    c += &format!("q 1 1 {} {} re W n BT /Helv {size} Tf 0.1 0.1 0.15 rg\n", w - 2.0, h - 2.0);
    let mut y = h - size * 1.25;
    for l in &lines {
        // Long names are shortened to fit the box.
        let mut l = l.clone();
        while annots::helv_width(&l, size) > w - 8.0 && l.chars().count() > 4 {
            l.pop();
        }
        c += &format!("1 0 0 1 4 {y} Tm {} Tj\n", annots::pdf_literal(&l));
        y -= size * 1.3;
    }
    c += "ET Q\n";
    annots::form_xobject([0.0, 0.0, w, h], c)
}

/// Sign `bytes` (an unencrypted PDF). Returns the signed file.
pub fn sign_pdf(bytes: Vec<u8>, req: &SignRequest, signer: &Signer) -> Result<Vec<u8>, String> {
    let original_len = bytes.len();
    let mut u = Update::load(bytes)?;
    let page = req.page;
    let (index, rect) = match req.field {
        Some(i) => {
            let id = u.annot_id(page, i)?;
            let d = u.dict(id)?;
            let r = d.get(b"Rect").and_then(Object::as_array).map_err(|_| "The signature field has no position.".to_string())?;
            let r: Vec<f32> = r.iter().map(|o| o.as_float().unwrap_or(0.0)).collect();
            (i, [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])])
        }
        None => {
            let names = u.field_names();
            let name = (1..).map(|k| format!("Signature {k}")).find(|n| !names.contains(n)).unwrap();
            (u.add_field(page, NewField::Signature, req.rect, &name)?, req.rect)
        }
    };
    let field_id = u.annot_id(page, index)?;
    let date = annots::pdf_date();

    let mut v = Dictionary::new();
    v.set("Type", Object::Name(b"Sig".to_vec()));
    v.set("Filter", Object::Name(b"Adobe.PPKLite".to_vec()));
    v.set("SubFilter", Object::Name(b"adbe.pkcs7.detached".to_vec()));
    v.set("ByteRange", Object::Array(vec![Object::Integer(0), Object::Integer(1_000_000_000), Object::Integer(1_000_000_000), Object::Integer(1_000_000_000)]));
    v.set("Contents", Object::String(vec![0; RESERVE], StringFormat::Hexadecimal));
    v.set("M", annots::text_string(&date));
    v.set("Name", annots::text_string(&signer.name));
    if !req.reason.trim().is_empty() {
        v.set("Reason", annots::text_string(req.reason.trim()));
    }
    if !req.location.trim().is_empty() {
        v.set("Location", annots::text_string(req.location.trim()));
    }
    let v_id = u.add(Object::Dictionary(v));

    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    let ap = u.add(Object::Stream(appearance(w, h, &signer.name, &annots::show_date(&date), &req.reason)));
    let mut field = u.dict(field_id)?;
    field.set("V", Object::Reference(v_id));
    let mut apd = Dictionary::new();
    apd.set("N", Object::Reference(ap));
    field.set("AP", Object::Dictionary(apd));
    // Print, and locked so viewers do not let it be moved or changed.
    field.set("F", Object::Integer(4 | 128));
    u.put(field_id, Object::Dictionary(field));
    u.set_sig_flags()?;
    let mut out = u.finish()?;

    // Find the placeholders in the update just written.
    let find = |hay: &[u8], needle: &[u8], from: usize| hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from);
    let range_at = find(&out, RANGE_PLACEHOLDER, original_len).ok_or("The signature space could not be found.")?;
    let zeros = vec![b'0'; 64];
    let mut contents_at = None;
    let mut from = original_len;
    while let Some(p) = find(&out, b"<", from) {
        if out.len() >= p + 1 + RESERVE * 2 + 1 && out[p + 1..p + 65] == zeros[..] && out[p + 1 + RESERVE * 2] == b'>' {
            contents_at = Some(p);
            break;
        }
        from = p + 1;
    }
    let a = contents_at.ok_or("The signature space could not be found.")?;
    let b = a + RESERVE * 2 + 2;
    let range = format!("[0 {a} {b} {}", out.len() - b);
    let mut patched = range.into_bytes();
    patched.resize(RANGE_PLACEHOLDER.len() - 1, b' ');
    patched.push(b']');
    out[range_at..range_at + RANGE_PLACEHOLDER.len()].copy_from_slice(&patched);

    let mut data = Vec::with_capacity(out.len() - (b - a));
    data.extend_from_slice(&out[..a]);
    data.extend_from_slice(&out[b..]);
    let sig = signer.sign(&data)?;
    if sig.len() > RESERVE {
        return Err("The signature is larger than the space kept for it.".into());
    }
    let hex: Vec<u8> = sig.iter().flat_map(|x| format!("{x:02X}").into_bytes()).collect();
    out[a + 1..a + 1 + hex.len()].copy_from_slice(&hex);
    Ok(out)
}

// ── the dialog ──

pub struct SignDialog {
    pub pfx: Option<PathBuf>,
    password: String,
    reason: String,
    location: String,
    /// Signature fields not yet signed: (page, index, name).
    fields: Vec<(usize, usize, String)>,
    /// Chosen field, or None for a new box on the current page.
    choice: Option<usize>,
    making: Option<NewCert>,
    error: Option<String>,
}

struct NewCert {
    name: String,
    email: String,
    password: String,
    again: String,
}

impl SignDialog {
    pub fn new(ed: &Editor, preferred: Option<(usize, usize)>) -> Self {
        let mut fields = Vec::new();
        if let Some(f) = &ed.file {
            for page in 0..f.page_count() {
                for a in f.annots(page).iter() {
                    if let Some(fd) = &a.field
                        && fd.kind == crate::pdf::FIELD_SIGNATURE
                        && fd.value.is_empty()
                    {
                        fields.push((page, a.index, fd.name.clone()));
                    }
                }
            }
        }
        let choice = preferred.and_then(|(p, i)| fields.iter().position(|f| f.0 == p && f.1 == i)).or(if fields.is_empty() { None } else { Some(0) });
        SignDialog {
            pfx: ed.style.pfx.clone().filter(|p| p.exists()),
            password: String::new(),
            reason: String::new(),
            location: String::new(),
            fields,
            choice,
            making: None,
            error: None,
        }
    }
}

/// Returns false when the dialog is done.
pub fn sign_window(ctx: &egui::Context, ed: &mut Editor, d: &mut SignDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let mut go = false;
    let mut make = false;
    let mut cancel_new = false;
    let (_, close) = modal(ctx, "sign-digitally", 440.0, |ui| {
        title(ui, "Sign digitally");
        ui.label(theme::secondary(ui, "A digital signature proves who signed and shows if the document changes afterwards."));
        theme::heading(ui, "CERTIFICATE");
        match &d.pfx {
            Some(path) => {
                ui.label(path.file_name().unwrap_or_default().to_string_lossy());
            }
            None => {
                ui.label(theme::secondary(ui, "No certificate chosen"));
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Choose .pfx file…").clicked()
                && let Some(f) = rfd::FileDialog::new().add_filter("Certificate", &["pfx", "p12"]).pick_file()
            {
                d.pfx = Some(f);
                d.error = None;
            }
            if d.making.is_none() && ui.button("Create a test certificate…").clicked() {
                d.making = Some(NewCert { name: whoami(), email: String::new(), password: String::new(), again: String::new() });
            }
        });
        if let Some(n) = &mut d.making {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(theme::secondary(ui, "A self-signed certificate, for trying signatures out. Readers will show it as not trusted."));
                labeled(ui, "Name", &mut n.name, false);
                labeled(ui, "Email", &mut n.email, false);
                labeled(ui, "Password", &mut n.password, true);
                labeled(ui, "Again", &mut n.again, true);
                ui.horizontal(|ui| {
                    if ui.button("Create and save…").clicked() {
                        make = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_new = true;
                    }
                });
            });
        }
        if d.pfx.is_some() && d.making.is_none() {
            ui.add(egui::TextEdit::singleline(&mut d.password).password(true).desired_width(f32::INFINITY).hint_text("Certificate password"));
        }
        theme::heading(ui, "WHERE");
        let here = format!("A new box at the bottom of page {}", ed.current + 1);
        let current = d.choice.and_then(|k| d.fields.get(k)).map(|f| format!("{} (page {})", f.2, f.0 + 1)).unwrap_or(here.clone());
        egui::ComboBox::from_id_salt("sign-where").selected_text(current).width(ui.available_width() - 8.0).show_ui(ui, |ui| {
            for (k, f) in d.fields.iter().enumerate() {
                ui.selectable_value(&mut d.choice, Some(k), format!("{} (page {})", f.2, f.0 + 1));
            }
            ui.selectable_value(&mut d.choice, None, here);
        });
        theme::heading(ui, "DETAILS");
        labeled(ui, "Reason", &mut d.reason, false);
        labeled(ui, "Location", &mut d.location, false);
        if ed.file.as_ref().is_some_and(|f| matches!(f.security, crate::security::Security::Protect(_)) || f.doc.encrypted()) {
            ui.label(RichText::new("The signed copy is saved without password protection.").color(p.warning));
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(p.danger));
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, "Sign and save as…", d.pfx.is_some() && d.making.is_none()) {
                go = true;
            }
            if secondary_button(ui, "Cancel") {
                open = false;
            }
        });
    });
    if close {
        open = false;
    }
    if cancel_new {
        d.making = None;
        d.error = None;
    }
    if make && let Some(n) = &d.making {
        if n.name.trim().is_empty() {
            d.error = Some("Type the name to show on signatures.".into());
        } else if n.password.is_empty() || n.password != n.again {
            d.error = Some("Type the same password twice; it protects the certificate file.".into());
        } else if let Some(out) = rfd::FileDialog::new().add_filter("Certificate", &["pfx"]).set_file_name(format!("{}.pfx", n.name.trim())).save_file() {
            match create_test_certificate(n.name.trim(), &n.email, &n.password, &out) {
                Ok(()) => {
                    d.password = n.password.clone();
                    d.pfx = Some(out);
                    d.making = None;
                    d.error = None;
                }
                Err(e) => d.error = Some(e),
            }
        }
    }
    if go && let Some(pfx) = d.pfx.clone() {
        match sign_now(ed, d, &pfx) {
            Ok(true) => return false,
            Ok(false) => {}
            Err(e) => d.error = Some(e),
        }
    }
    open
}

fn whoami() -> String {
    annots::author()
}

fn labeled(ui: &mut Ui, label: &str, s: &mut String, password: bool) {
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 20.0], egui::Label::new(label));
        ui.add(egui::TextEdit::singleline(s).password(password).desired_width(f32::INFINITY));
    });
}

/// Sign and write the copy. Ok(false) when the save dialog was cancelled.
fn sign_now(ed: &mut Editor, d: &SignDialog, pfx: &Path) -> Result<bool, String> {
    let signer = Signer::load(pfx, &d.password)?;
    ed.commit_edits();
    ed.commit();
    let f = ed.file.as_ref().ok_or("No document is open.")?;
    let (page, field, rect) = match d.choice.and_then(|k| d.fields.get(k)) {
        Some(&(page, index, _)) => (page, Some(index), [0.0; 4]),
        None => {
            let page = ed.current;
            let cb = f.doc.page(page).map(|p| p.crop_box()).ok_or("The page could not be loaded.")?;
            let (w, h) = (200.0, 60.0);
            (page, None, [cb[2] - 36.0 - w, cb[1] + 36.0, cb[2] - 36.0, cb[1] + 36.0 + h])
        }
    };
    let stem = f.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into());
    let mut dlg = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{stem} (signed).pdf"));
    if let Some(dir) = f.path.as_ref().and_then(|p| p.parent()) {
        dlg = dlg.set_directory(dir);
    }
    let Some(out) = dlg.save_file() else { return Ok(false) };
    let req = SignRequest { page, field, rect, reason: d.reason.clone(), location: d.location.clone() };
    let signed = sign_pdf(f.doc.save_plain()?, &req, &signer)?;
    std::fs::write(&out, &signed).map_err(|e| format!("{} could not be written ({e}).", out.display()))?;
    ed.style.pfx = Some(pfx.to_path_buf());
    ed.open_path(&out);
    ed.say(format!("Signed by {} and saved as {}", signer.name, out.file_name().unwrap_or_default().to_string_lossy()));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes a test certificate, signs a PDF with it and checks the signature (run with --ignored;
    /// needs pdfium.dll in target/debug).
    #[test]
    #[ignore]
    fn sign_and_verify() {
        crate::pdf::init(Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug").as_path()).unwrap();
        let dir = std::env::temp_dir().join("dockit-sign-test");
        let _ = std::fs::create_dir_all(&dir);
        let pfx = dir.join("test.pfx");
        create_test_certificate("Dockit Tester", "tester@example.com", "pw", &pfx).unwrap();
        let signer = Signer::load(&pfx, "pw").unwrap();
        assert_eq!(signer.name, "Dockit Tester");
        assert!(Signer::load(&pfx, "wrong").is_err());

        let doc = Doc::new_empty();
        assert!(doc.insert_blank(0, 300.0, 300.0));
        let req = SignRequest { page: 0, field: None, rect: [50.0, 50.0, 250.0, 110.0], reason: "Testing".into(), location: String::new() };
        let signed = sign_pdf(doc.save().unwrap(), &req, &signer).unwrap();
        let back = Doc::open(signed.clone(), None).unwrap();
        let sigs = verify_all(&signed, &back);
        assert_eq!(sigs.len(), 1);
        assert!(sigs[0].valid && sigs[0].whole, "{:?}", sigs[0]);
        assert_eq!(sigs[0].signer, "Dockit Tester");
        assert!(!sigs[0].trusted);

        // Any change to the signed bytes breaks it.
        let mut tampered = signed.clone();
        let k = tampered.windows(3).position(|w| w == b"300").expect("page size in the file");
        tampered[k] = b'2';
        let back = Doc::open(tampered.clone(), None);
        if let Ok(back) = back {
            assert!(verify_all(&tampered, &back).iter().all(|s| !s.valid));
        }
        std::fs::write(dir.join("signed.pdf"), &signed).unwrap();
    }
}
