#![windows_subsystem = "windows"]

mod capture;
mod clipboard;
mod encode;
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

// Frame metrics at 96 DPI. We draw our own frame: color-key hit-testing is
// unreliable with the system frame, and a window region (what LICEcap uses)
// makes the window flicker transparent during live resize. DWM still adds the
// Windows 11 rounded corners, shadow and border.
const TITLEBAR: i32 = 32;
const BORDER: i32 = 6;
const TOOLBAR: i32 = 44;
const GRIP: i32 = 12;
const CLOSE_W: i32 = 46;
const CONTROL_H: i32 = 28;
const RADIUS: i32 = 4;
const DEFAULT_FPS: u32 = 30;
const MIN_FPS: i32 = 10;
const MAX_FPS: i32 = 60;
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

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

// Windows 11 light palette.
const BG: COLORREF = rgb(243, 243, 243);
const TEXT: COLORREF = rgb(26, 26, 26);
const SUBTLE: COLORREF = rgb(96, 96, 96);
const STROKE: COLORREF = rgb(229, 229, 229);
const HOLE_STROKE: COLORREF = rgb(200, 200, 200);
const FIELD: COLORREF = rgb(255, 255, 255);
const FIELD_OFF: COLORREF = rgb(249, 249, 249);
const FIELD_LINE: COLORREF = rgb(134, 134, 134);
const ACCENT: COLORREF = rgb(0, 95, 184);
const RED: COLORREF = rgb(196, 43, 28);
const RED_HOT: COLORREF = rgb(212, 62, 48);
const RED_DOWN: COLORREF = rgb(168, 36, 24);
const BTN: COLORREF = rgb(251, 251, 251);
const BTN_HOT: COLORREF = rgb(246, 246, 246);
const BTN_DOWN: COLORREF = rgb(240, 240, 240);
const OFF: COLORREF = rgb(229, 229, 229);
const OFF_TEXT: COLORREF = rgb(140, 140, 140);

type EncodeResult = Result<(PathBuf, u64), String>;

#[derive(Clone, Copy)]
struct Ui {
    hwnd: HWND,
    rec: HWND,
    fps: HWND,
    w: HWND,
    h: HWND,
    recent: HWND,
    font: HFONT,
    icon_font: HFONT,
    text_h: i32,
    field_off: HBRUSH,
    dpi: u32,
}

/// Rects of the toolbar items, in client coordinates.
struct Toolbar {
    rec: RECT,
    fps_label: RECT,
    fps: RECT,
    w: RECT,
    x_label: RECT,
    h: RECT,
    recent: RECT,
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

