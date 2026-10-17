#![windows_subsystem = "windows"]

mod capture;
mod clipboard;
mod encode;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::{mem, ptr, thread};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use capture::Region;

// Frame metrics at 96 DPI. We draw our own frame: color-key hit-testing is
// unreliable with the system frame, and a window region (what LICEcap uses)
// makes the window flicker transparent during live resize.
const TITLEBAR: i32 = 26;
const BORDER: i32 = 6;
const TOOLBAR: i32 = 36;
const GRIP: i32 = 12;
const DEFAULT_FPS: u32 = 30;
const DEFAULT_SIZE: (i32, i32) = (640, 360);

const ID_REC: u32 = 101;
const ID_FPS: u32 = 102;
const ID_W: u32 = 103;
const ID_H: u32 = 104;
const ID_CLOSE: u32 = 105;
const ID_OK: u32 = 1; // IDOK, sent by IsDialogMessage on Enter
const TIMER: usize = 1;
// Static control styles (avoids pulling in Win32_UI_Controls).
const SS_CENTER: u32 = 0x1;
const SS_RIGHT: u32 = 0x2;
const SS_CENTERIMAGE: u32 = 0x200;
const WM_DONE: u32 = WM_APP + 1;

type EncodeResult = Result<(PathBuf, u64), String>;

#[derive(Clone, Copy)]
struct Ui {
    hwnd: HWND,
    rec: HWND,
    lbl_fps: HWND,
    fps: HWND,
    w: HWND,
    lbl_x: HWND,
    h: HWND,
    close: HWND,
    font: HFONT,
    dpi: u32,
}

enum State {
    Idle,
    Recording { stop: Arc<AtomicBool>, frames: Arc<AtomicUsize>, start: Instant },
    Encoding,
}

thread_local! {
    static UI: Cell<Option<Ui>> = const { Cell::new(None) };
    static STATE: RefCell<State> = const { RefCell::new(State::Idle) };
}

fn ui() -> Option<Ui> {
    UI.with(|u| u.get())
}

fn busy() -> bool {
    STATE.with(|s| !matches!(*s.borrow(), State::Idle))
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn scale(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

fn style() -> u32 {
    // WS_THICKFRAME is required for edge resizing; WM_NCCALCSIZE hides its border.
    WS_POPUP | WS_THICKFRAME | WS_CLIPCHILDREN | WS_SYSMENU | WS_MINIMIZEBOX
}

const EX_STYLE: u32 = WS_EX_TOPMOST | WS_EX_APPWINDOW | WS_EX_LAYERED;
/// The hole is painted this color, which the layered window makes see-through
/// and click-through. Capture uses BitBlt without CAPTUREBLT, which skips
/// layered windows, so the recording shows what's underneath.
const KEY: COLORREF = 0x00FF00FF;

fn main() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinst = GetModuleHandleW(ptr::null());
        let class = wide("rcap");

        let mut wc: WNDCLASSW = mem::zeroed();
        wc.style = CS_HREDRAW | CS_VREDRAW;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.hCursor = LoadCursorW(ptr::null_mut(), IDC_ARROW);
        wc.hIcon = LoadIconW(ptr::null_mut(), IDI_APPLICATION);
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            EX_STYLE,
            class.as_ptr(),
            wide("rcap").as_ptr(),
            style(),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            800,
            600,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        );

        SetLayeredWindowAttributes(hwnd, KEY, 255, LWA_COLORKEY);
        let dpi = GetDpiForWindow(hwnd);
        let edit = WS_TABSTOP | ES_NUMBER as u32 | ES_CENTER as u32;
        let label = SS_CENTERIMAGE as u32;
        let u = Ui {
            hwnd,
            rec: child(hwnd, "BUTTON", "Record", WS_TABSTOP | BS_PUSHBUTTON as u32, 0, ID_REC),
            lbl_fps: child(hwnd, "STATIC", "FPS", label | SS_RIGHT as u32, 0, 0),
            fps: child(hwnd, "EDIT", &DEFAULT_FPS.to_string(), edit, WS_EX_CLIENTEDGE, ID_FPS),
            w: child(hwnd, "EDIT", "", edit, WS_EX_CLIENTEDGE, ID_W),
            lbl_x: child(hwnd, "STATIC", "×", label | SS_CENTER as u32, 0, 0),
            h: child(hwnd, "EDIT", "", edit, WS_EX_CLIENTEDGE, ID_H),
            close: child(hwnd, "BUTTON", "✕", BS_PUSHBUTTON as u32, 0, ID_CLOSE),
            font: make_font(dpi),
            dpi,
        };
        UI.with(|c| c.set(Some(u)));
        apply_font(&u);
        set_capture_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1);
        ShowWindow(hwnd, SW_SHOW);

        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe fn child(parent: HWND, class: &str, text: &str, style: u32, ex: u32, id: u32) -> HWND {
    CreateWindowExW(
        ex,
        wide(class).as_ptr(),
        wide(text).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        0,
        0,
        0,
        0,
        parent,
        id as usize as HMENU,
        GetModuleHandleW(ptr::null()),
        ptr::null(),
    )
}

