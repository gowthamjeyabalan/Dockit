//! Printing through Windows: the system print dialog picks the printer, pages and copies; each
//! page is then drawn at the printer's resolution, fitted to the printable area (turned when that
//! fits better), and sent one per frame so the window stays responsive.

use crate::editor::{Editor, plural};
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows_sys::Win32::UI::Controls::Dialogs::*;

/// Largest side, in pixels, a page is drawn at for printing (keeps memory in check).
const MAX_SIDE: f32 = 7000.0;

pub struct PrintJob {
    hdc: HDC,
    pages: Vec<usize>,
    next: usize,
    failed: bool,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Drop for PrintJob {
    fn drop(&mut self) {
        unsafe {
            if self.next < self.pages.len() || self.failed {
                AbortDoc(self.hdc);
            }
            DeleteDC(self.hdc);
        }
    }
}

/// Show the print dialog. None when it is cancelled (or fails, with the reason in `ed.error`).
pub fn begin(ed: &mut Editor) -> Option<PrintJob> {
    let f = ed.file.as_ref()?;
    let name = f.name();
    let n = f.page_count().min(u16::MAX as usize) as u16;
    unsafe {
        let mut pd: PRINTDLGW = std::mem::zeroed();
        pd.lStructSize = std::mem::size_of::<PRINTDLGW>() as u32;
        pd.hwndOwner = windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow();
        pd.Flags = PD_RETURNDC | PD_USEDEVMODECOPIESANDCOLLATE | PD_NOSELECTION;
        pd.nMinPage = 1;
        pd.nMaxPage = n.max(1);
        pd.nFromPage = 1;
        pd.nToPage = n.max(1);
        pd.nCopies = 1;
        let ok = PrintDlgW(&mut pd) != 0;
        if !pd.hDevMode.is_null() {
            GlobalFree(pd.hDevMode);
        }
        if !pd.hDevNames.is_null() {
            GlobalFree(pd.hDevNames);
        }
        if !ok || pd.hDC.is_null() {
            let e = CommDlgExtendedError();
            if e != 0 {
                ed.error = Some(format!("The print dialog could not open (error {e:#x})."));
            }
            return None;
        }
        let pages: Vec<usize> = if pd.Flags & PD_PAGENUMS != 0 {
            let (a, b) = (pd.nFromPage.min(pd.nToPage).max(1), pd.nToPage.max(pd.nFromPage).min(n));
            (a as usize - 1..b as usize).collect()
        } else {
            (0..n as usize).collect()
        };
        let name = wide(&name);
        let di = DOCINFOW { cbSize: std::mem::size_of::<DOCINFOW>() as i32, lpszDocName: name.as_ptr(), lpszOutput: std::ptr::null(), lpszDatatype: std::ptr::null(), fwType: 0 };
        if StartDocW(pd.hDC, &di) <= 0 {
            DeleteDC(pd.hDC);
            return None;
        }
        Some(PrintJob { hdc: pd.hDC, pages, next: 0, failed: false })
    }
}

impl PrintJob {
    /// "3 of 10"
    pub fn progress(&self) -> String {
        format!("{} of {}", (self.next + 1).min(self.pages.len()), self.pages.len())
    }

    /// Print the next page. Returns false when the job is finished (or failed).
    pub fn step(&mut self, ed: &mut Editor) -> bool {
        let Some(&page) = self.pages.get(self.next) else {
            unsafe { EndDoc(self.hdc) };
            self.next = self.pages.len();
            ed.say(format!("Sent {} to the printer", plural(self.pages.len(), "page", "pages")));
            return false;
        };
        let Some(f) = &ed.file else {
            self.failed = true;
            return false;
        };
        let printed = unsafe {
            let (pw, ph) = (GetDeviceCaps(self.hdc, HORZRES as i32) as f32, GetDeviceCaps(self.hdc, VERTRES as i32) as f32);
            let (dx, dy) = (GetDeviceCaps(self.hdc, LOGPIXELSX as i32) as f32, GetDeviceCaps(self.hdc, LOGPIXELSY as i32) as f32);
            let (w, h) = f.sizes[page];
            // Turn landscape pages on portrait paper (and the other way round).
            let turn = (w > h) != (pw > ph);
            let (w, h) = if turn { (h, w) } else { (w, h) };
            // Actual size when it fits, otherwise shrunk to fit.
            let fit = (pw / (w / 72.0 * dx)).min(ph / (h / 72.0 * dy)).min(1.0);
            let (dw, dh) = (w / 72.0 * dx * fit, h / 72.0 * dy * fit);
            // Draw at the printer's resolution, capped.
            let k = (MAX_SIDE / dw.max(dh)).min(1.0);
            let (bw, bh) = ((dw * k).round().max(1.0) as i32, (dh * k).round().max(1.0) as i32);
            let Some(px) = f.doc.page(page).and_then(|p| p.render_print(bw, bh, if turn { 1 } else { 0 })) else {
                self.failed = true;
                return false;
            };
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = bw;
            bmi.bmiHeader.biHeight = -bh;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            StartPage(self.hdc) > 0
                && StretchDIBits(
                    self.hdc,
                    ((pw - dw) / 2.0).round() as i32,
                    ((ph - dh) / 2.0).round() as i32,
                    dw.round() as i32,
                    dh.round() as i32,
                    0,
                    0,
                    bw,
                    bh,
                    px.as_ptr() as *const _,
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                ) != 0
                && EndPage(self.hdc) > 0
        };
        if !printed {
            self.failed = true;
            ed.error = Some("The printer stopped taking pages. Check that it is ready, then print again.".into());
            return false;
        }
        self.next += 1;
        true
    }
}
