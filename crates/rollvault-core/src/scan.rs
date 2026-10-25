//! Source (camera card) scanning. Any read error is fatal: a DIT tool must
//! never report success for a card it could not fully list.

use crate::hashing::{HashAlgo, Hasher};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

/// Suffix of in-flight files written by the engine. Never treated as data.
pub const PARTIAL_SUFFIX: &str = ".rvpart";

const JUNK_NAMES: &[&str] = &[
    ".DS_Store",
    ".Spotlight-V100",
    ".Trashes",
    ".fseventsd",
    ".TemporaryItems",
    ".DocumentRevisions-V100",
    ".VolumeIcon.icns",
    ".apdisk",
    ".com.apple.timemachine.donotpresent",
    "Thumbs.db",
    "desktop.ini",
    "System Volume Information",
    "$RECYCLE.BIN",
];

/// macOS / Windows housekeeping files that are never camera data.
pub fn is_os_junk_name(name: &str) -> bool {
    name.starts_with("._") || JUNK_NAMES.contains(&name) || name.ends_with(PARTIAL_SUFFIX)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceFile {
    /// Path relative to the source root, `/`-separated.
    pub rel: String,
    pub size: u64,
    pub mtime: SystemTime,
    pub atime: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceDir {
    pub rel: String,
    pub mtime: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceScan {
    pub root: PathBuf,
    pub files: Vec<SourceFile>,
    pub dirs: Vec<SourceDir>,
    pub total_bytes: u64,
    /// Stable identity of the card contents (paths, sizes, mtimes).
    pub fingerprint: String,
    pub skipped_junk: Vec<String>,
    pub warnings: Vec<String>,
}

impl SourceScan {
    pub fn largest_file(&self) -> u64 {
        self.files.iter().map(|f| f.size).max().unwrap_or(0)
    }
}

#[derive(Debug)]
pub enum ScanError {
    Missing(PathBuf),
    NotADirectory(PathBuf),
    Unreadable { path: PathBuf, message: String },
    NonUtf8(PathBuf),
    CaseCollision(String, String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ScanError::Missing(p) => l!("Source not found: {}", "Không tìm thấy nguồn: {}", p.display()),
            ScanError::NotADirectory(p) => l!("Source is not a folder: {}", "Nguồn không phải thư mục: {}", p.display()),
            ScanError::Unreadable { path, message } => {
                l!("Cannot read {}: {}", "Không đọc được {}: {}", path.display(), message)
            }
            ScanError::NonUtf8(p) => l!("File name is not UTF-8: {}", "Tên file không phải UTF-8: {}", p.display()),
            ScanError::CaseCollision(a, b) => l!(
                "Two paths differ only in upper/lower case and would overwrite each other on a macOS drive: '{}' and '{}'",
                "Hai đường dẫn chỉ khác nhau chữ hoa/thường, ổ đích macOS sẽ ghi đè lẫn nhau: '{}' và '{}'",
                a,
                b
            ),
        };
        f.write_str(&s)
    }
}

impl std::error::Error for ScanError {}

fn rel_string(root: &Path, path: &Path) -> Result<String, ScanError> {
    let rel = path.strip_prefix(root).map_err(|_| ScanError::NonUtf8(path.to_path_buf()))?;
    let mut parts = Vec::new();
    for c in rel.components() {
        let s = c
            .as_os_str()
            .to_str()
            .ok_or_else(|| ScanError::NonUtf8(path.to_path_buf()))?;
        parts.push(s.to_string());
    }
    Ok(parts.join("/"))
}

fn time_nanos(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

pub fn fingerprint(files: &[SourceFile]) -> String {
    let mut sorted: Vec<&SourceFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut h = Hasher::new(HashAlgo::Xxh128);
    for f in sorted {
        h.update(f.rel.as_bytes());
        h.update(&[0]);
        h.update(f.size.to_string().as_bytes());
        h.update(&[0]);
        h.update(time_nanos(f.mtime).to_string().as_bytes());
        h.update(b"\n");
    }
    h.finalize_hex()
}

pub fn scan_source(root: &Path) -> Result<SourceScan, ScanError> {
    let meta = std::fs::metadata(root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ScanError::Missing(root.to_path_buf())
        } else {
            ScanError::Unreadable { path: root.to_path_buf(), message: e.to_string() }
        }
    })?;
    if !meta.is_dir() {
        return Err(ScanError::NotADirectory(root.to_path_buf()));
    }

    let skipped: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let warnings: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut total = 0u64;

    let walker = WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if is_os_junk_name(&name) {
                skipped.borrow_mut().push(e.path().display().to_string());
                return false;
            }
            if e.depth() == 1 && e.file_type().is_dir() && name == "ascmhl" {
                warnings.borrow_mut().push(
                    l!(
                        "The source has an ascmhl folder at its root. It is not copied because RollVault creates a new ASC MHL history.",
                        "Nguồn có sẵn thư mục ascmhl ở gốc. Thư mục này không được chép vì RollVault tạo lịch sử ASC MHL mới."
                    ),
                );
                return false;
            }
            true
        });

    for entry in walker {
        let entry = entry.map_err(|e| ScanError::Unreadable {
            path: e.path().map(Path::to_path_buf).unwrap_or_else(|| root.to_path_buf()),
            message: e
                .io_error()
                .map(|io| io.to_string())
                .unwrap_or_else(|| e.to_string()),
        })?;
        if entry.depth() == 0 {
            continue;
        }
        let rel = rel_string(root, entry.path())?;
        let ft = entry.file_type();
        if ft.is_symlink() {
            warnings
                .borrow_mut()
                .push(l!("Skipped symlink: {}", "Bỏ qua symlink: {}", rel));
            continue;
        }
        let md = entry.metadata().map_err(|e| ScanError::Unreadable {
            path: entry.path().to_path_buf(),
            message: e.to_string(),
        })?;
        let mtime = md.modified().unwrap_or(UNIX_EPOCH);
        if ft.is_dir() {
            dirs.push(SourceDir { rel, mtime });
        } else if ft.is_file() {
            let atime = md.accessed().unwrap_or(mtime);
            total += md.len();
            files.push(SourceFile { rel, size: md.len(), mtime, atime });
        } else {
            warnings
                .borrow_mut()
                .push(l!("Skipped non-regular file: {}", "Bỏ qua mục không phải file thường: {}", rel));
        }
    }

    // Destination volumes on macOS are usually case-insensitive.
    let mut seen: HashMap<String, String> = HashMap::new();
    for rel in files.iter().map(|f| &f.rel).chain(dirs.iter().map(|d| &d.rel)) {
        let key = rel.to_lowercase();
        if let Some(prev) = seen.insert(key, rel.clone()) {
            return Err(ScanError::CaseCollision(prev, rel.clone()));
        }
    }

    let fp = fingerprint(&files);
    Ok(SourceScan {
        root: root.to_path_buf(),
        files,
        dirs,
        total_bytes: total,
        fingerprint: fp,
        skipped_junk: skipped.into_inner(),
        warnings: warnings.into_inner(),
    })
}

