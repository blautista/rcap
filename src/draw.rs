//! All painting. Everything draws into a given HDC, so tests render it into
//! an off-screen bitmap and check pixels (and can save PNG snapshots).

use std::{mem, ptr};

use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::layout::{self, scale, RADIUS};
use crate::wide;

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// The hole is painted this color, which the layered window makes see-through
/// and click-through. Capture uses BitBlt without CAPTUREBLT, which skips
/// layered windows, so the recording shows what's underneath.
pub const KEY: COLORREF = rgb(255, 0, 255);

// Windows 11 light palette.
pub const BG: COLORREF = rgb(243, 243, 243);
pub const TEXT: COLORREF = rgb(26, 26, 26);
pub const SUBTLE: COLORREF = rgb(96, 96, 96);
pub const STROKE: COLORREF = rgb(229, 229, 229);
pub const HOLE_STROKE: COLORREF = rgb(200, 200, 200);
pub const FIELD: COLORREF = rgb(255, 255, 255);
pub const FIELD_OFF: COLORREF = rgb(249, 249, 249);
pub const FIELD_LINE: COLORREF = rgb(134, 134, 134);
pub const ACCENT: COLORREF = rgb(0, 95, 184);
pub const RED: COLORREF = rgb(196, 43, 28);
pub const RED_HOT: COLORREF = rgb(212, 62, 48);
pub const RED_DOWN: COLORREF = rgb(168, 36, 24);
pub const BTN: COLORREF = rgb(251, 251, 251);
pub const BTN_HOT: COLORREF = rgb(246, 246, 246);
pub const BTN_DOWN: COLORREF = rgb(240, 240, 240);
pub const OFF: COLORREF = rgb(229, 229, 229);
pub const OFF_TEXT: COLORREF = rgb(140, 140, 140);

const CLOSE_GLYPH: &str = "\u{E8BB}";
const RECENT_GLYPH: &str = "\u{E81C}";

/// Fonts for one DPI.
#[derive(Clone, Copy)]
pub struct Fonts {
    pub text: HFONT,
    /// Segoe MDL2 Assets (present on Windows 10 and 11), for glyphs.
    pub icon: HFONT,
    pub dpi: u32,
}

impl Fonts {
    pub unsafe fn new(dpi: u32) -> Fonts {
        let mut ncm: NONCLIENTMETRICSW = mem::zeroed();
        ncm.cbSize = mem::size_of::<NONCLIENTMETRICSW>() as u32;
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            &mut ncm as *mut _ as *mut _,
            0,
            dpi,
        );
        let icon = CreateFontW(
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
        );
        Fonts {
            text: CreateFontIndirectW(&ncm.lfMessageFont),
            icon,
            dpi,
        }
    }

    pub unsafe fn delete(&self) {
        DeleteObject(self.text);
        DeleteObject(self.icon);
    }

    /// Line height of the text font.
    pub unsafe fn text_height(&self) -> i32 {
        let dc = CreateCompatibleDC(ptr::null_mut());
        let old = SelectObject(dc, self.text);
        let mut tm: TEXTMETRICW = mem::zeroed();
        GetTextMetricsW(dc, &mut tm);
        SelectObject(dc, old);
        DeleteDC(dc);
        tm.tmHeight
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Fps,
    W,
    H,
}

/// Everything the window frame's look depends on.
pub struct Frame<'a> {
    pub title: &'a str,
    pub recording: bool,
    pub close_hot: bool,
    pub close_down: bool,
    pub fields_enabled: bool,
    pub focus: Option<Field>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Record,
    Recent,
}

/// A button's interaction state (from comctl6 custom draw).
#[derive(Clone, Copy, Default)]
pub struct ButtonState {
    pub hot: bool,
    pub pressed: bool,
    pub disabled: bool,
    pub keyboard_focus: bool,
}

pub unsafe fn fill(hdc: HDC, r: &RECT, color: COLORREF) {
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

unsafe fn text(hdc: HDC, font: HFONT, s: &str, r: &RECT, color: COLORREF, flags: DRAW_TEXT_FORMAT) {
    let w: Vec<u16> = s.encode_utf16().collect();
    let mut r = *r;
    let old = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, color);
    DrawTextW(
        hdc,
        w.as_ptr(),
        w.len() as i32,
        &mut r,
        flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
    );
    SelectObject(hdc, old);
}

