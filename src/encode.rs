//! Streaming GIF encoder. Runs while recording so stopping is near-instant.
//!
//! Each frame is diffed against the previous one: identical frames just extend
//! the previous frame's delay, otherwise only the changed bounding box is
//! emitted, with unchanged pixels inside it marked transparent.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::Receiver;

use color_quant::NeuQuant;
use rayon::prelude::*;

use crate::capture::Msg;

const TRANSPARENT: u8 = 255;

struct Patch {
    start_cs: u32,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    rgba: Vec<u8>,
}

pub fn run(rx: Receiver<Msg>, w: u16, h: u16, path: &Path) -> Result<u64, String> {
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut enc = gif::Encoder::new(BufWriter::new(file), w, h, &[]).map_err(|e| e.to_string())?;
    enc.set_repeat(gif::Repeat::Infinite).map_err(|e| e.to_string())?;

    let cs = |ms: u32| (ms + 5) / 10;
    let batch_size = rayon::current_num_threads() * 2;
    let (wu, hu) = (w as usize, h as usize);
    let mut prev: Option<Vec<u8>> = None;
    let mut pending: Option<Patch> = None;
    let mut batch: Vec<(Patch, u16)> = Vec::new();
    let mut end_ms = None;

    for msg in rx {
        match msg {
            Msg::Frame(t, bgra) => {
                let bbox = match &prev {
                    None => Some((0, 0, wu, hu)),
                    Some(p) => diff_bbox(p, &bgra, wu, hu),
                };
                let Some((px, py, pw, ph)) = bbox else { continue };

                let mut rgba = Vec::with_capacity(pw * ph * 4);
                for y in py..py + ph {
                    let row = (y * wu + px) * 4..(y * wu + px + pw) * 4;
                    let cur = &bgra[row.clone()];
                    match &prev {
                        Some(p) => {
                            for (c, o) in cur.chunks_exact(4).zip(p[row].chunks_exact(4)) {
                                if c == o {
                                    rgba.extend_from_slice(&[0, 0, 0, 0]);
                                } else {
                                    rgba.extend_from_slice(&[c[2], c[1], c[0], 255]);
                                }
                            }
                        }
                        None => {
                            for c in cur.chunks_exact(4) {
                                rgba.extend_from_slice(&[c[2], c[1], c[0], 255]);
                            }
                        }
                    }
                }

                let mut start_cs = cs(t);
                if let Some(p) = pending.take() {
                    // GIF players clamp delays < 2cs, so never emit one.
                    start_cs = start_cs.max(p.start_cs + 2);
                    let delay = (start_cs - p.start_cs).min(u16::MAX as u32) as u16;
                    batch.push((p, delay));
                }
                pending = Some(Patch { start_cs, x: px as u16, y: py as u16, w: pw as u16, h: ph as u16, rgba });
                prev = Some(bgra);

                if batch.len() >= batch_size {
                    flush(&mut enc, &mut batch)?;
                }
            }
            Msg::End(t) => {
                end_ms = Some(t);
                break;
            }
        }
    }

    let last = pending.ok_or("no frames captured")?;
    let delay = match end_ms {
        Some(t) => cs(t).saturating_sub(last.start_cs).clamp(2, u16::MAX as u32) as u16,
        None => 10,
    };
    batch.push((last, delay));
    flush(&mut enc, &mut batch)?;

    let mut out = enc.into_inner().map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())?;
    let size = out.get_ref().metadata().map(|m| m.len()).unwrap_or(0);
    Ok(size)
}