/// Best-effort guess of which camera wrote a card, from its folder layout.
pub fn detect_card_vendor(root: &Path) -> Option<&'static str> {
    let has = |p: &str| root.join(p).exists();
    if has("PRIVATE/M4ROOT") || has("M4ROOT") || has("XDROOT") || has("PRIVATE/XDROOT") {
        return Some("Sony");
    }
    if has("PRIVATE/AVCHD") {
        return Some("AVCHD");
    }
    if has("CONTENTS/CLIPS001") || has("DCIM/100CANON") || has("PRIVATE/CANON") {
        return Some("Canon");
    }
    if let Ok(rd) = std::fs::read_dir(root.join("DCIM")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_uppercase();
            if n.contains("MEDIA") || n.starts_with("DJI") {
                return Some("DJI");
            }
            if n.contains("GOPRO") {
                return Some("GoPro");
            }
            if n.contains("PANA") || n.contains("_PANA") {
                return Some("Panasonic");
            }
            if n.contains("NIKON") || n.contains("NZ") {
                return Some("Nikon");
            }
            if n.contains("FUJI") {
                return Some("Fujifilm");
            }
        }
        return Some("DCIM");
    }
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.ends_with(".braw") {
                return Some("Blackmagic");
            }
            if n.ends_with(".rdm") || n.ends_with(".rdc") {
                return Some("RED");
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn junk_filter() {
        assert!(is_os_junk_name("._C0001.MP4"));
        assert!(is_os_junk_name(".DS_Store"));
        assert!(is_os_junk_name(".Spotlight-V100"));
        assert!(is_os_junk_name(".C0001.MP4.rvpart"));
        assert!(!is_os_junk_name("C0001.MP4"));
        assert!(!is_os_junk_name(".hidden_camera_file"));
    }

    #[test]
    fn scan_skips_junk_keeps_empty_dirs_and_sorts() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("PRIVATE/M4ROOT/CLIP")).unwrap();
        fs::create_dir_all(r.join("PRIVATE/M4ROOT/SUB")).unwrap();
        fs::create_dir_all(r.join(".Spotlight-V100/Store")).unwrap();
        fs::write(r.join("PRIVATE/M4ROOT/CLIP/C0002.MP4"), b"bb").unwrap();
        fs::write(r.join("PRIVATE/M4ROOT/CLIP/C0001.MP4"), b"a").unwrap();
        fs::write(r.join("PRIVATE/M4ROOT/CLIP/._C0001.MP4"), b"junk").unwrap();
        fs::write(r.join(".DS_Store"), b"junk").unwrap();
        let s = scan_source(r).unwrap();
        let rels: Vec<_> = s.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec!["PRIVATE/M4ROOT/CLIP/C0001.MP4", "PRIVATE/M4ROOT/CLIP/C0002.MP4"]);
        assert!(s.dirs.iter().any(|d| d.rel == "PRIVATE/M4ROOT/SUB"));
        assert_eq!(s.total_bytes, 3);
        assert_eq!(s.skipped_junk.len(), 3);
        assert_eq!(detect_card_vendor(r), Some("Sony"));
    }

    #[test]
    fn unreadable_directory_is_fatal() {
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("CLIP");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("C0001.MP4"), b"x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let res = scan_source(d.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(res, Err(ScanError::Unreadable { .. })), "{res:?}");
    }

    #[test]
    fn fingerprint_changes_with_content_metadata() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.mp4"), b"1").unwrap();
        let a = scan_source(d.path()).unwrap().fingerprint;
        fs::write(d.path().join("b.mp4"), b"2").unwrap();
        let b = scan_source(d.path()).unwrap().fingerprint;
        assert_ne!(a, b);
    }
}
