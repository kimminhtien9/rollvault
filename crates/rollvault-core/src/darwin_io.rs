//! Darwin (macOS) file I/O primitives used by the copy engine.
//!
//! Notes on what these calls really guarantee:
//! * `F_NOCACHE` is a hint, not `O_DIRECT`. Page-aligned reads/writes bypass the
//!   unified buffer cache, but pages that are already resident can still be
//!   served from RAM. That is why [`purge_cached_pages`] is called before every
//!   read-back verification, and why the engine uses page-aligned buffers.
//! * `F_FULLFSYNC` asks the drive to flush its own write cache to stable media.
//!   Some file systems (SMB, some FAT drivers) do not support it; we then fall
//!   back to `fsync`.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::time::SystemTime;

/// Size of the virtual memory page (16 KiB on Apple Silicon, 4 KiB on Intel).
pub fn page_size() -> usize {
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: sysconf has no preconditions.
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 { p as usize } else { 16 * 1024 }
    })
}

fn round_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

/// A heap buffer aligned to the VM page size so that `F_NOCACHE` I/O can go
/// straight to the device instead of through the buffer cache.
pub struct AlignedBuf {
    ptr: NonNull<u8>,
    cap: usize,
    layout: Layout,
}

// SAFETY: the buffer is plain bytes owned exclusively by this value.
unsafe impl Send for AlignedBuf {}
// SAFETY: shared access only hands out `&[u8]`.
unsafe impl Sync for AlignedBuf {}

impl AlignedBuf {
    pub fn new(capacity: usize) -> Self {
        let align = page_size().max(4096);
        let cap = round_up(capacity.max(1), align);
        let layout = Layout::from_size_align(cap, align).expect("valid buffer layout");
        // SAFETY: layout has non-zero size.
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        Self { ptr, cap, layout }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr is valid for cap bytes for the lifetime of self.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.cap) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: ptr is valid for cap bytes and we hold &mut self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.cap) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: ptr was allocated with this layout.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

fn is_unsupported(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOTTY) | Some(libc::EBADF)
    )
}

/// Turn off buffer-cache use for this descriptor (best effort on file systems
/// that do not support it).
pub fn set_nocache(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: fcntl on a valid descriptor.
        let r = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
        if r == -1 {
            let err = io::Error::last_os_error();
            if !is_unsupported(&err) {
                return Err(err);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = file;
    Ok(())
}

/// Flush file data all the way to stable media. Falls back to `fsync` only
/// when the file system reports that `F_FULLFSYNC` is unsupported; real I/O
/// errors are returned.
pub fn full_fsync(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: fcntl on a valid descriptor.
        let r = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };
        if r != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if !is_unsupported(&err) {
            return Err(err);
        }
    }
    file.sync_all()
}

/// Persist a directory entry change (create/rename). Unsupported file systems
/// are tolerated because the data itself was already flushed.
pub fn fsync_dir(dir: &Path) -> io::Result<()> {
    let f = match File::open(dir) {
        Ok(f) => f,
        Err(e) if is_unsupported(&e) => return Ok(()),
        Err(e) => return Err(e),
    };
    match full_fsync(&f) {
        Ok(()) => Ok(()),
        Err(e) if is_unsupported(&e) || e.raw_os_error() == Some(libc::EACCES) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Read until `buf` is full or EOF. Returns the number of bytes read.
pub fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Ask the kernel to drop any cached pages of this file so the next read has
/// to come from the device. Best effort: errors are returned but callers treat
/// them as non-fatal.
pub fn purge_cached_pages(file: &File, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let len = usize::try_from(len).map_err(|_| io::Error::other("file too large to map"))?;
    // SAFETY: mapping a regular file read-only; unmapped before return.
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        );
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let r = libc::msync(p, len, libc::MS_INVALIDATE);
        let err = if r != 0 { Some(io::Error::last_os_error()) } else { None };
        libc::munmap(p, len);
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Count how many pages of the file are resident in RAM. Used by tests and
/// diagnostics to prove that verification reads are not served from cache.
pub fn resident_pages(file: &File, len: u64) -> io::Result<(usize, usize)> {
    if len == 0 {
        return Ok((0, 0));
    }
    let len = usize::try_from(len).map_err(|_| io::Error::other("file too large to map"))?;
    let page = page_size();
    let pages = len.div_ceil(page);
    let mut vec = vec![0 as libc::c_char; pages];
    // SAFETY: read-only mapping inspected with mincore, then unmapped.
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        );
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let r = libc::mincore(p, len, vec.as_mut_ptr());
        let err = if r != 0 { Some(io::Error::last_os_error()) } else { None };
        libc::munmap(p, len);
        if let Some(e) = err {
            return Err(e);
        }
    }
    let resident = vec.iter().filter(|v| (**v as i32) & 1 != 0).count();
    Ok((resident, pages))
}