fn flush<W: Write>(enc: &mut gif::Encoder<W>, batch: &mut Vec<(Patch, u16)>) -> Result<(), String> {
    let frames: Vec<gif::Frame> = std::mem::take(batch)
        .into_par_iter()
        .map(|(p, delay)| quantize(p, delay))
        .collect();
    for f in &frames {
        enc.write_frame(f).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Bounding box (x, y, w, h) of pixels that differ between two frames.
fn diff_bbox(a: &[u8], b: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let stride = w * 4;
    let row = |y: usize| y * stride..(y + 1) * stride;
    let top = (0..h).find(|&y| a[row(y)] != b[row(y)])?;
    let bottom = (top..h).rev().find(|&y| a[row(y)] != b[row(y)]).unwrap();

    let (mut left, mut right) = (w, 0);
    for y in top..=bottom {
        let (ra, rb) = (&a[row(y)], &b[row(y)]);
        if ra == rb {
            continue;
        }
        let px = |x: usize| ra[x * 4..x * 4 + 4] != rb[x * 4..x * 4 + 4];
        if let Some(l) = (0..left).find(|&x| px(x)) {
            left = l;
        }
        if let Some(r) = (right..w).rev().find(|&x| px(x)) {
            right = r;
        }
    }
    Some((left, top, right - left + 1, bottom - top + 1))
}

/// Exact palette when the patch has <= 255 colors (typical for UI), NeuQuant otherwise.
/// Index 255 is reserved for transparency.
fn quantize(p: Patch, delay: u16) -> gif::Frame<'static> {
    let n = p.w as usize * p.h as usize;
    let mut indices = vec![TRANSPARENT; n];
    let mut has_transparent = false;
    let mut palette = vec![0u8; 256 * 3];

    let mut map: HashMap<[u8; 3], u8> = HashMap::new();
    let mut last: Option<([u8; 3], u8)> = None;
    let mut exact = true;
    for (i, c) in p.rgba.chunks_exact(4).enumerate() {
        if c[3] == 0 {
            has_transparent = true;
            continue;
        }
        let key = [c[0], c[1], c[2]];
        let idx = match last {
            Some((k, v)) if k == key => v,
            _ => match map.get(&key) {
                Some(&v) => v,
                None => {
                    if map.len() == TRANSPARENT as usize {
                        exact = false;
                        break;
                    }
                    let v = map.len() as u8;
                    map.insert(key, v);
                    palette[v as usize * 3..v as usize * 3 + 3].copy_from_slice(&key);
                    v
                }
            },
        };
        last = Some((key, idx));
        indices[i] = idx;
    }

    if !exact {
        let opaque: Vec<u8> = p.rgba.chunks_exact(4).filter(|c| c[3] != 0).flatten().copied().collect();
        let nq = NeuQuant::new(10, TRANSPARENT as usize, &opaque);
        palette = nq.color_map_rgb();
        palette.resize(256 * 3, 0);
        for (i, c) in p.rgba.chunks_exact(4).enumerate() {
            indices[i] = if c[3] == 0 { TRANSPARENT } else { nq.index_of(c) as u8 };
        }
    }

    let mut f = gif::Frame::default();
    f.left = p.x;
    f.top = p.y;
    f.width = p.w;
    f.height = p.h;
    f.buffer = Cow::Owned(indices);
    f.palette = Some(palette);
    f.transparent = has_transparent.then_some(TRANSPARENT);
    f.delay = delay;
    f.dispose = gif::DisposalMethod::Keep;
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Encode synthetic frames, decode them back, and check every pixel and delay.
    #[test]
    fn round_trip() {
        let (w, h) = (64usize, 48usize);
        let frame = |i: usize| -> Vec<u8> {
            let mut v = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let p = (y * w + x) * 4;
                    v[p] = (x / 8 * 30) as u8; // B
                    v[p + 1] = (y / 8 * 40) as u8; // G
                    v[p + 2] = if (10..20).contains(&x) && y == i { 255 } else { 40 }; // R: moving line
                }
            }
            v
        };
        let (tx, rx) = mpsc::sync_channel(100);
        let times = [0u32, 33, 66, 100, 133];
        for (i, &t) in times.iter().enumerate() {
            tx.send(Msg::Frame(t, frame(i.min(3)))).unwrap(); // frames 3 and 4 identical
        }
        tx.send(Msg::End(166)).unwrap();
        drop(tx);

        let path = std::env::temp_dir().join("rcap-test.gif");
        run(rx, w as u16, h as u16, &path).unwrap();

        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::RGBA);
        let mut dec = opts.read_info(File::open(&path).unwrap()).unwrap();
        let mut canvas = vec![0u8; w * h * 4];
        let mut delays = vec![];
        let mut n = 0;
        while let Some(f) = dec.read_next_frame().unwrap() {
            for y in 0..f.height as usize {
                for x in 0..f.width as usize {
                    let s = (y * f.width as usize + x) * 4;
                    if f.buffer[s + 3] != 0 {
                        let d = ((y + f.top as usize) * w + x + f.left as usize) * 4;
                        canvas[d..d + 4].copy_from_slice(&f.buffer[s..s + 4]);
                    }
                }
            }
            let expect = frame(n);
            for i in 0..w * h {
                assert_eq!(
                    &canvas[i * 4..i * 4 + 3],
                    &[expect[i * 4 + 2], expect[i * 4 + 1], expect[i * 4]],
                    "frame {n} pixel {i}"
                );
            }
            delays.push(f.delay);
            n += 1;
        }
        assert_eq!(n, 4, "identical trailing frame should be merged");
        assert_eq!(delays, vec![3, 4, 3, 7]);
        assert_eq!(delays.iter().map(|&d| d as u32).sum::<u32>(), 17);
    }
}
