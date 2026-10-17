//! Recent recordings: the GIFs in %TEMP%\rcap, newest first. Only the last
//! MAX are kept; older ones are deleted.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX: usize = 10;
/// Unreadable files younger than this may still be being written.
const BROKEN_GRACE_MS: u128 = 10 * 60 * 1000;

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

/// Every rcap-*.gif in dir, newest first, with its info if the header is
/// readable (it isn't for recordings still being written, or cut off by a crash).
fn scan(dir: &Path) -> Vec<(PathBuf, u128, Option<Info>)> {
    let Ok(entries) = fs::read_dir(dir) else {
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
    list_in(&dir())
}

fn list_in(dir: &Path) -> Vec<Gif> {
    scan(dir)
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
    prune_in(&dir(), keep, now_ms());
}

fn prune_in(dir: &Path, keep: usize, now: u128) {
    let mut kept = 0;
    for (path, stamp, info) in scan(dir) {
        let delete = match info {
            Some(_) => {
                kept += 1;
                kept > keep
            }
            None => now.saturating_sub(stamp) > BROKEN_GRACE_MS,
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
pub fn ago(stamp: u128, now: u128) -> String {
    let s = now.saturating_sub(stamp) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        86400..=172799 => "yesterday".into(),
        _ => format!("{} days ago", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u128 = 60 * 1000;
    const NOW: u128 = 1_800_000_000_000;

    /// A fresh, empty folder for one test, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!("rcap-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        /// A readable GIF (header plus some body) recorded `age` ms before NOW.
        fn gif(&self, age: u128, w: u16, h: u16) -> PathBuf {
            let mut bytes = b"GIF89a".to_vec();
            bytes.extend_from_slice(&w.to_le_bytes());
            bytes.extend_from_slice(&h.to_le_bytes());
            bytes.resize(2000, 0);
            self.file(&format!("rcap-{}.gif", NOW - age), &bytes)
        }

        /// An empty (still writing, or crashed) recording.
        fn broken(&self, age: u128) -> PathBuf {
            self.file(&format!("rcap-{}.gif", NOW - age), &[])
        }

        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            fs::write(&p, bytes).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn list_is_newest_first_and_skips_junk() {
        let d = TempDir::new("list");
        let old = d.gif(60 * MIN, 640, 360);
        let new = d.gif(MIN, 1280, 720);
        d.broken(0);
        d.file("rcap-notanumber.gif", b"GIF89a....");
        d.file("other.txt", b"hi");

        let gifs = list_in(&d.0);
        let paths: Vec<_> = gifs.iter().map(|g| g.path.clone()).collect();
        assert_eq!(paths, vec![new, old]);
        assert_eq!((gifs[0].w, gifs[0].h, gifs[0].size), (1280, 720, 2000));
        assert_eq!(gifs[0].stamp, NOW - MIN);
    }

    #[test]
    fn prune_keeps_newest_and_young_broken_files() {
        let d = TempDir::new("prune");
        let keep: Vec<_> = (1..=3).map(|i| d.gif(i * MIN, 640, 360)).collect();
        let drop: Vec<_> = (4..=6).map(|i| d.gif(i * MIN, 640, 360)).collect();
        let young_broken = d.broken(MIN / 2);
        let old_broken = d.broken(BROKEN_GRACE_MS + MIN);
        let unrelated = d.file("notes.txt", b"keep me");

        prune_in(&d.0, 3, NOW);

        for p in keep.iter().chain([&young_broken, &unrelated]) {
            assert!(p.exists(), "{} should be kept", p.display());
        }
        for p in drop.iter().chain([&old_broken]) {
            assert!(!p.exists(), "{} should be deleted", p.display());
        }
    }

    #[test]
    fn prune_zero_clears_all_recordings() {
        let d = TempDir::new("clear");
        for i in 1..=4 {
            d.gif(i * MIN, 640, 360);
        }
        prune_in(&d.0, 0, NOW);
        assert!(list_in(&d.0).is_empty());
    }

    #[test]
    fn missing_folder_is_empty() {
        let d = TempDir::new("missing");
        let gone = d.0.join("nope");
        assert!(list_in(&gone).is_empty());
        prune_in(&gone, 0, NOW);
    }

    #[test]
    fn ago_labels() {
        let s = 1000;
        assert_eq!(ago(NOW, NOW), "just now");
        assert_eq!(ago(NOW - 59 * s, NOW), "just now");
        assert_eq!(ago(NOW - 60 * s, NOW), "1 min ago");
        assert_eq!(ago(NOW - 3599 * s, NOW), "59 min ago");
        assert_eq!(ago(NOW - 3600 * s, NOW), "1 h ago");
        assert_eq!(ago(NOW - 86399 * s, NOW), "23 h ago");
        assert_eq!(ago(NOW - 86400 * s, NOW), "yesterday");
        assert_eq!(ago(NOW - 3 * 86400 * s, NOW), "3 days ago");
        // A file from the future (clock change) isn't negative.
        assert_eq!(ago(NOW + 5000, NOW), "just now");
    }

    #[test]
    fn size_labels() {
        assert_eq!(size_label(0), "0 KB");
        assert_eq!(size_label(1), "1 KB");
        assert_eq!(size_label(1024), "1 KB");
        assert_eq!(size_label(1025), "2 KB");
        assert_eq!(size_label(1_048_575), "1024 KB");
        assert_eq!(size_label(1_048_576), "1.0 MB");
        assert_eq!(size_label(1_572_864), "1.5 MB");
    }
}
