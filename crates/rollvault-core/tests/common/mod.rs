#![allow(dead_code)]
//! Test helpers: synthetic camera cards and disk images (hdiutil).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

pub const MIB: usize = 1024 * 1024;

/// Deterministic pseudo-random bytes.
pub fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// A Sony-like card with edge-case sizes (empty, chunk boundary, chunk+1).
pub fn make_sony_card(root: &Path) -> Vec<(String, usize)> {
    let files: Vec<(&str, usize)> = vec![
        ("PRIVATE/M4ROOT/CLIP/C0001.MP4", 8 * MIB),
        ("PRIVATE/M4ROOT/CLIP/C0001M01.XML", 3_000),
        ("PRIVATE/M4ROOT/CLIP/C0002.MP4", 8 * MIB + 1),
        ("PRIVATE/M4ROOT/CLIP/C0002M01.XML", 3_100),
        ("PRIVATE/M4ROOT/CLIP/C0003.MP4", 21 * MIB + 12_345),
        ("PRIVATE/M4ROOT/THMBNL/C0001T01.JPG", 40_000),
        ("PRIVATE/M4ROOT/MEDIAPRO.XML", 2_000),
        ("PRIVATE/M4ROOT/STATUS.BIN", 0),
        ("DCIM/100MSDCF/DSC00001.JPG", 1),
    ];
    let mut out = Vec::new();
    for (i, (rel, size)) in files.iter().enumerate() {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes(i as u64 + 1, *size)).unwrap();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000 + i as u64 * 61);
        filetime::set_file_mtime(&p, filetime::FileTime::from_system_time(t)).unwrap();
        out.push((rel.to_string(), *size));
    }
    fs::create_dir_all(root.join("PRIVATE/M4ROOT/SUB")).unwrap();
    fs::write(root.join(".DS_Store"), b"junk").unwrap();
    fs::write(root.join("PRIVATE/M4ROOT/CLIP/._C0001.MP4"), b"junk").unwrap();
    out
}

pub fn files_equal(a: &Path, b: &Path) -> bool {
    fs::read(a).unwrap() == fs::read(b).unwrap()
}

pub fn find_partials(root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".rvpart"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// A small HFS+ disk image attached at a private mount point.
pub struct DiskImage {
    pub image: PathBuf,
    pub mount: PathBuf,
    attached: bool,
}

impl DiskImage {
    pub fn create(dir: &Path, name: &str, size_mb: u64) -> Option<DiskImage> {
        Self::create_fs(dir, name, size_mb, "HFS+")
    }

    pub fn create_fs(dir: &Path, name: &str, size_mb: u64, fs_name: &str) -> Option<DiskImage> {
        let image = dir.join(format!("{name}.dmg"));
        let mount = dir.join(format!("{name}-mnt"));
        fs::create_dir_all(&mount).ok()?;
        let ok = Command::new("hdiutil")
            .args(["create", "-quiet", "-size", &format!("{size_mb}m"), "-fs", fs_name, "-volname", name])
            .arg(&image)
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        let ok = Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-noautoopen", "-mountpoint"])
            .arg(&mount)
            .arg(&image)
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        Some(DiskImage { image, mount, attached: true })
    }

    /// Re-attach after a forced detach (simulates re-inserting the card).
    pub fn reattach(&mut self) -> bool {
        let ok = Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-noautoopen", "-mountpoint"])
            .arg(&self.mount)
            .arg(&self.image)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        self.attached = ok;
        ok
    }

    pub fn detach_force(&mut self) {
        if self.attached {
            let _ = Command::new("hdiutil").args(["detach", "-quiet", "-force"]).arg(&self.mount).status();
            self.attached = false;
        }
    }
}

impl Drop for DiskImage {
    fn drop(&mut self) {
        self.detach_force();
        let _ = fs::remove_file(&self.image);
    }
}
