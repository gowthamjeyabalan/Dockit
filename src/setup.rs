//! Built-in setup: installing Dockit for the current user (no administrator rights and no other
//! installer needed), registering it for PDF files, uninstalling it again, and handing files
//! opened from Explorer to a Dockit that is already running.

use crate::dialogs::{modal, primary, secondary_button, title};
use crate::theme::{self, pal};
use egui::RichText;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::System::Registry::*;

const PROG_ID: &str = "Dockit.PDF";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Dockit";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Where Dockit is installed for this user.
pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join("Dockit")
}

fn installed_exe() -> PathBuf {
    install_dir().join("Dockit.exe")
}

/// Is this the installed copy of Dockit?
pub fn running_installed() -> bool {
    let here = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let there = installed_exe().canonicalize().ok();
    here.is_some() && here == there
}

pub fn is_installed() -> bool {
    installed_exe().exists()
}

fn start_menu() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    base.join(r"Microsoft\Windows\Start Menu\Programs\Dockit.lnk")
}

fn desktop() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    base.join(r"Desktop\Dockit.lnk")
}

// ── registry ──

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

impl Key {
    fn create(path: &str) -> Result<Key, String> {
        let mut h: HKEY = null_mut();
        let p = wide(path);
        let r = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, p.as_ptr(), 0, null(), 0, KEY_WRITE, null(), &mut h, null_mut()) };
        if r != 0 {
            return Err(format!("The registry key {path} could not be written (error {r})."));
        }
        Ok(Key(h))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let n = wide(name);
        let v = wide(value);
        let r = unsafe { RegSetValueExW(self.0, if name.is_empty() { null() } else { n.as_ptr() }, 0, REG_SZ, v.as_ptr() as *const u8, (v.len() * 2) as u32) };
        if r != 0 { Err(format!("A registry value could not be written (error {r}).")) } else { Ok(()) }
    }

    fn set_dword(&self, name: &str, value: u32) -> Result<(), String> {
        let n = wide(name);
        let r = unsafe { RegSetValueExW(self.0, n.as_ptr(), 0, REG_DWORD, &value as *const u32 as *const u8, 4) };
        if r != 0 { Err(format!("A registry value could not be written (error {r}).")) } else { Ok(()) }
    }
}

fn delete_tree(path: &str) {
    let p = wide(path);
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, p.as_ptr()) };
}

fn delete_value(path: &str, name: &str) {
    let (p, n) = (wide(path), wide(name));
    let mut h: HKEY = null_mut();
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, p.as_ptr(), 0, KEY_WRITE, &mut h) == 0 {
            RegDeleteValueW(h, n.as_ptr());
            RegCloseKey(h);
        }
    }
}

/// Register `exe` as a program that opens PDF files (it then shows in "Open with" and in Default
/// apps; Windows lets only the user make it the default).
fn register(exe: &Path) -> Result<(), String> {
    let exe = exe.display().to_string();
    let open = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    Key::create(&format!(r"Software\Classes\{PROG_ID}"))?.set("", "PDF Document")?;
    Key::create(&format!(r"Software\Classes\{PROG_ID}\DefaultIcon"))?.set("", &icon)?;
    Key::create(&format!(r"Software\Classes\{PROG_ID}\shell\open\command"))?.set("", &open)?;
    Key::create(r"Software\Classes\.pdf\OpenWithProgids")?.set(PROG_ID, "")?;
    let app = Key::create(r"Software\Classes\Applications\Dockit.exe")?;
    app.set("FriendlyAppName", "Dockit")?;
    Key::create(r"Software\Classes\Applications\Dockit.exe\SupportedTypes")?.set(".pdf", "")?;
    Key::create(r"Software\Classes\Applications\Dockit.exe\shell\open\command")?.set("", &open)?;
    let caps = Key::create(r"Software\Dockit\Capabilities")?;
    caps.set("ApplicationName", "Dockit")?;
    caps.set("ApplicationDescription", "Edit, combine, split, sign and fill in PDFs.")?;
    caps.set("ApplicationIcon", &icon)?;
    Key::create(r"Software\Dockit\Capabilities\FileAssociations")?.set(".pdf", PROG_ID)?;
    Key::create(r"Software\RegisteredApplications")?.set("Dockit", r"Software\Dockit\Capabilities")?;
    unsafe {
        windows_sys::Win32::UI::Shell::SHChangeNotify(windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32, windows_sys::Win32::UI::Shell::SHCNF_IDLIST, null(), null());
    }
    Ok(())
}