/// Copy access and modification times onto `path`.
pub fn set_file_times(path: &Path, atime: SystemTime, mtime: SystemTime) -> io::Result<()> {
    filetime::set_file_times(
        path,
        filetime::FileTime::from_system_time(atime),
        filetime::FileTime::from_system_time(mtime),
    )
}

fn cstr(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

/// Rename without ever replacing an existing file. Uses `renamex_np` with
/// `RENAME_EXCL` (atomic) and falls back to check-then-rename on file systems
/// that do not support it.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let f = cstr(from)?;
        let t = cstr(to)?;
        // SAFETY: valid NUL-terminated paths.
        let r = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
        if r == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EEXIST) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("destination already exists: {}", to.display()),
            ));
        }
        if !is_unsupported(&err) {
            return Err(err);
        }
    }
    if to.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", to.display()),
        ));
    }
    std::fs::rename(from, to)
}

/// Statistics for the file system holding `path`.
#[derive(Debug, Clone)]
pub struct FsStat {
    pub mount_point: String,
    pub device: String,
    pub fs_type: String,
    pub block_size: u64,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub flags: u32,
}

pub(crate) fn c_chars_to_string(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub(crate) fn fsstat_from_raw(s: &libc::statfs) -> FsStat {
    let bsize = s.f_bsize as u64;
    FsStat {
        mount_point: c_chars_to_string(&s.f_mntonname),
        device: c_chars_to_string(&s.f_mntfromname),
        fs_type: c_chars_to_string(&s.f_fstypename),
        block_size: bsize.max(512),
        total_bytes: s.f_blocks.saturating_mul(bsize),
        free_bytes: s.f_bavail.saturating_mul(bsize),
        flags: s.f_flags,
    }
}

pub fn statfs(path: &Path) -> io::Result<FsStat> {
    let c = cstr(path)?;
    // SAFETY: zeroed statfs is a valid out-parameter.
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid path and out pointer.
    let r = unsafe { libc::statfs(c.as_ptr(), &mut s) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fsstat_from_raw(&s))
}

/// All mounted file systems (no blocking on network mounts).
pub fn mounted_filesystems() -> Vec<FsStat> {
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo fills a buffer owned by libc.
    let n = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if n <= 0 || buf.is_null() {
        return Vec::new();
    }
    // SAFETY: libc guarantees n entries.
    let entries = unsafe { std::slice::from_raw_parts(buf, n as usize) };
    entries.iter().map(fsstat_from_raw).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn aligned_buffer_is_page_aligned() {
        let mut b = AlignedBuf::new(8 * 1024 * 1024 + 3);
        assert_eq!(b.as_slice().as_ptr() as usize % page_size(), 0);
        assert_eq!(b.capacity() % page_size(), 0);
        b.as_mut_slice()[0] = 7;
        assert_eq!(b.as_slice()[0], 7);
    }

    #[test]
    fn rename_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let err = rename_no_replace(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        let c = dir.path().join("c");
        rename_no_replace(&a, &c).unwrap();
        assert_eq!(std::fs::read(&c).unwrap(), b"a");
    }

    /// Proves the cache purge works: pages written through the cache are
    /// resident, and after `purge_cached_pages` they are gone.
    #[test]
    fn purge_drops_resident_pages() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cached.bin");
        let len = 8 * 1024 * 1024u64;
        {
            let mut f = File::create(&p).unwrap();
            f.write_all(&vec![0xABu8; len as usize]).unwrap();
            f.sync_all().unwrap();
        }
        let f = File::open(&p).unwrap();
        let (before, pages) = resident_pages(&f, len).unwrap();
        assert!(before > pages / 2, "expected cached pages after buffered write, got {before}/{pages}");
        purge_cached_pages(&f, len).unwrap();
        let (after, _) = resident_pages(&f, len).unwrap();
        assert_eq!(after, 0, "pages still resident after purge: {after}/{pages}");
    }

    #[test]
    fn statfs_works_on_tmp() {
        let s = statfs(Path::new("/tmp")).unwrap();
        assert!(s.total_bytes > 0);
        assert!(!mounted_filesystems().is_empty());
    }
}
