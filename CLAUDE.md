# rcap

A minimal LICEcap clone for Windows: a resizable window with a see-through
hole, FPS and size fields, and Record/Stop. Stopping encodes a GIF and copies
it to the clipboard. Rust + raw Win32 (`windows-sys`), no UI framework.

## Commands

```
cargo build --release          # binary: target\release\rcap.exe
cargo test                     # all automated tests (fast, no window)
cargo fmt                      # format
cargo clippy --all-targets     # lint; keep it at zero warnings
```

If a build fails with "Access is denied", rcap.exe is still running:
`taskkill /IM rcap.exe /F`.

## Code map

- `src/main.rs` — window creation, message loop, `wndproc`, recording state
  machine (Idle → Recording → Encoding → Idle). Thin wiring only: geometry
  goes in `layout.rs`, painting in `draw.rs`.
- `src/layout.rs` — pure geometry and input rules: hole rect, toolbar layout,
  hit-testing, FPS/size parsing and clamping. All functions of DPI + size.
- `src/draw.rs` — all painting, into any HDC. Colors, fonts, the frame and the
  custom-drawn buttons.
- `src/recent.rs` — recent GIFs in `%TEMP%\rcap` (keep newest 10, prune broken).
- `src/capture.rs` — BitBlt capture thread. `src/encode.rs` — streaming GIF
  encoder. `src/clipboard.rs` — CF_HDROP clipboard copy.

## Testing

### Automated (default — use these first)

`cargo test` runs everything in well under a second. None of the tests opens a
window, moves the mouse or reads the screen.

- **Logic** — `layout.rs` and `recent.rs` tests. Keep logic pure so it stays
  testable: take DPI, sizes, folders and the current time as parameters rather
  than reading them from Windows inside the function (see `recent::prune_in`,
  `recent::ago`). `recent.rs` tests use a throwaway temp folder, never the real
  `%TEMP%\rcap`.
- **Drawing** — `draw.rs` tests render into an off-screen DIB (`Canvas`) and
  assert pixel colors at chosen points. Pick points clear of text, and compare
  against the palette constants. ClearType adds colored fringes to text, so
  for text color assert on counts or ratios, not exact zeros.
- **Encoder** — `encode.rs` round-trip test decodes the GIF and checks every
  pixel and delay.

When changing behavior or looks, add or update a test in the matching module.
When adding a new visual state, add it to `draw.rs` tests and, if useful, to
`window_snapshots`.

### Snapshots (to look at the UI without running it)

```
$env:RCAP_SNAPSHOTS="1"; cargo test; Remove-Item Env:RCAP_SNAPSHOTS
```

writes PNGs of each state to `target\snapshots\` (`window_idle.png`,
`window_recording.png`, `window_focus_hover.png`, buttons, frame states).
View them to review visual changes. The magenta area is the hole, which is
see-through in the real app. Child edit controls aren't real in snapshots;
their text is drawn in for realism.

### Live testing (only when automated tests can't cover it)

Some bugs only exist in the real window as Windows composites it: resize
flicker or gray bands, the whole window flashing transparent, click-through in
the hole, the classic frame painted on focus change, DWM corners/shadow/border.

**Ask the user before any live test that:**
- captures the screen (screenshots, `ddagrab`/ffmpeg recordings), or
- records with rcap itself (the hole captures whatever is behind it), or
- moves the mouse or sends real keyboard input.

The user decides when; they may need to hide things or keep hands off.
Afterwards, delete any screenshots, recordings and test GIFs created, and
clear the clipboard if a test put a recording on it.

Without asking, it's fine to launch rcap and drive it through window messages
(`PostMessage` of `WM_COMMAND` with control IDs, e.g. 101 = Record) and read
its title text, as long as nothing records. Background scripts can't change
the active window normally (Windows' foreground lock); attach with
`AttachThreadInput` to the foreground thread first.

## Platform gotchas (learned the hard way)

- The window is layered with a color key (`draw::KEY`, magenta) for the
  see-through, click-through hole. Capture uses BitBlt **without**
  CAPTUREBLT, which skips layered windows, so rcap never records itself.
- Custom frame: `WM_NCCALCSIZE` returns 0 and `layout::hit_test` provides
  resize/caption areas. `WS_THICKFRAME` is required for resizing. The system
  frame breaks color-key click handling; `SetWindowRgn` causes transparent
  flashes during resize. Don't go back to either.
- DWM doesn't draw frames for layered windows, so `WM_NCPAINT` returns 0 and
  `WM_NCACTIVATE` passes lParam -1; otherwise the classic thick frame gets
  painted over the edges on focus change.
- Paint is double-buffered and child moves use `DeferWindowPos`; otherwise the
  compositor catches half-painted frames (gray bands in the hole while
  resizing).
- GDI can't antialias, so avoid thick rounded strokes (focus ring is a 1px
  border color change).