unsafe fn text_width(hdc: HDC, font: HFONT, s: &str) -> i32 {
    let w: Vec<u16> = s.encode_utf16().collect();
    let mut m: RECT = mem::zeroed();
    let old = SelectObject(hdc, font);
    DrawTextW(
        hdc,
        w.as_ptr(),
        w.len() as i32,
        &mut m,
        DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
    );
    SelectObject(hdc, old);
    m.right
}

/// The window itself (everything except the child controls) at w x h.
pub unsafe fn frame(hdc: HDC, f: &Fonts, w: i32, h: i32, st: &Frame) {
    let dpi = f.dpi;
    let s = |v| scale(v, dpi);
    fill(
        hdc,
        &RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        },
        BG,
    );

    let hole = layout::hole(dpi, w, h);
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
    let title_color = if st.recording { RED } else { TEXT };
    text(
        hdc,
        f.text,
        st.title,
        &layout::title(dpi, w),
        title_color,
        DT_END_ELLIPSIS,
    );
    let close = layout::close_button(dpi, w);
    if st.close_hot {
        fill(hdc, &close, if st.close_down { RED_DOWN } else { RED });
    }
    let glyph = if st.close_hot { FIELD } else { TEXT };
    text(hdc, f.icon, CLOSE_GLYPH, &close, glyph, DT_CENTER);

    // Toolbar: labels and the boxes the borderless edits sit in.
    let tb = layout::toolbar(dpi, w, h);
    text(hdc, f.text, "FPS", &tb.fps_label, SUBTLE, DT_RIGHT);
    text(hdc, f.text, "×", &tb.x_label, SUBTLE, DT_CENTER);
    for (field, r) in [(Field::Fps, &tb.fps), (Field::W, &tb.w), (Field::H, &tb.h)] {
        let bg = if st.fields_enabled { FIELD } else { FIELD_OFF };
        round_rect(hdc, r, s(RADIUS), bg, STROKE);
        if st.fields_enabled {
            let (line, thick) = if st.focus == Some(field) {
                (ACCENT, s(2))
            } else {
                (FIELD_LINE, 1)
            };
            let bottom = RECT {
                left: r.left + s(RADIUS) / 2,
                top: r.bottom - thick,
                right: r.right - s(RADIUS) / 2,
                bottom: r.bottom,
            };
            fill(hdc, &bottom, line);
        }
    }
}