fn unregister() {
    delete_tree(&format!(r"Software\Classes\{PROG_ID}"));
    delete_value(r"Software\Classes\.pdf\OpenWithProgids", PROG_ID);
    delete_tree(r"Software\Classes\Applications\Dockit.exe");
    delete_tree(r"Software\Dockit");
    delete_value(r"Software\RegisteredApplications", "Dockit");
    unsafe {
        windows_sys::Win32::UI::Shell::SHChangeNotify(windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32, windows_sys::Win32::UI::Shell::SHCNF_IDLIST, null(), null());
    }
}

/// A shortcut (.lnk) to `target` at `at`.
fn shortcut(target: &Path, at: &Path) -> Result<(), String> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};
    let err = |e: windows::core::Error| format!("The shortcut could not be made ({}).", e.message());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(err)?;
        link.SetPath(&HSTRING::from(target.as_os_str())).map_err(err)?;
        link.SetDescription(&HSTRING::from("Edit, combine, split, sign and fill in PDFs")).map_err(err)?;
        link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0).map_err(err)?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())).map_err(err)?;
        }
        let file: IPersistFile = link.cast().map_err(err)?;
        if let Some(parent) = at.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        file.Save(&HSTRING::from(at.as_os_str()), true).map_err(err)?;
    }
    Ok(())
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map(|it| it.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0)
}

/// Install this copy of Dockit for the current user. Returns the installed program.
pub fn install(desktop_shortcut: bool) -> Result<PathBuf, String> {
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let src_dir = src.parent().ok_or("Dockit's folder could not be found.")?.to_path_buf();
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} could not be created ({e}).", dir.display()))?;
    let exe = installed_exe();
    if !running_installed() {
        // An older installed copy that is running cannot be replaced; say so plainly.
        std::fs::copy(&src, &exe).map_err(|e| format!("Dockit could not be copied to {} ({e}). Close any other Dockit window and try again.", dir.display()))?;
        let dll = src_dir.join("pdfium.dll");
        std::fs::copy(&dll, dir.join("pdfium.dll")).map_err(|e| format!("pdfium.dll could not be copied ({e})."))?;
    }
    shortcut(&exe, &start_menu())?;
    if desktop_shortcut {
        shortcut(&exe, &desktop())?;
    }
    register(&exe)?;
    let u = Key::create(UNINSTALL_KEY)?;
    u.set("DisplayName", "Dockit")?;
    u.set("DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    u.set("Publisher", env!("CARGO_PKG_AUTHORS"))?;
    u.set("DisplayIcon", &format!("\"{}\",0", exe.display()))?;
    u.set("InstallLocation", &dir.display().to_string())?;
    u.set("UninstallString", &format!("\"{}\" --uninstall", exe.display()))?;
    u.set_dword("NoModify", 1)?;
    u.set_dword("NoRepair", 1)?;
    u.set_dword("EstimatedSize", (dir_size(&dir) / 1024) as u32)?;
    Ok(exe)
}

/// Remove Dockit: shortcuts, registration and its folder (once this program has exited). The
/// user's settings are kept.
pub fn uninstall() {
    unregister();
    delete_tree(UNINSTALL_KEY);
    let _ = std::fs::remove_file(start_menu());
    let _ = std::fs::remove_file(desktop());
    let dir = install_dir();
    if dir.exists() {
        // The running program cannot delete itself: a hidden command does it a moment later.
        use std::os::windows::process::CommandExt;
        // raw_arg: cmd needs the quotes exactly as written.
        let _ = std::process::Command::new("cmd")
            .raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()))
            .creation_flags(0x0800_0000)
            .spawn();
    }
}

