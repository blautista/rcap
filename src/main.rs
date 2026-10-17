#![windows_subsystem = "windows"]

mod capture;
mod clipboard;
mod draw;
mod encode;
mod layout;
mod recent;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;
use std::{mem, ptr, thread};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use capture::Region;
use draw::{Fonts, KEY};
use layout::{scale, DEFAULT_FPS};

const DEFAULT_SIZE: (i32, i32) = (640, 360);

const ID_REC: u32 = 101;
const ID_FPS: u32 = 102;
const ID_W: u32 = 103;
const ID_H: u32 = 104;
const ID_RECENT: u32 = 105;
const ID_OK: u32 = 1; // IDOK, sent by IsDialogMessage on Enter
                      // Recent menu commands; recordings use MENU_GIF + index.
const MENU_GIF: usize = 1000;
const MENU_FOLDER: usize = 2000;
const MENU_CLEAR: usize = 2001;
const TIMER: usize = 1;
const WM_DONE: u32 = WM_APP + 1;

type EncodeResult = Result<(PathBuf, u64), String>;

#[derive(Clone, Copy)]
struct Ui {
    hwnd: HWND,
    rec: HWND,
    fps: HWND,
    w: HWND,
    h: HWND,
    recent: HWND,
    fonts: Fonts,
    text_h: i32,
    field_off: HBRUSH,
}

enum State {
    Idle,
    Recording {
        stop: Arc<AtomicBool>,
        frames: Arc<AtomicUsize>,
        start: Instant,
    },
    Encoding,
}

thread_local! {
    static UI: Cell<Option<Ui>> = const { Cell::new(None) };
    static STATE: RefCell<State> = const { RefCell::new(State::Idle) };
    /// Close button (hovered, pressed).
    static CLOSE: Cell<(bool, bool)> = const { Cell::new((false, false)) };
}

fn ui() -> Option<Ui> {
    UI.with(|u| u.get())
}

fn busy() -> bool {
    STATE.with(|s| !matches!(*s.borrow(), State::Idle))
}

fn recording() -> bool {
    STATE.with(|s| matches!(*s.borrow(), State::Recording { .. }))
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn style() -> u32 {
    // WS_THICKFRAME is required for edge resizing; WM_NCCALCSIZE hides its border.
    WS_POPUP | WS_THICKFRAME | WS_CLIPCHILDREN | WS_SYSMENU | WS_MINIMIZEBOX
}

const EX_STYLE: u32 = WS_EX_TOPMOST | WS_EX_APPWINDOW | WS_EX_LAYERED;

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
        wc.hIcon = LoadIconW(hinst, 1 as _); // assets/rcap.rc
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
        let round: i32 = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &round as *const _ as *const _,
            4,
        );
        // Borderless windows only get the DWM shadow with some frame extended into the client.
        let m = MARGINS {
            cxLeftWidth: 0,
            cxRightWidth: 0,
            cyTopHeight: 1,
            cyBottomHeight: 0,
        };
        DwmExtendFrameIntoClientArea(hwnd, &m);

        let edit = WS_TABSTOP | ES_NUMBER as u32 | ES_CENTER as u32;
        let button = WS_TABSTOP | BS_PUSHBUTTON as u32;
        let fonts = Fonts::new(GetDpiForWindow(hwnd));
        let u = Ui {
            hwnd,
            rec: child(hwnd, "BUTTON", "●  Record", button, ID_REC),
            fps: child(hwnd, "EDIT", &DEFAULT_FPS.to_string(), edit, ID_FPS),
            w: child(hwnd, "EDIT", "", edit, ID_W),
            h: child(hwnd, "EDIT", "", edit, ID_H),
            recent: child(hwnd, "BUTTON", "Recent", button, ID_RECENT),
            fonts,
            text_h: fonts.text_height(),
            field_off: CreateSolidBrush(draw::FIELD_OFF),
        };
        UI.with(|c| c.set(Some(u)));
        apply_font(&u);
        SendMessageW(u.fps, EM_LIMITTEXT, 2, 0);
        SendMessageW(u.w, EM_LIMITTEXT, 5, 0);
        SendMessageW(u.h, EM_LIMITTEXT, 5, 0);
        set_capture_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1);
        ShowWindow(hwnd, SW_SHOW);
        thread::spawn(|| recent::prune(recent::MAX));

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