/// A toolbar button filling r. Neutral, except Record turns solid red as the
/// Stop button while recording.
pub unsafe fn button(
    hdc: HDC,
    f: &Fonts,
    r: RECT,
    kind: Button,
    label: &str,
    recording: bool,
    st: ButtonState,
) {
    fill(hdc, &r, BG);
    let stop = kind == Button::Record && recording;
    let (bg, border, fg) = match () {
        _ if st.disabled => (OFF, OFF, OFF_TEXT),
        _ if stop && st.pressed => (RED_DOWN, RED_DOWN, FIELD),
        _ if stop && st.hot => (RED_HOT, RED_HOT, FIELD),
        _ if stop => (RED, RED, FIELD),
        _ if st.pressed => (BTN_DOWN, STROKE, SUBTLE),
        _ if st.hot => (BTN_HOT, STROKE, TEXT),
        _ => (BTN, STROKE, TEXT),
    };
    // Keyboard focus just darkens the 1px border: GDI can't antialias a thick ring.
    let border = if st.keyboard_focus && !st.disabled {
        TEXT
    } else {
        border
    };
    round_rect(hdc, &r, scale(RADIUS, f.dpi), bg, border);

    // An icon in front of the label: Record's red dot (idle), Recent's clock.
    let icon = match label.strip_prefix('●') {
        Some(rest) if !st.disabled => Some(("●", f.text, RED, rest.to_string())),
        _ if kind == Button::Recent => Some((RECENT_GLYPH, f.icon, fg, format!("  {label}"))),
        _ => None,
    };
    match icon {
        Some((glyph, font, color, rest)) => {
            let (icon_w, rest_w) = (text_width(hdc, font, glyph), text_width(hdc, f.text, &rest));
            let left = r.left + (r.right - r.left - icon_w - rest_w) / 2;
            let icon_rc = RECT {
                left,
                right: left + icon_w,
                ..r
            };
            text(hdc, font, glyph, &icon_rc, color, DT_LEFT);
            let rest_rc = RECT {
                left: left + icon_w,
                ..r
            };
            text(hdc, f.text, &rest, &rest_rc, fg, DT_LEFT);
        }
        None => text(hdc, f.text, label, &r, fg, DT_CENTER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// An off-screen 32-bit top-down bitmap we can draw into and read back.
    struct Canvas {
        dc: HDC,
        bmp: HBITMAP,
        old: HGDIOBJ,
        bits: *const u32,
        w: i32,
        h: i32,
    }

    impl Canvas {
        fn new(w: i32, h: i32) -> Canvas {
            unsafe {
                let dc = CreateCompatibleDC(ptr::null_mut());
                let mut bmi: BITMAPINFO = mem::zeroed();
                bmi.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
                bmi.bmiHeader.biWidth = w;
                bmi.bmiHeader.biHeight = -h;
                bmi.bmiHeader.biPlanes = 1;
                bmi.bmiHeader.biBitCount = 32;
                bmi.bmiHeader.biCompression = BI_RGB;
                let mut bits = ptr::null_mut();
                let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
                assert!(!bmp.is_null());
                let old = SelectObject(dc, bmp);
                Canvas {
                    dc,
                    bmp,
                    old,
                    bits: bits as *const u32,
                    w,
                    h,
                }
            }
        }

        fn px(&self, x: i32, y: i32) -> COLORREF {
            unsafe { GdiFlush() };
            self.raw(x, y)
        }

        /// Pixel without flushing pending GDI drawing first.
        fn raw(&self, x: i32, y: i32) -> COLORREF {
            assert!(
                x >= 0 && y >= 0 && x < self.w && y < self.h,
                "({x},{y}) out of bounds"
            );
            // DIB pixels are 0x00RRGGBB; COLORREF is 0x00BBGGRR.
            let p = unsafe { *self.bits.add((y * self.w + x) as usize) };
            rgb((p >> 16) as u8, (p >> 8) as u8, p as u8)
        }

        /// Pixels in r matching the predicate.
        fn count(&self, r: &RECT, pred: impl Fn(u8, u8, u8) -> bool) -> usize {
            unsafe { GdiFlush() };
            let mut n = 0;
            for y in r.top..r.bottom {
                for x in r.left..r.right {
                    let c = self.raw(x, y);
                    if pred(c as u8, (c >> 8) as u8, (c >> 16) as u8) {
                        n += 1;
                    }
                }
            }
            n
        }

        /// With RCAP_SNAPSHOTS=1, save as target/snapshots/<name>.png to look at.
        fn snapshot(&self, name: &str) {
            if std::env::var_os("RCAP_SNAPSHOTS").is_none() {
                return;
            }
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/snapshots");
            std::fs::create_dir_all(&dir).unwrap();
            unsafe { GdiFlush() };
            let mut rgba = Vec::with_capacity((self.w * self.h * 4) as usize);
            for y in 0..self.h {
                for x in 0..self.w {
                    let c = self.raw(x, y);
                    rgba.extend_from_slice(&[c as u8, (c >> 8) as u8, (c >> 16) as u8, 255]);
                }
            }
            let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
            let mut enc =
                png::Encoder::new(std::io::BufWriter::new(file), self.w as u32, self.h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
    }

    impl Drop for Canvas {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.dc, self.old);
                DeleteObject(self.bmp);
                DeleteDC(self.dc);
            }
        }
    }

    const W: i32 = 652;
    const H: i32 = 436;

    fn idle() -> Frame<'static> {
        Frame {
            title: "rcap",
            recording: false,
            close_hot: false,
            close_down: false,
            fields_enabled: true,
            focus: None,
        }
    }

    fn render_frame(st: &Frame) -> Canvas {
        let c = Canvas::new(W, H);
        unsafe {
            let f = Fonts::new(96);
            frame(c.dc, &f, W, H, st);
            f.delete();
        }
        c
    }

    fn render_button(kind: Button, label: &str, recording: bool, st: ButtonState) -> Canvas {
        let c = Canvas::new(96, 28);
        unsafe {
            let f = Fonts::new(96);
            let r = RECT {
                left: 0,
                top: 0,
                right: 96,
                bottom: 28,
            };
            button(c.dc, &f, r, kind, label, recording, st);
            f.delete();
        }
        c
    }

    fn reddish(r: u8, g: u8, b: u8) -> bool {
        r > 150 && g < 110 && b < 110
    }

    fn whitish(r: u8, g: u8, b: u8) -> bool {
        r > 220 && g > 220 && b > 220
    }

    #[test]
    fn hole_is_keyed_and_outlined() {
        let c = render_frame(&idle());
        let hole = layout::hole(96, W, H);
        let mid = (hole.top + hole.bottom) / 2;
        assert_eq!(c.px(W / 2, mid), KEY);
        assert_eq!(c.px(hole.left, hole.top), KEY);
        assert_eq!(c.px(hole.right - 1, hole.bottom - 1), KEY);
        assert_eq!(c.px(hole.left - 1, mid), HOLE_STROKE);
        assert_eq!(c.px(hole.right, mid), HOLE_STROKE);
        assert_eq!(c.px(1, mid), BG);
        // Nothing outside the hole is keyed (would be see-through).
        let keyed = c.count(
            &RECT {
                left: 0,
                top: 0,
                right: W,
                bottom: H,
            },
            |r, g, b| rgb(r, g, b) == KEY,
        );
        let (hw, hh) = (hole.right - hole.left, hole.bottom - hole.top);
        assert_eq!(keyed, (hw * hh) as usize);
        c.snapshot("frame_idle");
    }

    #[test]
    fn close_button_hover_and_press() {
        let close = layout::close_button(96, W);
        let corner = (close.left + 2, close.top + 2);
        let c = render_frame(&idle());
        assert_eq!(c.px(corner.0, corner.1), BG);
        assert!(
            c.count(&close, |r, g, b| r < 100 && g < 100 && b < 100) > 0,
            "glyph drawn"
        );

        let hot = Frame {
            close_hot: true,
            ..idle()
        };
        let c = render_frame(&hot);
        assert_eq!(c.px(corner.0, corner.1), RED);
        assert!(c.count(&close, whitish) > 0, "white glyph on red");
        c.snapshot("frame_close_hot");

        let down = Frame {
            close_hot: true,
            close_down: true,
            ..idle()
        };
        assert_eq!(render_frame(&down).px(corner.0, corner.1), RED_DOWN);
    }

    #[test]
    fn title_turns_red_while_recording() {
        // Same text both ways, so only the color differs. ClearType fringes
        // give dark text a few reddish edge pixels, hence the ratio.
        let text = "●  REC 0:01.0  ·  30 frames";
        let title = layout::title(96, W);
        let c = render_frame(&Frame {
            title: text,
            ..idle()
        });
        let idle_red = c.count(&title, reddish);
        assert!(c.count(&title, |r, g, b| r < 100 && g < 100 && b < 100) > 0);

        let rec = Frame {
            title: text,
            recording: true,
            fields_enabled: false,
            ..idle()
        };
        let c = render_frame(&rec);
        let rec_red = c.count(&title, reddish);
        assert!(
            rec_red > 10 * idle_red.max(1),
            "red {rec_red} vs idle {idle_red}"
        );
        c.snapshot("frame_recording");
    }

    #[test]
    fn fields_show_focus_and_disabled() {
        let tb = layout::toolbar(96, W, H);
        let line = |r: &RECT, dy| ((r.left + r.right) / 2, r.bottom - dy);

        let c = render_frame(&idle());
        for r in [&tb.fps, &tb.w, &tb.h] {
            assert_eq!(c.px(r.left + 3, (r.top + r.bottom) / 2), FIELD);
            let (x, y) = line(r, 1);
            assert_eq!(c.px(x, y), FIELD_LINE);
        }

        let focused = Frame {
            focus: Some(Field::W),
            ..idle()
        };
        let c = render_frame(&focused);
        for dy in [1, 2] {
            let (x, y) = line(&tb.w, dy);
            assert_eq!(c.px(x, y), ACCENT, "focused underline is 2px");
        }
        let (x, y) = line(&tb.fps, 1);
        assert_eq!(c.px(x, y), FIELD_LINE, "other fields unchanged");
        c.snapshot("frame_focus_w");

        let disabled = Frame {
            fields_enabled: false,
            ..idle()
        };
        let c = render_frame(&disabled);
        for r in [&tb.fps, &tb.w, &tb.h] {
            assert_eq!(c.px(r.left + 3, (r.top + r.bottom) / 2), FIELD_OFF);
            let (x, y) = line(r, 1);
            assert_eq!(c.px(x, y), STROKE, "no underline when disabled");
        }
    }

    // A point inside the button's fill, clear of the centered label.
    const FILL: (i32, i32) = (4, 14);
    const TOP_EDGE: (i32, i32) = (48, 0);

    #[test]
    fn record_button_is_neutral_with_red_dot() {
        let c = render_button(Button::Record, "●  Record", false, ButtonState::default());
        assert_eq!(c.px(FILL.0, FILL.1), BTN);
        assert_eq!(c.px(TOP_EDGE.0, TOP_EDGE.1), STROKE);
        assert_eq!(c.px(0, 0), BG, "rounded corner");
        assert!(c.count(&full(&c), reddish) > 0, "red dot");
        c.snapshot("button_record");

        let hot = ButtonState {
            hot: true,
            ..Default::default()
        };
        let pressed = ButtonState {
            pressed: true,
            ..Default::default()
        };
        let px = |st| render_button(Button::Record, "●  Record", false, st).px(FILL.0, FILL.1);
        assert_eq!(px(hot), BTN_HOT);
        assert_eq!(px(pressed), BTN_DOWN);
    }

    #[test]
    fn stop_button_is_red() {
        let st = ButtonState::default();
        let c = render_button(Button::Record, "■  Stop", true, st);
        assert_eq!(c.px(FILL.0, FILL.1), RED);
        assert!(c.count(&full(&c), whitish) > 0, "white label");
        c.snapshot("button_stop");
        let hot = ButtonState { hot: true, ..st };
        let pressed = ButtonState {
            pressed: true,
            ..st
        };
        let px = |st| render_button(Button::Record, "■  Stop", true, st).px(FILL.0, FILL.1);
        assert_eq!(px(hot), RED_HOT);
        assert_eq!(px(pressed), RED_DOWN);
    }

    #[test]
    fn recent_button_is_never_red() {
        for recording in [false, true] {
            let c = render_button(Button::Recent, "Recent", recording, ButtonState::default());
            assert_eq!(c.px(FILL.0, FILL.1), BTN);
            assert_eq!(c.count(&full(&c), reddish), 0);
        }
        let disabled = ButtonState {
            disabled: true,
            ..Default::default()
        };
        let c = render_button(Button::Recent, "Recent", true, disabled);
        assert_eq!(c.px(FILL.0, FILL.1), OFF);
        c.snapshot("button_recent_disabled");
    }

    #[test]
    fn keyboard_focus_darkens_border_only() {
        let focus = ButtonState {
            keyboard_focus: true,
            ..Default::default()
        };
        for kind in [Button::Record, Button::Recent] {
            let c = render_button(kind, "Recent", false, focus);
            assert_eq!(c.px(TOP_EDGE.0, TOP_EDGE.1), TEXT, "{kind:?}");
            assert_eq!(c.px(TOP_EDGE.0, 1), BTN, "{kind:?} ring is 1px");
        }
        let disabled = ButtonState {
            disabled: true,
            ..focus
        };
        let c = render_button(Button::Recent, "Recent", false, disabled);
        assert_eq!(c.px(TOP_EDGE.0, TOP_EDGE.1), OFF);
    }

    fn full(c: &Canvas) -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: c.w,
            bottom: c.h,
        }
    }

    /// A whole-window mockup per state, for eyeballing with RCAP_SNAPSHOTS=1.
    #[test]
    fn window_snapshots() {
        let states: [(&str, Frame, bool); 3] = [
            ("window_idle", idle(), false),
            (
                "window_recording",
                Frame {
                    title: "●  REC 0:02.1  ·  63 frames",
                    recording: true,
                    fields_enabled: false,
                    ..idle()
                },
                true,
            ),
            (
                "window_focus_hover",
                Frame {
                    focus: Some(Field::Fps),
                    close_hot: true,
                    ..idle()
                },
                false,
            ),
        ];
        for (name, st, recording) in states {
            let c = Canvas::new(W, H);
            unsafe {
                let f = Fonts::new(96);
                frame(c.dc, &f, W, H, &st);
                let tb = layout::toolbar(96, W, H);
                let (label, disabled) = if recording {
                    ("■  Stop", false)
                } else {
                    ("●  Record", false)
                };
                let rec_state = ButtonState {
                    disabled,
                    ..Default::default()
                };
                button(
                    c.dc,
                    &f,
                    tb.rec,
                    Button::Record,
                    label,
                    recording,
                    rec_state,
                );
                let recent_state = ButtonState {
                    disabled: recording,
                    ..Default::default()
                };
                button(
                    c.dc,
                    &f,
                    tb.recent,
                    Button::Recent,
                    "Recent",
                    recording,
                    recent_state,
                );
                // The edits' text, as the real child controls would show it.
                let color = if st.fields_enabled { TEXT } else { OFF_TEXT };
                for (r, s) in [(&tb.fps, "30"), (&tb.w, "640"), (&tb.h, "360")] {
                    text(c.dc, f.text, s, r, color, DT_CENTER);
                }
                f.delete();
            }
            c.snapshot(name);
        }
    }
}
