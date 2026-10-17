//! Window geometry and input rules. Everything here is a pure function of the
//! DPI and window size, so it's unit tested without creating a window.
//!
//! We draw our own frame: color-key hit-testing is unreliable with the system
//! frame, and a window region (what LICEcap uses) makes the window flicker
//! transparent during live resize. DWM still adds the Windows 11 rounded
//! corners, shadow and border.

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// Metrics at 96 DPI.
pub const TITLEBAR: i32 = 32;
pub const BORDER: i32 = 6;
pub const TOOLBAR: i32 = 44;
const GRIP: i32 = 12;
const RESIZE_TOP: i32 = 4;
const CLOSE_W: i32 = 46;
const CONTROL_H: i32 = 28;
pub const RADIUS: i32 = 4;
const MIN_W: i32 = 450;
const MIN_H: i32 = 120;

pub const DEFAULT_FPS: u32 = 30;
pub const MIN_FPS: u32 = 10;
pub const MAX_FPS: u32 = 60;

pub fn scale(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

pub fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// The see-through capture area of a w x h window (client area == whole window).
pub fn hole(dpi: u32, w: i32, h: i32) -> RECT {
    let s = |v| scale(v, dpi);
    RECT {
        left: s(BORDER),
        top: s(TITLEBAR),
        right: (w - s(BORDER)).max(s(BORDER)),
        bottom: (h - s(TOOLBAR)).max(s(TITLEBAR)),
    }
}

/// Window size whose hole is exactly w x h.
pub fn window_for_hole(dpi: u32, w: i32, h: i32) -> (i32, i32) {
    let s = |v| scale(v, dpi);
    (w + s(BORDER) * 2, h + s(TITLEBAR) + s(TOOLBAR))
}

/// Largest hole whose window still fits a screen of the given size.
pub fn clamp_hole(dpi: u32, w: i32, h: i32, screen: (i32, i32)) -> (i32, i32) {
    let (chrome_w, chrome_h) = window_for_hole(dpi, 0, 0);
    (
        w.clamp(1, (screen.0 - chrome_w).max(1)),
        h.clamp(1, (screen.1 - chrome_h).max(1)),
    )
}

pub fn min_window(dpi: u32) -> (i32, i32) {
    (scale(MIN_W, dpi), scale(MIN_H, dpi))
}

pub fn close_button(dpi: u32, w: i32) -> RECT {
    RECT {
        left: w - scale(CLOSE_W, dpi),
        top: 0,
        right: w,
        bottom: scale(TITLEBAR, dpi),
    }
}

pub fn title(dpi: u32, w: i32) -> RECT {
    RECT {
        left: scale(12, dpi),
        top: 0,
        right: close_button(dpi, w).left - scale(8, dpi),
        bottom: scale(TITLEBAR, dpi),
    }
}

/// Rects of the toolbar items, in client coordinates.
pub struct Toolbar {
    pub rec: RECT,
    pub fps_label: RECT,
    pub fps: RECT,
    pub w: RECT,
    pub x_label: RECT,
    pub h: RECT,
    pub recent: RECT,
}

pub fn toolbar(dpi: u32, w: i32, h: i32) -> Toolbar {
    let s = |v| scale(v, dpi);
    let height = s(CONTROL_H);
    let top = h - s(TOOLBAR) + (s(TOOLBAR) - height) / 2;
    let mut x = s(BORDER);
    let mut next = |width: i32, gap: i32| {
        let r = RECT {
            left: x,
            top,
            right: x + s(width),
            bottom: top + height,
        };
        x += s(width) + s(gap);
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
            left: w - s(BORDER) - s(92),
            top,
            right: w - s(BORDER),
            bottom: top + height,
        },
    }
}

/// Where a borderless edit goes inside its painted field box: inset and
/// vertically centered for a font of the given line height.
pub fn edit_in_field(dpi: u32, field: &RECT, text_h: i32) -> RECT {
    let top = field.top + (field.bottom - field.top - text_h) / 2;
    RECT {
        left: field.left + scale(4, dpi),
        top,
        right: field.right - scale(4, dpi),
        bottom: top + text_h,
    }
}

/// WM_NCHITTEST result for client point (x, y): resize from the edges, drag
/// by the title bar, click everywhere else (including the close button).
pub fn hit_test(dpi: u32, w: i32, h: i32, x: i32, y: i32) -> u32 {
    let s = |v| scale(v, dpi);
    let (left, right) = (x < s(BORDER), x >= w - s(BORDER));
    let (top, bottom) = (y < s(RESIZE_TOP), y >= h - s(RESIZE_TOP));
    let (near_left, near_right) = (x < s(GRIP), x >= w - s(GRIP));
    let (near_top, near_bottom) = (y < s(GRIP), y >= h - s(GRIP));
    match () {
        _ if (top && near_left) || (left && near_top) => HTTOPLEFT,
        _ if (top && near_right) || (right && near_top) => HTTOPRIGHT,
        _ if (bottom && near_left) || (left && near_bottom) => HTBOTTOMLEFT,
        _ if (bottom && near_right) || (right && near_bottom) => HTBOTTOMRIGHT,
        _ if top => HTTOP,
        _ if bottom => HTBOTTOM,
        _ if left => HTLEFT,
        _ if right => HTRIGHT,
        _ if contains(&close_button(dpi, w), x, y) => HTCLIENT,
        _ if y < s(TITLEBAR) => HTCAPTION,
        _ => HTCLIENT,
    }
}

