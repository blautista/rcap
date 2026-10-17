//! Puts a file on the clipboard the way Explorer's "Copy" does (CF_HDROP),
//! which is what Discord, Slack, Teams, browsers and Explorer accept for
//! pasting an animated GIF.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::{mem, ptr, thread, time::Duration};

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::System::Ole::CF_HDROP;
use windows_sys::Win32::UI::Shell::DROPFILES;

pub fn copy_file(hwnd: HWND, path: &Path) -> bool {
    unsafe {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
        let header = mem::size_of::<DROPFILES>();
        let drop = GlobalAlloc(GMEM_MOVEABLE, header + wide.len() * 2);
        if drop.is_null() {
            return false;
        }
        let p = GlobalLock(drop) as *mut u8;
        ptr::write_unaligned(
            p as *mut DROPFILES,
            DROPFILES {
                pFiles: header as u32,
                pt: POINT { x: 0, y: 0 },
                fNC: 0,
                fWide: 1,
            },
        );
        ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, p.add(header), wide.len() * 2);
        GlobalUnlock(drop);

        let effect = GlobalAlloc(GMEM_MOVEABLE, 4);
        *(GlobalLock(effect) as *mut u32) = 1; // DROPEFFECT_COPY
        GlobalUnlock(effect);
        let effect_fmt = RegisterClipboardFormatW(crate::wide("Preferred DropEffect").as_ptr());

        // Another app may briefly hold the clipboard.
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(hwnd) != 0 {
                opened = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if !opened {
            return false;
        }
        EmptyClipboard();
        let ok = !SetClipboardData(CF_HDROP as u32, drop).is_null();
        SetClipboardData(effect_fmt, effect);
        CloseClipboard();
        ok
    }
}