unsafe fn child(parent: HWND, class: &str, text: &str, style: u32, id: u32) -> HWND {
    CreateWindowExW(
        0,
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

unsafe fn apply_font(u: &Ui) {
    for c in [u.rec, u.fps, u.w, u.h, u.recent] {
        SendMessageW(c, WM_SETFONT, u.fonts.text as WPARAM, 1);
    }
}

unsafe fn get_text(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn set_text(h: HWND, s: &str) {
    SetWindowTextW(h, wide(s).as_ptr());
}

/// The FPS field's value, clamped; rewrites the field if it was out of range.
unsafe fn read_fps(u: &Ui) -> u32 {
    let fps = layout::parse_fps(&get_text(u.fps));
    set_text(u.fps, &fps.to_string());
    fps
}

/// The (multi-monitor) virtual screen size.
unsafe fn screen_size() -> (i32, i32) {
    (
        GetSystemMetrics(SM_CXVIRTUALSCREEN),
        GetSystemMetrics(SM_CYVIRTUALSCREEN),
    )
}

/// Resize the window so the see-through hole is exactly w x h physical pixels.
unsafe fn set_capture_size(w: i32, h: i32) {
    let Some(u) = ui() else { return };
    let (ww, wh) = layout::window_for_hole(u.fonts.dpi, w, h);
    SetWindowPos(
        u.hwnd,
        ptr::null_mut(),
        0,
        0,
        ww,
        wh,
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

/// Client (== window) size.
unsafe fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut rc: RECT = mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    (rc.right, rc.bottom)
}

unsafe fn close_rect(u: &Ui) -> RECT {
    layout::close_button(u.fonts.dpi, client_size(u.hwnd).0)
}

unsafe fn capture_region(u: &Ui) -> Region {
    let (w, h) = client_size(u.hwnd);
    let r = layout::hole(u.fonts.dpi, w, h);
    let mut origin = POINT {
        x: r.left,
        y: r.top,
    };
    ClientToScreen(u.hwnd, &mut origin);
    Region {
        x: origin.x,
        y: origin.y,
        w: r.right - r.left,
        h: r.bottom - r.top,
    }
}

unsafe fn layout() {
    let Some(u) = ui() else { return };
    let (w, h) = client_size(u.hwnd);
    let dpi = u.fonts.dpi;
    let tb = layout::toolbar(dpi, w, h);
    let mut dwp = BeginDeferWindowPos(5);
    let mut place = |c: HWND, r: &RECT| {
        dwp = DeferWindowPos(
            dwp,
            c,
            ptr::null_mut(),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    };
    place(u.rec, &tb.rec);
    place(u.recent, &tb.recent);
    // Borderless edits sit inside field boxes painted by us.
    for (c, r) in [(u.fps, &tb.fps), (u.w, &tb.w), (u.h, &tb.h)] {
        place(c, &layout::edit_in_field(dpi, r, u.text_h));
    }
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
    RedrawWindow(
        u.hwnd,
        ptr::null(),
        ptr::null_mut(),
        RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
    );
}

unsafe fn paint(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let mut ps: PAINTSTRUCT = mem::zeroed();
    let screen_dc = BeginPaint(hwnd, &mut ps);
    let (w, h) = client_size(hwnd);
    // Draw off-screen and blit once: the compositor can grab a frame mid-paint,
    // and a frame-colored fill caught before the hole is keyed out shows up as
    // gray bands in the hole during resize.
    let hdc = CreateCompatibleDC(screen_dc);
    let bmp = CreateCompatibleBitmap(screen_dc, w, h);
    let old_bmp = SelectObject(hdc, bmp);

    let focus = GetFocus();
    let (close_hot, close_down) = CLOSE.with(|c| c.get());
    let title = get_text(hwnd);
    let state = draw::Frame {
        title: &title,
        recording: recording(),
        close_hot,
        close_down,
        fields_enabled: IsWindowEnabled(u.fps) != 0,
        focus: match focus {
            f if f == u.fps => Some(draw::Field::Fps),
            f if f == u.w => Some(draw::Field::W),
            f if f == u.h => Some(draw::Field::H),
            _ => None,
        },
    };
    draw::frame(hdc, &u.fonts, w, h, &state);

    BitBlt(screen_dc, 0, 0, w, h, hdc, 0, 0, SRCCOPY);
    SelectObject(hdc, old_bmp);
    DeleteObject(bmp);
    DeleteDC(hdc);
    EndPaint(hwnd, &ps);
}

/// Draws the Record and Recent buttons ourselves (via comctl6 custom draw),
/// keeping their keyboard, focus and hover behavior.
unsafe fn custom_draw_button(u: &Ui, cd: &NMCUSTOMDRAW) -> LRESULT {
    if cd.dwDrawStage != CDDS_PREERASE {
        return CDRF_DODEFAULT as LRESULT;
    }
    let st = cd.uItemState;
    let kind = if cd.hdr.hwndFrom == u.rec {
        draw::Button::Record
    } else {
        draw::Button::Recent
    };
    let state = draw::ButtonState {
        hot: st & CDIS_HOT != 0,
        pressed: st & CDIS_SELECTED != 0,
        disabled: st & CDIS_DISABLED != 0,
        keyboard_focus: st & CDIS_FOCUS != 0 && st & CDIS_SHOWKEYBOARDCUES != 0,
    };
    let label = get_text(cd.hdr.hwndFrom);
    draw::button(cd.hdc, &u.fonts, cd.rc, kind, &label, recording(), state);
    CDRF_SKIPDEFAULT as LRESULT
}

/// Copy a GIF to the clipboard and say so in the title bar.
unsafe fn copy_gif(u: &Ui, path: &Path, size: u64) {
    let title = if clipboard::copy_file(u.hwnd, path) {
        format!("Copied GIF to clipboard ({})", recent::size_label(size))
    } else {
        format!("Clipboard busy, saved {}", path.display())
    };
    set_title(u, &title);
}

/// Menu of recent recordings above the Recent button; picking one copies it.
unsafe fn show_recent(u: &Ui) {
    let gifs = recent::list();
    let now = recent::now_ms();
    let menu = CreatePopupMenu();
    if gifs.is_empty() {
        AppendMenuW(
            menu,
            MF_STRING | MF_GRAYED,
            0,
            wide("No recordings yet").as_ptr(),
        );
    }
    for (i, g) in gifs.iter().enumerate() {
        // Text after a tab is right-aligned in its own column.
        let label = format!(
            "{}\t{}×{}  ·  {}",
            recent::ago(g.stamp, now),
            g.w,
            g.h,
            recent::size_label(g.size)
        );
        AppendMenuW(menu, MF_STRING, MENU_GIF + i, wide(&label).as_ptr());
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, MENU_FOLDER, wide("Open folder").as_ptr());
    // Not while a recording is being written to the same folder.
    let clear = if gifs.is_empty() || busy() {
        MF_STRING | MF_GRAYED
    } else {
        MF_STRING
    };
    AppendMenuW(menu, clear, MENU_CLEAR, wide("Clear all").as_ptr());

    let mut r: RECT = mem::zeroed();
    GetWindowRect(u.recent, &mut r);
    let flags = TPM_RETURNCMD | TPM_RIGHTALIGN | TPM_BOTTOMALIGN | TPM_NONOTIFY;
    let cmd = TrackPopupMenu(
        menu,
        flags,
        r.right,
        r.top - scale(4, u.fonts.dpi),
        0,
        u.hwnd,
        ptr::null(),
    ) as usize;
    DestroyMenu(menu);

    match cmd {
        MENU_FOLDER => {
            let dir = recent::dir();
            let _ = std::fs::create_dir_all(&dir);
            let dir = wide(&dir.to_string_lossy());
            ShellExecuteW(
                u.hwnd,
                wide("open").as_ptr(),
                dir.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            );
        }
        MENU_CLEAR => {
            recent::prune(0);
            set_title(u, "Cleared recent GIFs");
        }
        c if c >= MENU_GIF => {
            if let Some(g) = gifs.get(c - MENU_GIF) {
                copy_gif(u, &g.path, g.size);
            }
        }
        _ => {}
    }
}

/// Set the window (taskbar) title and repaint our drawn title bar.
unsafe fn set_title(u: &Ui, title: &str) {
    set_text(u.hwnd, title);
    let r = layout::title(u.fonts.dpi, client_size(u.hwnd).0);
    InvalidateRect(u.hwnd, &r, 0);
}

/// Red window border while recording (Windows 11; ignored elsewhere).
unsafe fn set_border(u: &Ui, color: Option<COLORREF>) {
    let c = color.unwrap_or(DWMWA_COLOR_DEFAULT);
    DwmSetWindowAttribute(
        u.hwnd,
        DWMWA_BORDER_COLOR as u32,
        &c as *const _ as *const _,
        4,
    );
}

/// Update the close button's hover/pressed state, repainting it on change.
unsafe fn set_close_state(u: &Ui, hot: bool, down: bool) {
    if CLOSE.with(|c| c.replace((hot, down))) != (hot, down) {
        InvalidateRect(u.hwnd, &close_rect(u), 0);
    }
}

fn mouse_pos(lp: LPARAM) -> (i32, i32) {
    (
        (lp & 0xFFFF) as i16 as i32,
        ((lp >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe fn toggle() {
    if recording() {
        stop();
    } else if !busy() {
        start();
    }
}

unsafe fn start() {
    let Some(u) = ui() else { return };
    let fps = read_fps(&u);
    let region = capture_region(&u);
    if region.w < 1 || region.h < 1 {
        return;
    }

    let path = recent::new_path();

    // Bound queued raw frames to ~512 MB in case encoding falls behind.
    let frame_bytes = (region.w * region.h * 4) as usize;
    let (tx, rx) = mpsc::sync_channel(((512 << 20) / frame_bytes.max(1)).clamp(4, 300));
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicUsize::new(0));

    let hwnd = u.hwnd as isize;
    thread::spawn(move || {
        let res: EncodeResult =
            encode::run(rx, region.w as u16, region.h as u16, &path).map(|size| (path, size));
        PostMessageW(
            hwnd as HWND,
            WM_DONE,
            0,
            Box::into_raw(Box::new(res)) as LPARAM,
        );
    });
    {
        let (stop, frames) = (stop.clone(), frames.clone());
        thread::spawn(move || capture::run(region, fps, stop, frames, tx));
    }

    STATE.with(|s| {
        *s.borrow_mut() = State::Recording {
            stop,
            frames,
            start: Instant::now(),
        }
    });
    set_text(u.rec, "■  Stop");
    for c in [u.fps, u.w, u.h, u.recent] {
        EnableWindow(c, 0);
    }
    set_border(&u, Some(draw::RED));
    InvalidateRect(u.hwnd, ptr::null(), 0);
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
    set_border(&u, None);
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
    set_text(u.rec, "●  Record");
    for c in [u.rec, u.fps, u.w, u.h, u.recent] {
        EnableWindow(c, 1);
    }
    set_border(&u, None);
    InvalidateRect(u.hwnd, ptr::null(), 0);
    match res {
        Ok((path, size)) => copy_gif(&u, &path, size),
        Err(e) => set_title(&u, &format!("Error: {e}")),
    }
    recent::prune(recent::MAX);
}

unsafe fn update_title() {
    let Some(u) = ui() else { return };
    let title = STATE.with(|s| match &*s.borrow() {
        State::Recording { frames, start, .. } => {
            let t = start.elapsed().as_secs_f64();
            format!(
                "●  REC {}:{:04.1}  ·  {} frames",
                (t / 60.0) as u32,
                t % 60.0,
                frames.load(Ordering::Relaxed)
            )
        }
        State::Encoding => "Encoding…".into(),
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
        // DWM doesn't draw frames for layered windows, so the default handlers
        // paint the classic thick frame over our edges on (de)activation.
        WM_NCPAINT => 0,
        WM_NCACTIVATE => DefWindowProcW(hwnd, msg, wp, -1),
        WM_NCHITTEST => match ui() {
            Some(u) => {
                let (x, y) = mouse_pos(lp);
                let mut pt = POINT { x, y };
                ScreenToClient(hwnd, &mut pt);
                let (w, h) = client_size(hwnd);
                layout::hit_test(u.fonts.dpi, w, h, pt.x, pt.y) as LRESULT
            }
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
                let (min_w, min_h) = layout::min_window(u.fonts.dpi);
                let (max_w, max_h) = screen_size();
                mmi.ptMinTrackSize = POINT { x: min_w, y: min_h };
                mmi.ptMaxTrackSize = POINT { x: max_w, y: max_h };
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
        WM_MOUSEMOVE => {
            if let Some(u) = ui() {
                let (x, y) = mouse_pos(lp);
                let hot = layout::contains(&close_rect(&u), x, y);
                let (was_hot, down) = CLOSE.with(|c| c.get());
                if hot && !was_hot {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tme);
                }
                set_close_state(&u, hot, down);
            }
            0
        }
        WM_MOUSELEAVE => {
            if let Some(u) = ui() {
                let (_, down) = CLOSE.with(|c| c.get());
                set_close_state(&u, false, down);
            }
            0
        }
        WM_LBUTTONDOWN => {
            if let Some(u) = ui() {
                let (x, y) = mouse_pos(lp);
                if layout::contains(&close_rect(&u), x, y) {
                    SetCapture(hwnd);
                    set_close_state(&u, true, true);
                } else {
                    // Clicking empty space commits whatever is being typed.
                    SetFocus(hwnd);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            if let Some(u) = ui() {
                let (hot, down) = CLOSE.with(|c| c.get());
                if down {
                    ReleaseCapture();
                    let (x, y) = mouse_pos(lp);
                    let inside = layout::contains(&close_rect(&u), x, y);
                    set_close_state(&u, hot && inside, false);
                    if inside {
                        PostMessageW(hwnd, WM_CLOSE, 0, 0);
                    }
                }
            }
            0
        }
        WM_NOTIFY => {
            let hdr = &*(lp as *const NMHDR);
            match ui() {
                Some(u)
                    if (hdr.hwndFrom == u.rec || hdr.hwndFrom == u.recent)
                        && hdr.code == NM_CUSTOMDRAW =>
                {
                    custom_draw_button(&u, &*(lp as *const NMCUSTOMDRAW))
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        WM_CTLCOLOREDIT => {
            SetBkColor(wp as HDC, draw::FIELD);
            SetTextColor(wp as HDC, draw::TEXT);
            GetStockObject(WHITE_BRUSH) as LRESULT
        }
        // Disabled edits ask for static colors.
        WM_CTLCOLORSTATIC => match ui() {
            Some(u) => {
                SetBkColor(wp as HDC, draw::FIELD_OFF);
                SetTextColor(wp as HDC, draw::OFF_TEXT);
                u.field_off as LRESULT
            }
            None => DefWindowProcW(hwnd, msg, wp, lp),
        },
        WM_COMMAND => {
            let (id, code) = (loword(wp), hiword(wp));
            match id {
                ID_REC if code == BN_CLICKED => toggle(),
                ID_RECENT if code == BN_CLICKED => {
                    if let Some(u) = ui() {
                        show_recent(&u);
                    }
                }
                ID_FPS | ID_W | ID_H if code == EN_SETFOCUS => {
                    InvalidateRect(hwnd, ptr::null(), 0);
                }
                ID_FPS if code == EN_KILLFOCUS => {
                    InvalidateRect(hwnd, ptr::null(), 0);
                    if let (Some(u), false) = (ui(), busy()) {
                        read_fps(&u);
                    }
                }
                ID_W | ID_H if code == EN_KILLFOCUS => {
                    InvalidateRect(hwnd, ptr::null(), 0);
                    if let (Some(u), false) = (ui(), busy()) {
                        let r = capture_region(&u);
                        let num = |c, fallback| get_text(c).trim().parse().unwrap_or(fallback);
                        // The minimum is enforced by WM_GETMINMAXINFO.
                        let (w, h) = layout::clamp_hole(
                            u.fonts.dpi,
                            num(u.w, r.w),
                            num(u.h, r.h),
                            screen_size(),
                        );
                        set_capture_size(w, h);
                        layout();
                    }
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
                u.fonts.delete();
                u.fonts = Fonts::new(hiword(wp));
                u.text_h = u.fonts.text_height();
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