unsafe fn make_font(dpi: u32) -> HFONT {
    let mut ncm: NONCLIENTMETRICSW = mem::zeroed();
    ncm.cbSize = mem::size_of::<NONCLIENTMETRICSW>() as u32;
    SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, ncm.cbSize, &mut ncm as *mut _ as *mut _, 0, dpi);
    CreateFontIndirectW(&ncm.lfMessageFont)
}

unsafe fn apply_font(u: &Ui) {
    for c in [u.rec, u.lbl_fps, u.fps, u.w, u.lbl_x, u.h, u.close] {
        SendMessageW(c, WM_SETFONT, u.font as WPARAM, 1);
    }
}

unsafe fn get_text(h: HWND) -> String {
    let mut buf = [0u16; 32];
    let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn set_text(h: HWND, s: &str) {
    SetWindowTextW(h, wide(s).as_ptr());
}

unsafe fn get_num(h: HWND) -> Option<i32> {
    get_text(h).trim().parse().ok()
}

/// Resize the window so the see-through hole is exactly w x h physical pixels.
unsafe fn set_capture_size(w: i32, h: i32) {
    let Some(u) = ui() else { return };
    let s = |v| scale(v, u.dpi);
    SetWindowPos(
        u.hwnd,
        ptr::null_mut(),
        0,
        0,
        w + s(BORDER) * 2,
        h + s(TITLEBAR) + s(TOOLBAR),
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

/// The see-through capture area, in client coordinates.
unsafe fn hole_rect(u: &Ui) -> RECT {
    let mut rc: RECT = mem::zeroed();
    GetClientRect(u.hwnd, &mut rc);
    hole_for(u.dpi, rc.right, rc.bottom)
}

/// Hole for a window of the given size (client area == whole window).
fn hole_for(dpi: u32, w: i32, h: i32) -> RECT {
    let s = |v| scale(v, dpi);
    RECT {
        left: s(BORDER),
        top: s(TITLEBAR),
        right: (w - s(BORDER)).max(s(BORDER)),
        bottom: (h - s(TOOLBAR)).max(s(TITLEBAR)),
    }
}

unsafe fn capture_region(u: &Ui) -> Region {
    let r = hole_rect(u);
    let mut origin = POINT { x: r.left, y: r.top };
    ClientToScreen(u.hwnd, &mut origin);
    Region { x: origin.x, y: origin.y, w: r.right - r.left, h: r.bottom - r.top }
}

unsafe fn layout() {
    let Some(u) = ui() else { return };
    let mut rc: RECT = mem::zeroed();
    GetClientRect(u.hwnd, &mut rc);
    let s = |v| scale(v, u.dpi);
    let pad = s(6);
    let h = s(TOOLBAR) - pad * 2;
    let y = rc.bottom - s(TOOLBAR) + pad;
    let mut x = pad;
    let mut dwp = BeginDeferWindowPos(7);
    let mut place = |c: HWND, x: i32, y: i32, w: i32, h: i32| {
        dwp = DeferWindowPos(dwp, c, ptr::null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    };
    for (c, w, gap) in [
        (u.rec, s(84), pad * 2),
        (u.lbl_fps, s(28), pad),
        (u.fps, s(40), pad * 3),
        (u.w, s(52), 0),
        (u.lbl_x, s(18), 0),
        (u.h, s(52), 0),
    ] {
        place(c, x, y, w, h);
        x += w + gap;
    }
    let cb = s(TITLEBAR) - s(4) * 2;
    place(u.close, rc.right - s(4) - cb, s(4), cb, cb);
    EndDeferWindowPos(dwp);

    // Keep the size fields in sync, unless the user is typing in them.
    let r = capture_region(&u);
    let focus = GetFocus();
    if focus != u.w {
        set_text(u.w, &r.w.to_string());
    }
    if focus != u.h {
        set_text(u.h, &r.h.to_string());
    }
    // Paint now rather than when the queue is idle: during a live resize the
    // compositor would otherwise show the frame unpainted (see-through).
    RedrawWindow(u.hwnd, ptr::null(), ptr::null_mut(), RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN);
}


unsafe fn paint(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let mut ps: PAINTSTRUCT = mem::zeroed();
    let screen_dc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    // Draw off-screen and blit once: the compositor can grab a frame mid-paint,
    // and a frame-colored fill caught before the hole is keyed out shows up as
    // gray bands in the hole during resize.
    let hdc = CreateCompatibleDC(screen_dc);
    let bmp = CreateCompatibleBitmap(screen_dc, rc.right, rc.bottom);
    let old_bmp = SelectObject(hdc, bmp);
    FillRect(hdc, &rc, GetSysColorBrush(COLOR_BTNFACE));

    let recording = STATE.with(|s| matches!(*s.borrow(), State::Recording { .. }));
    let bar = RECT { bottom: scale(TITLEBAR, u.dpi), ..rc };
    if recording {
        let red = CreateSolidBrush(0x003030D0);
        FillRect(hdc, &bar, red);
        DeleteObject(red);
    }

    let hole = hole_rect(&u);
    let key = CreateSolidBrush(KEY);
    FillRect(hdc, &hole, key);
    DeleteObject(key);
    let outline = RECT { left: hole.left - 1, top: hole.top - 1, right: hole.right + 1, bottom: hole.bottom + 1 };
    FrameRect(hdc, &outline, GetSysColorBrush(COLOR_BTNSHADOW));

    let mut title = [0u16; 128];
    let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
    let mut text_rc = RECT { left: scale(10, u.dpi), right: rc.right - scale(TITLEBAR + 8, u.dpi), ..bar };
    let old = SelectObject(hdc, u.font);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, if recording { 0x00FFFFFF } else { GetSysColor(COLOR_BTNTEXT) });
    DrawTextW(hdc, title.as_ptr(), n, &mut text_rc, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    SelectObject(hdc, old);

    BitBlt(screen_dc, 0, 0, rc.right, rc.bottom, hdc, 0, 0, SRCCOPY);
    SelectObject(hdc, old_bmp);
    DeleteObject(bmp);
    DeleteDC(hdc);
    EndPaint(hwnd, &ps);
}

/// Set the window (taskbar) title and repaint our drawn title bar.
unsafe fn set_title(u: &Ui, title: &str) {
    set_text(u.hwnd, title);
    let mut rc: RECT = mem::zeroed();
    GetClientRect(u.hwnd, &mut rc);
    rc.bottom = scale(TITLEBAR, u.dpi);
    InvalidateRect(u.hwnd, &rc, 0);
}

/// Our own frame: drag by the title bar, resize from the edges.
unsafe fn hit_test(u: &Ui, lp: LPARAM) -> LRESULT {
    let mut pt = POINT { x: (lp & 0xFFFF) as i16 as i32, y: ((lp >> 16) & 0xFFFF) as i16 as i32 };
    ScreenToClient(u.hwnd, &mut pt);
    let mut rc: RECT = mem::zeroed();
    GetClientRect(u.hwnd, &mut rc);
    let s = |v| scale(v, u.dpi);
    let (left, right) = (pt.x < s(BORDER), pt.x >= rc.right - s(BORDER));
    let (top, bottom) = (pt.y < s(4), pt.y >= rc.bottom - s(4));
    let (near_left, near_right) = (pt.x < s(GRIP), pt.x >= rc.right - s(GRIP));
    let (near_top, near_bottom) = (pt.y < s(GRIP), pt.y >= rc.bottom - s(GRIP));
    let code = match () {
        _ if (top && near_left) || (left && near_top) => HTTOPLEFT,
        _ if (top && near_right) || (right && near_top) => HTTOPRIGHT,
        _ if (bottom && near_left) || (left && near_bottom) => HTBOTTOMLEFT,
        _ if (bottom && near_right) || (right && near_bottom) => HTBOTTOMRIGHT,
        _ if top => HTTOP,
        _ if bottom => HTBOTTOM,
        _ if left => HTLEFT,
        _ if right => HTRIGHT,
        _ if pt.y < s(TITLEBAR) => HTCAPTION,
        _ => HTCLIENT,
    };
    code as LRESULT
}

unsafe fn toggle() {
    let recording = STATE.with(|s| matches!(*s.borrow(), State::Recording { .. }));
    if recording {
        stop();
    } else if !busy() {
        start();
    }
}

unsafe fn start() {
    let Some(u) = ui() else { return };
    let fps = get_num(u.fps).unwrap_or(DEFAULT_FPS as i32).clamp(1, 60) as u32;
    set_text(u.fps, &fps.to_string());
    let region = capture_region(&u);
    if region.w < 1 || region.h < 1 {
        return;
    }

    let dir = std::env::temp_dir().join("rcap");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let path = dir.join(format!("rcap-{stamp}.gif"));

    // Bound queued raw frames to ~512 MB in case encoding falls behind.
    let frame_bytes = (region.w * region.h * 4) as usize;
    let (tx, rx) = mpsc::sync_channel(((512 << 20) / frame_bytes.max(1)).clamp(4, 300));
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicUsize::new(0));

    let hwnd = u.hwnd as isize;
    thread::spawn(move || {
        let res: EncodeResult =
            encode::run(rx, region.w as u16, region.h as u16, &path).map(|size| (path, size));
        PostMessageW(hwnd as HWND, WM_DONE, 0, Box::into_raw(Box::new(res)) as LPARAM);
    });
    {
        let (stop, frames) = (stop.clone(), frames.clone());
        thread::spawn(move || capture::run(region, fps, stop, frames, tx));
    }

    STATE.with(|s| *s.borrow_mut() = State::Recording { stop, frames, start: Instant::now() });
    set_text(u.rec, "Stop");
    for c in [u.fps, u.w, u.h] {
        EnableWindow(c, 0);
    }
    SetTimer(u.hwnd, TIMER, 100, None);
    update_title();
}

unsafe fn stop() {
    let Some(u) = ui() else { return };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let State::Recording { stop, .. } = &*s {
            stop.store(true, Ordering::Relaxed);
        }
        *s = State::Encoding;
    });
    set_text(u.rec, "Encoding…");
    EnableWindow(u.rec, 0);
    update_title();
}

unsafe fn finish(res: EncodeResult) {
    let Some(u) = ui() else { return };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let State::Recording { stop, .. } = &*s {
            stop.store(true, Ordering::Relaxed);
        }
        *s = State::Idle;
    });
    KillTimer(u.hwnd, TIMER);
    set_text(u.rec, "Record");
    for c in [u.rec, u.fps, u.w, u.h] {
        EnableWindow(c, 1);
    }
    let title = match res {
        Ok((path, size)) => {
            let mb = size as f64 / (1024.0 * 1024.0);
            if clipboard::copy_file(u.hwnd, &path) {
                format!("rcap — copied GIF to clipboard ({mb:.1} MB)")
            } else {
                format!("rcap — clipboard busy, saved {}", path.display())
            }
        }
        Err(e) => format!("rcap — error: {e}"),
    };
    set_title(&u, &title);
}