/// Open Windows' page for choosing Dockit as the default PDF app.
pub fn open_default_apps() {
    let _ = std::process::Command::new("explorer").arg("ms-settings:defaultapps?registeredAppUser=Dockit").spawn();
}

// ── one window for many files ──

fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default().replace(['\\', '/'], "_");
    format!(r"\\.\pipe\Dockit-{user}")
}

/// Hand `path` to a Dockit that is already running. True if one took it.
pub fn forward(path: &Path) -> bool {
    use std::io::Write;
    match std::fs::OpenOptions::new().write(true).open(pipe_name()) {
        Ok(mut p) => p.write_all(path.as_os_str().to_string_lossy().as_bytes()).is_ok(),
        Err(_) => false,
    }
}

/// Listen for files handed over by later launches; each arrives on the returned channel.
pub fn listen(ctx: egui::Context) -> std::sync::mpsc::Receiver<PathBuf> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_INBOUND, ReadFile};
    use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
    let (tx, rx) = std::sync::mpsc::channel();
    let name = wide(&pipe_name());
    std::thread::spawn(move || {
        loop {
            let h = unsafe { CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES, 0, 8192, 0, null()) };
            if h == INVALID_HANDLE_VALUE {
                return;
            }
            let connected = unsafe { ConnectNamedPipe(h, null_mut()) } != 0 || std::io::Error::last_os_error().raw_os_error() == Some(535);
            if connected {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let mut n = 0u32;
                    if unsafe { ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, &mut n, null_mut()) } == 0 || n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n as usize]);
                }
                let path = PathBuf::from(String::from_utf8_lossy(&data).into_owned());
                if !path.as_os_str().is_empty() && tx.send(path).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            unsafe {
                DisconnectNamedPipe(h);
                CloseHandle(h);
            }
        }
    });
    rx
}

// ── the setup window ──

pub struct SetupDialog {
    desktop: bool,
    done: Option<PathBuf>,
    error: Option<String>,
}

impl SetupDialog {
    pub fn new() -> Self {
        SetupDialog { desktop: true, done: None, error: None }
    }
}

/// Returns false when it closes.
pub fn setup_window(ctx: &egui::Context, d: &mut SetupDialog) -> bool {
    let p = pal(ctx);
    let mut open = true;
    let (_, close) = modal(ctx, "setup", 420.0, |ui| {
        match &d.done {
            None => {
                title(ui, "Install Dockit");
                ui.label(theme::secondary(ui, format!("Dockit is installed for you only, in {}. No administrator rights are needed.", install_dir().display())));
                ui.add_space(6.0);
                ui.label("• A Start menu shortcut");
                ui.label("• Dockit in \"Open with\" for PDF files");
                ui.label("• An entry in Settings > Apps, to uninstall it");
                ui.add_space(4.0);
                ui.checkbox(&mut d.desktop, "Also put a shortcut on the desktop");
                if let Some(e) = &d.error {
                    ui.label(RichText::new(e).color(p.danger));
                }
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary(ui, if is_installed() { "Install again" } else { "Install" }, true) {
                        match install(d.desktop) {
                            Ok(exe) => {
                                d.done = Some(exe);
                                d.error = None;
                            }
                            Err(e) => d.error = Some(e),
                        }
                    }
                    if secondary_button(ui, "Cancel") {
                        open = false;
                    }
                });
            }
            Some(exe) => {
                title(ui, "Dockit is installed");
                ui.label("To open PDF files with Dockit when you double-click them, choose Dockit in Windows' Default apps. Windows asks you to make that choice yourself.");
                ui.add_space(12.0);
                let exe = exe.clone();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary(ui, "Choose default app…", true) {
                        open_default_apps();
                    }
                    if !running_installed() && ui.button("Start installed Dockit").clicked() {
                        let _ = std::process::Command::new(&exe).spawn();
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        open = false;
                    }
                    if secondary_button(ui, "Done") {
                        open = false;
                    }
                });
            }
        }
    });
    if close {
        open = false;
    }
    open
}
