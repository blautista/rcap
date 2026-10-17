use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{mem, ptr};

use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub enum Msg {
    /// Milliseconds since start, BGRA pixels (top-down).
    Frame(u32, Vec<u8>),
    /// Milliseconds since start at which recording stopped.
    End(u32),
}

#[derive(Clone, Copy)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub fn run(
    r: Region,
    fps: u32,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicUsize>,
    tx: SyncSender<Msg>,
) {
    unsafe {
        timeBeginPeriod(1);
        let screen = GetDC(ptr::null_mut());
        let mem_dc = CreateCompatibleDC(screen);

        let mut bmi: BITMAPINFO = mem::zeroed();
        bmi.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = r.w;
        bmi.bmiHeader.biHeight = -r.h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits = ptr::null_mut();
        let dib = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
        let old = SelectObject(mem_dc, dib);
        let len = (r.w * r.h * 4) as usize;

        let interval = Duration::from_secs_f64(1.0 / fps as f64);
        let start = Instant::now();
        let mut next = start;
        while !stop.load(Ordering::Relaxed) {
            let t = start.elapsed().as_millis() as u32;
            // No CAPTUREBLT: layered windows (our own frame) are excluded.
            BitBlt(mem_dc, 0, 0, r.w, r.h, screen, r.x, r.y, SRCCOPY);
            draw_cursor(mem_dc, r.x, r.y);
            GdiFlush();
            let data = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
            if tx.send(Msg::Frame(t, data)).is_err() {
                break;
            }
            frames.fetch_add(1, Ordering::Relaxed);

            next += interval;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
        let _ = tx.send(Msg::End(start.elapsed().as_millis() as u32));

        SelectObject(mem_dc, old);
        DeleteObject(dib);
        DeleteDC(mem_dc);
        ReleaseDC(ptr::null_mut(), screen);
        timeEndPeriod(1);
    }
}

unsafe fn draw_cursor(dc: HDC, ox: i32, oy: i32) {
    let mut ci: CURSORINFO = mem::zeroed();
    ci.cbSize = mem::size_of::<CURSORINFO>() as u32;
    if GetCursorInfo(&mut ci) == 0 || ci.flags & CURSOR_SHOWING == 0 {
        return;
    }
    let mut ii: ICONINFO = mem::zeroed();
    if GetIconInfo(ci.hCursor, &mut ii) == 0 {
        return;
    }
    DrawIconEx(
        dc,
        ci.ptScreenPos.x - ox - ii.xHotspot as i32,
        ci.ptScreenPos.y - oy - ii.yHotspot as i32,
        ci.hCursor,
        0,
        0,
        0,
        ptr::null_mut(),
        DI_NORMAL,
    );
    if !ii.hbmMask.is_null() {
        DeleteObject(ii.hbmMask);
    }
    if !ii.hbmColor.is_null() {
        DeleteObject(ii.hbmColor);
    }
}
