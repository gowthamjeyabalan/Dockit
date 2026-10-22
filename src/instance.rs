//! One window for many files: a PDF opened from Explorer while Dockit is running is handed to
//! the running Dockit (over a named pipe) and opens there as a new tab.

use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

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
