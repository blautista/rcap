//! Recent recordings: the GIFs in %TEMP%\rcap, newest first. Only the last
//! MAX are kept; older ones are deleted.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX: usize = 10;

pub struct Gif {
    pub path: PathBuf,
    /// Milliseconds since the Unix epoch, from the file name.
    pub stamp: u128,
    pub size: u64,
    pub w: u16,
    pub h: u16,
}

pub fn dir() -> PathBuf {
    std::env::temp_dir().join("rcap")
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Path for a new recording.
pub fn new_path() -> PathBuf {
    let dir = dir();
    let _ = fs::create_dir_all(&dir);
    dir.join(format!("rcap-{}.gif", now_ms()))
}

/// File size, width, height.
type Info = (u64, u16, u16);

/// Every rcap-*.gif, newest first, with its info if the header is readable
/// (it isn't for recordings still being written, or cut off by a crash).
fn scan() -> Vec<(PathBuf, u128, Option<Info>)> {
    let Ok(entries) = fs::read_dir(dir()) else {
        return vec![];
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            let stamp: u128 = name
                .strip_prefix("rcap-")?
                .strip_suffix(".gif")?
                .parse()
                .ok()?;
            let info = (|| {
                let size = e.metadata().ok()?.len();
                // Logical screen size from the GIF header.
                let mut header = [0u8; 10];
                fs::File::open(&path).ok()?.read_exact(&mut header).ok()?;
                Some((
                    size,
                    u16::from_le_bytes([header[6], header[7]]),
                    u16::from_le_bytes([header[8], header[9]]),
                ))
            })();
            Some((path, stamp, info))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files
}

/// Readable recordings, newest first.
pub fn list() -> Vec<Gif> {
    scan()
        .into_iter()
        .filter_map(|(path, stamp, info)| {
            info.map(|(size, w, h)| Gif {
                path,
                stamp,
                size,
                w,
                h,
            })
        })
        .collect()
}

/// Delete all but the newest `keep` recordings, plus broken files old enough
/// that they can't still be in progress.
pub fn prune(keep: usize) {
    let mut kept = 0;
    for (path, stamp, info) in scan() {
        let old = now_ms().saturating_sub(stamp) > 10 * 60 * 1000;
        let delete = match info {
            Some(_) => {
                kept += 1;
                kept > keep
            }
            None => old,
        };
        if delete {
            let _ = fs::remove_file(path);
        }
    }
}

/// "850 KB", "1.4 MB".
pub fn size_label(bytes: u64) -> String {
    match bytes {
        0..=1_048_575 => format!("{} KB", bytes.div_ceil(1024)),
        _ => format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(stamp: u128) -> String {
    let s = now_ms().saturating_sub(stamp) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        86400..=172799 => "yesterday".into(),
        _ => format!("{} days ago", s / 86400),
    }
}