unsafe fn update_title() {
    let Some(u) = ui() else { return };
    let title = STATE.with(|s| match &*s.borrow() {
        State::Recording { frames, start, .. } => {
            let t = start.elapsed().as_secs_f64();
            format!(
                "rcap ● REC {}:{:04.1} · {} frames",
                (t / 60.0) as u32,
                t % 60.0,
                frames.load(Ordering::Relaxed)
            )
        }
        State::Encoding => "rcap — encoding…".into(),
        State::Idle => "rcap".into(),
    });
    set_title(&u, &title);
}

fn loword(v: usize) -> u32 {
    (v & 0xFFFF) as u32
}

fn hiword(v: usize) -> u32 {
    ((v >> 16) & 0xFFFF) as u32
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        // No system non-client area: the client covers the whole window.
        WM_NCCALCSIZE if wp != 0 => 0,
        WM_NCHITTEST => match ui() {
            Some(u) => hit_test(&u, lp),
            None => DefWindowProcW(hwnd, msg, wp, lp),
        },
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_SIZE => {
            layout();
            0
        }
        WM_GETMINMAXINFO => {
            if let Some(u) = ui() {
                let mmi = &mut *(lp as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT { x: scale(360, u.dpi), y: scale(120, u.dpi) };
            }
            0
        }
        WM_WINDOWPOSCHANGING => {
            // The capture region is fixed while recording/encoding.
            if busy() {
                (*(lp as *mut WINDOWPOS)).flags |= SWP_NOMOVE | SWP_NOSIZE;
            }
            0
        }
        WM_COMMAND => {
            let (id, code) = (loword(wp), hiword(wp));
            match id {
                ID_REC if code == BN_CLICKED => toggle(),
                ID_W | ID_H if code == EN_KILLFOCUS && !busy() => {
                    if let (Some(u), Some(r)) = (ui(), ui().map(|u| capture_region(&u))) {
                        let w = get_num(u.w).unwrap_or(r.w).clamp(1, 16000);
                        let h = get_num(u.h).unwrap_or(r.h).clamp(1, 16000);
                        set_capture_size(w, h);
                        layout();
                    }
                }
                ID_CLOSE => {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                ID_OK => {
                    SetFocus(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_TIMER => {
            update_title();
            0
        }
        WM_DONE => {
            let res = Box::from_raw(lp as *mut EncodeResult);
            finish(*res);
            0
        }
        WM_DPICHANGED => {
            if let Some(mut u) = ui() {
                DeleteObject(u.font);
                u.dpi = hiword(wp);
                u.font = make_font(u.dpi);
                UI.with(|c| c.set(Some(u)));
                apply_font(&u);
                let r = &*(lp as *const RECT);
                SetWindowPos(
                    hwnd,
                    ptr::null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