        let dpi = GetDpiForWindow(hwnd);
        let edit = WS_TABSTOP | ES_NUMBER as u32 | ES_CENTER as u32;
        let font = make_font(dpi);
        let u = Ui {
            hwnd,
            rec: child(
                hwnd,
                "BUTTON",
                "●  Record",
                WS_TABSTOP | BS_PUSHBUTTON as u32,
                ID_REC,
            ),
            fps: child(hwnd, "EDIT", &DEFAULT_FPS.to_string(), edit, ID_FPS),
            w: child(hwnd, "EDIT", "", edit, ID_W),
            h: child(hwnd, "EDIT", "", edit, ID_H),
            recent: child(
                hwnd,
                "BUTTON",
                "Recent",
                WS_TABSTOP | BS_PUSHBUTTON as u32,
                ID_RECENT,
            ),
            font,
            icon_font: make_icon_font(dpi),
            text_h: text_height(font),
            field_off: CreateSolidBrush(FIELD_OFF),
            dpi,
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

unsafe fn make_font(dpi: u32) -> HFONT {
    let mut ncm: NONCLIENTMETRICSW = mem::zeroed();
    ncm.cbSize = mem::size_of::<NONCLIENTMETRICSW>() as u32;
    SystemParametersInfoForDpi(
        SPI_GETNONCLIENTMETRICS,
        ncm.cbSize,
        &mut ncm as *mut _ as *mut _,
        0,
        dpi,
    );
    CreateFontIndirectW(&ncm.lfMessageFont)
}

/// Icon font for the close glyph (present on Windows 10 and 11).
unsafe fn make_icon_font(dpi: u32) -> HFONT {
    CreateFontW(
        -scale(10, dpi),
        0,
        0,
        0,
        FW_NORMAL as _,
        0,
        0,
        0,
        DEFAULT_CHARSET as _,
        OUT_DEFAULT_PRECIS as _,
        CLIP_DEFAULT_PRECIS as _,
        CLEARTYPE_QUALITY as _,
        0,
        wide("Segoe MDL2 Assets").as_ptr(),
    )
}

unsafe fn text_height(font: HFONT) -> i32 {
    let dc = GetDC(ptr::null_mut());
    let old = SelectObject(dc, font);
    let mut tm: TEXTMETRICW = mem::zeroed();
    GetTextMetricsW(dc, &mut tm);
    SelectObject(dc, old);
    ReleaseDC(ptr::null_mut(), dc);
    tm.tmHeight
}

unsafe fn apply_font(u: &Ui) {
    for c in [u.rec, u.fps, u.w, u.h, u.recent] {
        SendMessageW(c, WM_SETFONT, u.font as WPARAM, 1);
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

unsafe fn get_num(h: HWND) -> Option<i32> {
    get_text(h).trim().parse().ok()
}

/// FPS field value clamped to MIN_FPS..=MAX_FPS; rewrites the field if it was out of range.
unsafe fn read_fps(u: &Ui) -> u32 {
    let fps = get_num(u.fps)
        .unwrap_or(DEFAULT_FPS as i32)
        .clamp(MIN_FPS, MAX_FPS);
    set_text(u.fps, &fps.to_string());
    fps as u32
}

/// Largest window that fits the (multi-monitor) virtual screen.
unsafe fn max_window() -> POINT {
    POINT {
        x: GetSystemMetrics(SM_CXVIRTUALSCREEN),
        y: GetSystemMetrics(SM_CYVIRTUALSCREEN),
    }
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

unsafe fn client_rect(hwnd: HWND) -> RECT {
    let mut rc: RECT = mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    rc
}

/// The see-through capture area, in client coordinates.
unsafe fn hole_rect(u: &Ui) -> RECT {
    let rc = client_rect(u.hwnd);
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

fn close_rect(u: &Ui, rc: &RECT) -> RECT {
    RECT {
        left: rc.right - scale(CLOSE_W, u.dpi),
        top: 0,
        right: rc.right,
        bottom: scale(TITLEBAR, u.dpi),
    }
}

fn toolbar(u: &Ui, rc: &RECT) -> Toolbar {
    let s = |v| scale(v, u.dpi);
    let h = s(CONTROL_H);
    let top = rc.bottom - s(TOOLBAR) + (s(TOOLBAR) - h) / 2;
    let mut x = s(BORDER);
    let mut next = |w: i32, gap: i32| {
        let r = RECT {
            left: x,
            top,
            right: x + s(w),
            bottom: top + h,
        };
        x += s(w) + s(gap);
        r
    };
    Toolbar {
        rec: next(96, 16),
        fps_label: next(24, 6),
        fps: next(40, 16),
        w: next(56, 0),
        x_label: next(18, 0),
        h: next(56, 0),
        recent: RECT {
            left: rc.right - s(BORDER) - s(92),
            top,
            right: rc.right - s(BORDER),
            bottom: top + h,
        },
    }
}

fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

unsafe fn capture_region(u: &Ui) -> Region {
    let r = hole_rect(u);
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
    let rc = client_rect(u.hwnd);
    let tb = toolbar(&u, &rc);
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
    // Borderless edits sit inside field boxes painted by us, vertically centered.
    let pad = scale(4, u.dpi);
    for (c, r) in [(u.fps, &tb.fps), (u.w, &tb.w), (u.h, &tb.h)] {
        let top = r.top + (r.bottom - r.top - u.text_h) / 2;
        place(
            c,
            &RECT {
                left: r.left + pad,
                top,
                right: r.right - pad,
                bottom: top + u.text_h,
            },
        );
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

unsafe fn fill(hdc: HDC, r: &RECT, color: COLORREF) {
    let b = CreateSolidBrush(color);
    FillRect(hdc, r, b);
    DeleteObject(b);
}

unsafe fn round_rect(hdc: HDC, r: &RECT, radius: i32, fill: COLORREF, stroke: COLORREF) {
    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PS_SOLID, 1, stroke);
    let (ob, op) = (SelectObject(hdc, brush), SelectObject(hdc, pen));
    RoundRect(
        hdc,
        r.left,
        r.top,
        r.right,
        r.bottom,
        radius * 2,
        radius * 2,
    );
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    DeleteObject(brush);
    DeleteObject(pen);
}

unsafe fn text(hdc: HDC, s: &str, r: &RECT, color: COLORREF, flags: DRAW_TEXT_FORMAT) {
    let w: Vec<u16> = s.encode_utf16().collect();
    let mut r = *r;
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, color);
    DrawTextW(
        hdc,
        w.as_ptr(),
        w.len() as i32,
        &mut r,
        flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
    );
}

unsafe fn paint(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let mut ps: PAINTSTRUCT = mem::zeroed();
    let screen_dc = BeginPaint(hwnd, &mut ps);
    let rc = client_rect(hwnd);
    let s = |v| scale(v, u.dpi);
    // Draw off-screen and blit once: the compositor can grab a frame mid-paint,
    // and a frame-colored fill caught before the hole is keyed out shows up as
    // gray bands in the hole during resize.
    let hdc = CreateCompatibleDC(screen_dc);
    let bmp = CreateCompatibleBitmap(screen_dc, rc.right, rc.bottom);
    let old_bmp = SelectObject(hdc, bmp);
    let old_font = SelectObject(hdc, u.font);
    fill(hdc, &rc, BG);

    let hole = hole_rect(&u);
    fill(hdc, &hole, KEY);
    let outline = RECT {
        left: hole.left - 1,
        top: hole.top - 1,
        right: hole.right + 1,
        bottom: hole.bottom + 1,
    };
    let stroke = CreateSolidBrush(HOLE_STROKE);
    FrameRect(hdc, &outline, stroke);
    DeleteObject(stroke);

    // Title bar.
    let close = close_rect(&u, &rc);
    let title = get_text(hwnd);
    let title_rc = RECT {
        left: s(12),
        top: 0,
        right: close.left - s(8),
        bottom: s(TITLEBAR),
    };
    text(
        hdc,
        &title,
        &title_rc,
        if recording() { RED } else { TEXT },
        DT_END_ELLIPSIS,
    );
    let (hot, down) = CLOSE.with(|c| c.get());
    if hot {
        fill(hdc, &close, if down { RED_DOWN } else { RED });
    }
    SelectObject(hdc, u.icon_font);
    text(
        hdc,
        "\u{E8BB}",
        &close,
        if hot { FIELD } else { TEXT },
        DT_CENTER,
    );
    SelectObject(hdc, u.font);

    // Toolbar: labels and the boxes the borderless edits sit in.
    let tb = toolbar(&u, &rc);
    text(hdc, "FPS", &tb.fps_label, SUBTLE, DT_RIGHT);
    text(hdc, "×", &tb.x_label, SUBTLE, DT_CENTER);
    let focus = GetFocus();
    for (c, r) in [(u.fps, &tb.fps), (u.w, &tb.w), (u.h, &tb.h)] {
        let enabled = IsWindowEnabled(c) != 0;
        round_rect(
            hdc,
            r,
            s(RADIUS),
            if enabled { FIELD } else { FIELD_OFF },
            STROKE,
        );
        let (line, thick) = if focus == c {
            (ACCENT, s(2))
        } else {
            (FIELD_LINE, 1)
        };
        if enabled {
            let bottom = RECT {
                left: r.left + s(RADIUS) / 2,
                top: r.bottom - thick,
                right: r.right - s(RADIUS) / 2,
                bottom: r.bottom,
            };
            fill(hdc, &bottom, line);
        }
    }

    BitBlt(screen_dc, 0, 0, rc.right, rc.bottom, hdc, 0, 0, SRCCOPY);
    SelectObject(hdc, old_font);
    SelectObject(hdc, old_bmp);
    DeleteObject(bmp);
    DeleteDC(hdc);
    EndPaint(hwnd, &ps);
}

/// Draws the Record and Recent buttons ourselves (via comctl6 custom draw),
/// keeping their keyboard, focus and hover behavior.
unsafe fn draw_button(u: &Ui, cd: &NMCUSTOMDRAW) -> LRESULT {
    if cd.dwDrawStage != CDDS_PREERASE {
        return CDRF_DODEFAULT as LRESULT;
    }
    let (hdc, r, st) = (cd.hdc, cd.rc, cd.uItemState);
    let button = cd.hdr.hwndFrom;
    fill(hdc, &r, BG);
    // Neutral, except Record turns solid red as the Stop button.
    let stop = button == u.rec && recording();
    let (bg, border, fg) = match () {
        _ if st & CDIS_DISABLED != 0 => (OFF, OFF, OFF_TEXT),
        _ if stop && st & CDIS_SELECTED != 0 => (RED_DOWN, RED_DOWN, FIELD),
        _ if stop && st & CDIS_HOT != 0 => (RED_HOT, RED_HOT, FIELD),
        _ if stop => (RED, RED, FIELD),
        _ if st & CDIS_SELECTED != 0 => (BTN_DOWN, STROKE, SUBTLE),
        _ if st & CDIS_HOT != 0 => (BTN_HOT, STROKE, TEXT),
        _ => (BTN, STROKE, TEXT),
    };
    round_rect(hdc, &r, scale(RADIUS, u.dpi), bg, border);
    if st & CDIS_FOCUS != 0 && st & CDIS_SHOWKEYBOARDCUES != 0 {
        let pen = CreatePen(PS_SOLID, scale(2, u.dpi), TEXT);
        let (op, ob) = (
            SelectObject(hdc, pen),
            SelectObject(hdc, GetStockObject(NULL_BRUSH)),
        );
        let d = scale(RADIUS, u.dpi) * 2;
        RoundRect(hdc, r.left + 1, r.top + 1, r.right, r.bottom, d, d);
        SelectObject(hdc, op);
        SelectObject(hdc, ob);
        DeleteObject(pen);
    }
    let old = SelectObject(hdc, u.font);
    let label = get_text(button);
    // An icon in front of the label: Record's red dot (idle), Recent's clock.
    let icon = match label.strip_prefix('●') {
        Some(rest) if st & CDIS_DISABLED == 0 => Some(("●", u.font, RED, rest.to_string())),
        _ if button == u.recent => Some(("\u{E81C}", u.icon_font, fg, format!("  {label}"))),
        _ => None,
    };
    match icon {
        Some((glyph, font, color, rest)) => {
            let width = |s: &str, f: HFONT| {
                let w: Vec<u16> = s.encode_utf16().collect();
                let mut m: RECT = mem::zeroed();
                SelectObject(hdc, f);
                DrawTextW(
                    hdc,
                    w.as_ptr(),
                    w.len() as i32,
                    &mut m,
                    DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
                );
                m.right
            };
            let (icon_w, rest_w) = (width(glyph, font), width(&rest, u.font));
            let left = r.left + (r.right - r.left - icon_w - rest_w) / 2;
            SelectObject(hdc, font);
            text(
                hdc,
                glyph,
                &RECT {
                    left,
                    right: left + icon_w,
                    ..r
                },
                color,
                DT_LEFT,
            );
            SelectObject(hdc, u.font);
            text(
                hdc,
                &rest,
                &RECT {
                    left: left + icon_w,
                    right: r.right,
                    ..r
                },
                fg,
                DT_LEFT,
            );
        }
        None => text(hdc, &label, &r, fg, DT_CENTER),
    }
    SelectObject(hdc, old);
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
            recent::ago(g.stamp),
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
        r.top - scale(4, u.dpi),
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
    let mut rc = client_rect(u.hwnd);
    rc.bottom = scale(TITLEBAR, u.dpi);
    InvalidateRect(u.hwnd, &rc, 0);
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

/// Our own frame: drag by the title bar, resize from the edges.
unsafe fn hit_test(u: &Ui, lp: LPARAM) -> LRESULT {
    let mut pt = POINT {
        x: (lp & 0xFFFF) as i16 as i32,
        y: ((lp >> 16) & 0xFFFF) as i16 as i32,
    };
    ScreenToClient(u.hwnd, &mut pt);
    let rc = client_rect(u.hwnd);
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
        _ if contains(&close_rect(u, &rc), pt.x, pt.y) => HTCLIENT,
        _ if pt.y < s(TITLEBAR) => HTCAPTION,
        _ => HTCLIENT,
    };
    code as LRESULT
}

/// Update the close button's hover/pressed state, repainting it on change.
unsafe fn set_close_state(u: &Ui, hot: bool, down: bool) {
    if CLOSE.with(|c| c.replace((hot, down))) != (hot, down) {
        let rc = client_rect(u.hwnd);
        InvalidateRect(u.hwnd, &close_rect(u, &rc), 0);
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
    for c in [u.fps, u.w, u.h] {
        EnableWindow(c, 0);
    }
    set_border(&u, Some(RED));
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
    for c in [u.rec, u.fps, u.w, u.h] {
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
                mmi.ptMinTrackSize = POINT {
                    x: scale(450, u.dpi),
                    y: scale(120, u.dpi),
                };
                mmi.ptMaxTrackSize = max_window();
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
                let hot = contains(&close_rect(&u, &client_rect(hwnd)), x, y);
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
                if contains(&close_rect(&u, &client_rect(hwnd)), x, y) {
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
                    let inside = contains(&close_rect(&u, &client_rect(hwnd)), x, y);
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
                    draw_button(&u, &*(lp as *const NMCUSTOMDRAW))
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        WM_CTLCOLOREDIT => {
            SetBkColor(wp as HDC, FIELD);
            SetTextColor(wp as HDC, TEXT);
            GetStockObject(WHITE_BRUSH) as LRESULT
        }
        // Disabled edits ask for static colors.
        WM_CTLCOLORSTATIC => match ui() {
            Some(u) => {
                SetBkColor(wp as HDC, FIELD_OFF);
                SetTextColor(wp as HDC, OFF_TEXT);
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
                        // Cap the hole so the whole window fits on screen; the
                        // minimum is enforced by WM_GETMINMAXINFO.
                        let (s, max) = (|v| scale(v, u.dpi), max_window());
                        let w = get_num(u.w).unwrap_or(r.w).clamp(1, max.x - s(BORDER) * 2);
                        let h = get_num(u.h)
                            .unwrap_or(r.h)
                            .clamp(1, max.y - s(TITLEBAR) - s(TOOLBAR));
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
                DeleteObject(u.font);
                DeleteObject(u.icon_font);
                u.dpi = hiword(wp);
                u.font = make_font(u.dpi);
                u.icon_font = make_icon_font(u.dpi);
                u.text_h = text_height(u.font);
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