/// FPS field text to a frame rate; empty or garbage means the default.
pub fn parse_fps(text: &str) -> u32 {
    text.trim().parse::<u64>().map_or(DEFAULT_FPS, |v| {
        v.clamp(MIN_FPS as u64, MAX_FPS as u64) as u32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPIS: [u32; 4] = [96, 120, 144, 192];

    fn size(r: &RECT) -> (i32, i32) {
        (r.right - r.left, r.bottom - r.top)
    }

    fn overlaps(a: &RECT, b: &RECT) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    #[test]
    fn hole_matches_requested_size() {
        for dpi in DPIS {
            let (w, h) = window_for_hole(dpi, 640, 360);
            assert_eq!(size(&hole(dpi, w, h)), (640, 360), "dpi {dpi}");
        }
    }

    #[test]
    fn hole_never_inverts_when_tiny() {
        let r = hole(96, 1, 1);
        assert!(r.right >= r.left && r.bottom >= r.top);
    }

    #[test]
    fn clamp_hole_fits_screen() {
        assert_eq!(clamp_hole(96, 99999, 99999, (1920, 1080)), (1908, 1004));
        assert_eq!(clamp_hole(96, 0, -5, (1920, 1080)), (1, 1));
        assert_eq!(clamp_hole(96, 640, 360, (1920, 1080)), (640, 360));
        for dpi in DPIS {
            let (hw, hh) = clamp_hole(dpi, 99999, 99999, (3840, 2160));
            assert_eq!(window_for_hole(dpi, hw, hh), (3840, 2160), "dpi {dpi}");
        }
    }

    #[test]
    fn parse_fps_clamps_and_defaults() {
        assert_eq!(parse_fps("30"), 30);
        assert_eq!(parse_fps(" 45 "), 45);
        assert_eq!(parse_fps("2000"), MAX_FPS);
        assert_eq!(parse_fps("99999999999999999999"), DEFAULT_FPS);
        assert_eq!(parse_fps("3"), MIN_FPS);
        assert_eq!(parse_fps("0"), MIN_FPS);
        assert_eq!(parse_fps(""), DEFAULT_FPS);
        assert_eq!(parse_fps("abc"), DEFAULT_FPS);
    }

    #[test]
    fn hit_test_regions() {
        for dpi in DPIS {
            let (w, h) = window_for_hole(dpi, 640, 360);
            let s = |v| scale(v, dpi);
            let ht = |x, y| hit_test(dpi, w, h, x, y);
            assert_eq!(ht(0, 0), HTTOPLEFT, "dpi {dpi}");
            assert_eq!(ht(w - 1, 0), HTTOPRIGHT);
            assert_eq!(ht(0, h - 1), HTBOTTOMLEFT);
            assert_eq!(ht(w - 1, h - 1), HTBOTTOMRIGHT);
            assert_eq!(ht(w / 2, 0), HTTOP);
            assert_eq!(ht(w / 2, h - 1), HTBOTTOM);
            assert_eq!(ht(0, h / 2), HTLEFT);
            assert_eq!(ht(w - 1, h / 2), HTRIGHT);
            // Corner grips extend along the edges.
            assert_eq!(ht(0, s(GRIP) - 1), HTTOPLEFT);
            assert_eq!(ht(s(GRIP) - 1, h - 1), HTBOTTOMLEFT);
            // Title bar drags, except over the close button.
            assert_eq!(ht(w / 2, s(TITLEBAR) / 2), HTCAPTION);
            assert_eq!(ht(w - s(CLOSE_W) / 2, s(TITLEBAR) / 2), HTCLIENT);
            // The hole and toolbar are client area.
            assert_eq!(ht(w / 2, h / 2), HTCLIENT);
            assert_eq!(ht(w / 2, h - s(TOOLBAR) / 2), HTCLIENT);
        }
    }

    #[test]
    fn toolbar_fits_at_minimum_width() {
        for dpi in DPIS {
            let (w, h) = min_window(dpi);
            let tb = toolbar(dpi, w, h);
            let items = [
                tb.rec,
                tb.fps_label,
                tb.fps,
                tb.w,
                tb.x_label,
                tb.h,
                tb.recent,
            ];
            let hole = hole(dpi, w, h);
            for (i, a) in items.iter().enumerate() {
                assert!(
                    a.left >= 0 && a.right <= w,
                    "item {i} off-window at dpi {dpi}"
                );
                assert!(
                    a.top >= hole.bottom && a.bottom <= h,
                    "item {i} outside toolbar"
                );
                for (j, b) in items.iter().enumerate().skip(i + 1) {
                    assert!(!overlaps(a, b), "items {i} and {j} overlap at dpi {dpi}");
                }
            }
        }
    }

    #[test]
    fn edit_is_centered_in_field() {
        let field = RECT {
            left: 100,
            top: 400,
            right: 140,
            bottom: 428,
        };
        let e = edit_in_field(96, &field, 16);
        assert_eq!((e.top - field.top, field.bottom - e.bottom), (6, 6));
        assert!(e.left > field.left && e.right < field.right);
    }

    #[test]
    fn title_stops_before_close_button() {
        for dpi in DPIS {
            let w = 800;
            assert!(title(dpi, w).right < close_button(dpi, w).left);
        }
    }
}
