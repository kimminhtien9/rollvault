//! Single-read, multi-write copy pipeline with cold read-back verification.
//!
//! ```text
//!             ┌─> hasher thread ─────────────── source hash board
//! reader ─────┼─> writer[dest 0] ─> verifier[dest 0]
//! (thread)    └─> writer[dest n] ─> verifier[dest n]
//!                         ▲
//!                 supervisor (caller's thread)
//! ```
//!
//! * Each chunk is read from the source exactly once into a page-aligned
//!   buffer and shared (`Arc`) between the hasher and every writer.
//! * Writers write to a hidden `.<name>.rvpart` file, `F_FULLFSYNC` it and
//!   hand it to their verifier.
//! * Verifiers purge the file from the page cache, re-read it with
//!   `F_NOCACHE`, compare against the reference hash and only then rename it
//!   to its real name (never replacing an existing file). A file with its
//!   real name on a destination is therefore always a verified file.
//! * A destination that fails (unplugged, full, I/O error) is dropped and the
//!   others carry on.
//! * Every thread marks when it enters a blocking system call. The
//!   supervisor abandons a destination (or the source) whose call has been
//!   stuck longer than `stall_timeout`, so a yanked drive or a hung NAS can
//!   never freeze the job.
//! * Files that fail verification are retried once; if the source returns a
//!   different hash on the second read, the file is reported as unstable.

use crate::darwin_io::{self, AlignedBuf};
use crate::hashing::{HashAlgo, Hasher};
use crate::scan::{PARTIAL_SUFFIX, SourceFile};
use crate::util::Busy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub const DEFAULT_CHUNK: usize = 8 * 1024 * 1024;
/// A single system call on a drive taking longer than this means the drive
/// is gone or hung.
pub const DEFAULT_STALL: Duration = Duration::from_secs(120);
const POOL_BUFFERS: usize = 6;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestPhase {
    Queued = 0,
    WaitingDevice = 1,
    Copying = 2,
    Verifying = 3,
    Done = 4,
    Failed = 5,
    Cancelled = 6,
}

impl DestPhase {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => DestPhase::WaitingDevice,
            2 => DestPhase::Copying,
            3 => DestPhase::Verifying,
            4 => DestPhase::Done,
            5 => DestPhase::Failed,
            6 => DestPhase::Cancelled,
            _ => DestPhase::Queued,
        }
    }
}

#[derive(Default)]
pub struct DestProgress {
    pub bytes_written: AtomicU64,
    pub bytes_verified: AtomicU64,
    pub files_verified: AtomicU64,
    pub files_failed: AtomicU64,
    phase: AtomicU8,
    pub current: Mutex<String>,
}

impl DestProgress {
    pub fn phase(&self) -> DestPhase {
        DestPhase::from_u8(self.phase.load(Ordering::Relaxed))
    }
    pub fn set_phase(&self, p: DestPhase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }
    fn set_current(&self, s: &str) {
        if let Ok(mut c) = self.current.lock() {
            c.clear();
            c.push_str(s);
        }
    }
}

#[derive(Default)]
pub struct SourceProgress {
    pub bytes_read: AtomicU64,
    pub current: Mutex<String>,
}

#[derive(Clone)]
pub struct DestTarget {
    pub roll_dir: PathBuf,
    /// Rotational / network destinations verify after the copy pass so the
    /// read-back does not fight the writes for the heads.
    pub defer_verify: bool,
    pub progress: Arc<DestProgress>,
}

/// Callbacks used by the job layer to journal progress for crash recovery.
pub trait TransferListener: Send + Sync {
    fn source_hashed(&self, _file: usize, _hash: &str) {}
    fn file_verified(&self, _dest: usize, _file: usize, _hash: &str) {}
    fn file_failed(&self, _dest: usize, _file: usize, _reason: &str) {}
    fn dest_fatal(&self, _dest: usize, _reason: &str) {}
    fn source_error(&self, _file: usize, _reason: &str) {}
}

pub struct NoopListener;
impl TransferListener for NoopListener {}

/// Test-only fault injection. Production passes `None`.
#[derive(Default)]
pub struct FaultInjector {
    /// (dest, rel): flip one byte of the written file, first time only.
    pub corrupt_dest_once: Mutex<HashSet<(usize, String)>>,
    /// (dest, rel): flip one byte every time the file is written.
    pub corrupt_dest_always: Mutex<HashSet<(usize, String)>>,
    /// rel: flip one byte of the data read from the source, first time only
    /// (simulates a flaky card reader).
    pub corrupt_source_once: Mutex<HashSet<String>>,
    /// (dest, rel): opening the destination file fails.
    pub fail_dest_open: Mutex<HashSet<(usize, String)>>,
    /// (dest, rel): writing this file blocks "forever" (a hung drive).
    pub hang_dest_write: Mutex<HashSet<(usize, String)>>,
    /// rel: reading this file from the source blocks "forever".
    pub hang_source_read: Mutex<HashSet<String>>,
    /// Sleep after every chunk (to make cancel/unplug tests deterministic).
    pub chunk_delay: Mutex<Option<Duration>>,
}

impl FaultInjector {
    fn hit(set: &Mutex<HashSet<(usize, String)>>, d: usize, rel: &str) -> bool {
        set.lock().map(|s| s.contains(&(d, rel.to_string()))).unwrap_or(false)
    }
}

/// Blocks like a dead device would (bounded so test processes still exit).
fn simulate_hang() {
    std::thread::sleep(Duration::from_secs(600));
}

#[derive(Clone)]
pub struct TransferRequest {
    pub source_root: PathBuf,
    pub files: Arc<Vec<SourceFile>>,
    /// For each file index, the destination indices that still need it.
    pub work: Vec<Vec<usize>>,
    pub dests: Vec<DestTarget>,
    pub algo: HashAlgo,
    /// Original hashes every copy must match (card hashes for a cascade, or
    /// journaled hashes when resuming). `None` entries mean "use the hash
    /// computed while reading".
    pub expected: Option<Arc<Vec<Option<String>>>>,
    pub cancel: Arc<AtomicBool>,
    pub source_progress: Arc<SourceProgress>,
    pub listener: Arc<dyn TransferListener>,
    pub faults: Option<Arc<FaultInjector>>,
    pub chunk_size: usize,
    pub retry_failed: bool,
    pub stall_timeout: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct DestOutcome {
    /// Per file: the verified hash, if verified during this transfer.
    pub verified: Vec<Option<String>>,
    pub failures: BTreeMap<usize, String>,
    pub fatal: Option<String>,
}

#[derive(Debug, Default)]
pub struct TransferOutcome {
    pub source_hashes: Vec<Option<String>>,
    pub source_errors: BTreeMap<usize, String>,
    /// Files whose source returned different data on two reads.
    pub source_unstable: Vec<usize>,
    pub source_lost: bool,
    pub dests: Vec<DestOutcome>,
    pub cancelled: bool,
}

/// Human-readable description of an I/O error.
pub fn describe_io(e: &io::Error) -> String {
    let hint = match e.raw_os_error() {
        Some(libc::ENOSPC) => l!("disk full", "ổ đầy"),
        Some(libc::EDQUOT) => l!("quota exceeded", "vượt hạn mức dung lượng"),
        Some(libc::EIO) => l!("hardware I/O error", "lỗi I/O phần cứng"),
        Some(libc::ENXIO) | Some(libc::ENODEV) => l!("device disconnected", "thiết bị đã bị rút"),
        Some(libc::ENOENT) => l!("not found (drive removed?)", "không tìm thấy (ổ bị rút?)"),
        Some(libc::EACCES) | Some(libc::EPERM) => l!("permission denied", "không có quyền ghi/đọc"),
        Some(libc::EROFS) => l!("read-only volume", "ổ chỉ đọc"),
        Some(libc::EFBIG) => l!("file too large for this file system (FAT32 limit is 4 GB)", "file quá lớn cho hệ thống file này (FAT32 giới hạn 4 GB)"),
        Some(libc::ENAMETOOLONG) => l!("name too long", "tên quá dài"),
        Some(libc::ETIMEDOUT) => l!("timed out (network drive?)", "hết thời gian chờ (ổ mạng?)"),
        _ => String::new(),
    };
    if hint.is_empty() { e.to_string() } else { format!("{hint} ({e})") }
}

// ---------------------------------------------------------------------------
// Buffer pool
// ---------------------------------------------------------------------------

struct BufPool {
    free: Mutex<Vec<AlignedBuf>>,
    cv: Condvar,
    size: usize,
}

impl BufPool {
    fn new(count: usize, size: usize) -> Arc<Self> {
        let bufs = (0..count).map(|_| AlignedBuf::new(size)).collect();
        Arc::new(Self { free: Mutex::new(bufs), cv: Condvar::new(), size })
    }

    fn take(&self, cancel: &AtomicBool) -> Option<AlignedBuf> {
        let mut free = self.free.lock().ok()?;
        loop {
            if let Some(b) = free.pop() {
                return Some(b);
            }
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            free = self.cv.wait_timeout(free, Duration::from_millis(100)).ok()?.0;
        }
    }

    fn give(&self, b: AlignedBuf) {
        if let Ok(mut free) = self.free.lock() {
            free.push(b);
        }
        self.cv.notify_one();
    }

    /// Replace a buffer that an abandoned (stuck) thread will never return.
    fn grow(&self) {
        self.give(AlignedBuf::new(self.size));
    }
}

struct Chunk {
    buf: Option<AlignedBuf>,
    len: usize,
    pool: Arc<BufPool>,
}

impl Chunk {
    fn bytes(&self) -> &[u8] {
        &self.buf.as_ref().expect("chunk buffer").as_slice()[..self.len]
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        if let Some(b) = self.buf.take() {
            self.pool.give(b);
        }
    }
}

// ---------------------------------------------------------------------------
// Work queue the supervisor can drain (unlike an mpsc channel).
// ---------------------------------------------------------------------------

struct WorkQueue<T> {
    st: Mutex<(VecDeque<T>, bool)>,
    cv: Condvar,
}

impl<T> WorkQueue<T> {
    fn new() -> Self {
        Self { st: Mutex::new((VecDeque::new(), false)), cv: Condvar::new() }
    }

    fn push(&self, t: T) -> bool {
        let Ok(mut st) = self.st.lock() else { return false };
        if st.1 {
            return false;
        }
        st.0.push_back(t);
        drop(st);
        self.cv.notify_one();
        true
    }

    fn pop(&self) -> Option<T> {
        let mut st = self.st.lock().ok()?;
        loop {
            if let Some(t) = st.0.pop_front() {
                return Some(t);
            }
            if st.1 {
                return None;
            }
            st = self.cv.wait(st).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut st) = self.st.lock() {
            st.1 = true;
        }
        self.cv.notify_all();
    }

    /// Close and drop everything queued (returns chunk buffers to the pool).
    fn close_and_drain(&self) {
        let drained: Vec<T> = match self.st.lock() {
            Ok(mut st) => {
                st.1 = true;
                st.0.drain(..).collect()
            }
            Err(_) => Vec::new(),
        };
        drop(drained);
        self.cv.notify_all();
    }
}

// ---------------------------------------------------------------------------
// Source hash board: verifiers wait here for the hash of the file they check.
// ---------------------------------------------------------------------------

enum Slot {
    Pending,
    Ready(String),
    Failed,
}

struct HashBoard {
    state: Mutex<(Vec<Slot>, bool)>,
    cv: Condvar,
}

impl HashBoard {
    fn new(n: usize) -> Self {
        Self { state: Mutex::new(((0..n).map(|_| Slot::Pending).collect(), false)), cv: Condvar::new() }
    }

    fn set(&self, fi: usize, value: Option<String>) {
        if let Ok(mut st) = self.state.lock() {
            st.0[fi] = match value {
                Some(h) => Slot::Ready(h),
                None => Slot::Failed,
            };
        }
        self.cv.notify_all();
    }

    fn close(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.1 = true;
        }
        self.cv.notify_all();
    }

    fn wait(&self, fi: usize, cancel: &AtomicBool) -> Option<String> {
        let mut st = self.state.lock().ok()?;
        loop {
            match &st.0[fi] {
                Slot::Ready(h) => return Some(h.clone()),
                Slot::Failed => return None,
                Slot::Pending => {
                    if st.1 || cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    st = self.cv.wait_timeout(st, Duration::from_millis(200)).ok()?.0;
                }
            }
        }
    }

    fn snapshot(&self) -> Vec<Option<String>> {
        match self.state.lock() {
            Ok(st) => st
                .0
                .iter()
                .map(|s| match s {
                    Slot::Ready(h) => Some(h.clone()),
                    _ => None,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Messages and per-pass shared state
// ---------------------------------------------------------------------------

enum HMsg {
    Start,
    Data(Arc<Chunk>),
    End(usize),
    Abort(usize),
}

enum WMsg {
    Start(usize),
    Data(Arc<Chunk>),
    End(usize),
    Abort,
}

struct VMsg {
    fi: usize,
    temp: PathBuf,
    fin: PathBuf,
}

#[derive(Default)]
struct VerifyResult {
    verified: Vec<(usize, String)>,
    failures: Vec<(usize, String)>,
    fatal: Option<String>,
}

#[derive(Default)]
struct ReaderResult {
    errors: BTreeMap<usize, String>,
    cancelled: bool,
    source_lost: bool,
}

enum Done {
    Reader(ReaderResult),
    Hasher,
    Writer(usize, Option<String>),
    Verifier(usize, VerifyResult),
}

/// Everything the pass threads share. Owned (`Arc`) so a thread stuck in
/// the kernel can be abandoned without dangling borrows.
struct Ctx {
    req: TransferRequest,
    work: Vec<Vec<usize>>,
    prior: Option<Arc<Vec<Option<String>>>>,
    alive: Arc<Vec<AtomicBool>>,
    devs: Vec<Option<u64>>,
    board: HashBoard,
    pool: Arc<BufPool>,
    queues: Vec<WorkQueue<WMsg>>,
    reader_busy: Busy,
    writer_busy: Vec<Busy>,
    verifier_busy: Vec<Busy>,
    done: Mutex<Sender<Done>>,
}

impl Ctx {
    fn report(&self, d: Done) {
        if let Ok(tx) = self.done.lock() {
            let _ = tx.send(d);
        }
    }
}

/// Final and in-flight paths of a file on a destination.
pub fn dest_paths(roll_dir: &Path, rel: &str, fi: usize) -> (PathBuf, PathBuf) {
    let fin = roll_dir.join(rel);
    let name = fin.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp_name = if name.len() + PARTIAL_SUFFIX.len() < 250 {
        format!(".{name}{PARTIAL_SUFFIX}")
    } else {
        format!(".rv{fi}{PARTIAL_SUFFIX}")
    };
    (fin.with_file_name(temp_name), fin)
}

/// Hash a file straight from the device: purge cached pages, then read with
/// `F_NOCACHE` through an aligned buffer.
pub fn cold_hash_file(
    path: &Path,
    algo: HashAlgo,
    buf: &mut AlignedBuf,
    counter: Option<&AtomicU64>,
    cancel: &AtomicBool,
) -> io::Result<(String, u64)> {
    cold_hash_file_tracked(path, algo, buf, counter, cancel, None)
}

pub(crate) fn cold_hash_file_tracked(
    path: &Path,
    algo: HashAlgo,
    buf: &mut AlignedBuf,
    counter: Option<&AtomicU64>,
    cancel: &AtomicBool,
    busy: Option<&Busy>,
) -> io::Result<(String, u64)> {
    let enter = || busy.map(|b| b.enter());
    let mut f = {
        let _g = enter();
        let f = File::open(path)?;
        darwin_io::set_nocache(&f)?;
        let len = f.metadata()?.len();
        let _ = darwin_io::purge_cached_pages(&f, len);
        f
    };
    let mut h = Hasher::new(algo);
    let mut total = 0u64;
    let cap = buf.capacity();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, l!("cancelled", "đã huỷ")));
        }
        let n = {
            let _g = enter();
            darwin_io::read_full(&mut f, buf.as_mut_slice())?
        };
        if n == 0 {
            break;
        }
        h.update(&buf.as_slice()[..n]);
        total += n as u64;
        if let Some(c) = counter {
            c.fetch_add(n as u64, Ordering::Relaxed);
        }
        if n < cap {
            break;
        }
    }
    let _g = enter();
    drop(f);
    Ok((h.finalize_hex(), total))
}

fn flip_byte(path: &Path) -> io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    if len == 0 {
        return f.write_all(b"X");
    }
    let pos = len / 2;
    let mut b = [0u8; 1];
    f.seek(SeekFrom::Start(pos))?;
    f.read_exact(&mut b)?;
    b[0] ^= 0xFF;
    f.seek(SeekFrom::Start(pos))?;
    f.write_all(&b)?;
    f.sync_all()
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

fn hasher_loop(ctx: &Ctx, rx: Receiver<HMsg>) {
    let mut cur: Option<Hasher> = None;
    for msg in rx {
        match msg {
            HMsg::Start => cur = Some(Hasher::new(ctx.req.algo)),
            HMsg::Data(c) => {
                if let Some(h) = cur.as_mut() {
                    h.update(c.bytes());
                }
            }
            HMsg::End(fi) => {
                if let Some(h) = cur.take() {
                    let hex = h.finalize_hex();
                    ctx.req.listener.source_hashed(fi, &hex);
                    ctx.board.set(fi, Some(hex));
                }
            }
            HMsg::Abort(fi) => {
                cur = None;
                ctx.board.set(fi, None);
            }
        }
    }
    ctx.board.close();
}

fn open_dest(ctx: &Ctx, temp: &Path, d: usize, rel: &str) -> io::Result<File> {
    use std::os::unix::fs::MetadataExt;
    if let Some(f) = &ctx.req.faults
        && FaultInjector::hit(&f.fail_dest_open, d, rel)
    {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    // Directories were created up front. Never create them here: if the
    // drive was unplugged, re-creating the path would silently write the
    // footage onto whatever disk now owns that path.
    let parent = temp.parent().ok_or_else(|| io::Error::other("invalid path"))?;
    let md = fs::metadata(parent)?;
    if let Some(dev) = ctx.devs[d]
        && md.dev() != dev
    {
        return Err(io::Error::from_raw_os_error(libc::ENXIO));
    }
    if temp.symlink_metadata().is_ok() {
        fs::remove_file(temp)?;
    }
    let file = OpenOptions::new().write(true).create_new(true).open(temp)?;
    darwin_io::set_nocache(&file)?;
    Ok(file)
}

fn writer_loop(ctx: &Ctx, d: usize, vtx: Sender<VMsg>) -> Option<String> {
    let dest = &ctx.req.dests[d];
    let busy = &ctx.writer_busy[d];
    let alive = &ctx.alive[d];
    let mut fatal: Option<String> = None;
    let mut cur: Option<(usize, File, PathBuf, PathBuf)> = None;

    let die = |fatal: &mut Option<String>, msg: String| {
        alive.store(false, Ordering::Relaxed);
        ctx.req.listener.dest_fatal(d, &msg);
        *fatal = Some(msg);
    };

    while let Some(msg) = ctx.queues[d].pop() {
        match msg {
            WMsg::Start(fi) => {
                if fatal.is_some() {
                    continue;
                }
                let rel = &ctx.req.files[fi].rel;
                let (temp, fin) = dest_paths(&dest.roll_dir, rel, fi);
                dest.progress.set_phase(DestPhase::Copying);
                dest.progress.set_current(rel);
                let opened = {
                    let _g = busy.enter();
                    open_dest(ctx, &temp, d, rel)
                };
                match opened {
                    Ok(f) => cur = Some((fi, f, temp, fin)),
                    Err(e) => die(&mut fatal, l!("Cannot create {}: {}", "Không tạo được file {}: {}", rel, describe_io(&e))),
                }
            }
            WMsg::Data(chunk) => {
                if let Some((fi, f, temp, _)) = cur.as_mut() {
                    if let Some(faults) = &ctx.req.faults
                        && FaultInjector::hit(&faults.hang_dest_write, d, &ctx.req.files[*fi].rel)
                    {
                        let _g = busy.enter();
                        simulate_hang();
                    }
                    let res = {
                        let _g = busy.enter();
                        f.write_all(chunk.bytes())
                    };
                    match res {
                        Ok(()) => {
                            dest.progress.bytes_written.fetch_add(chunk.len as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let rel = ctx.req.files[*fi].rel.clone();
                            let temp = temp.clone();
                            cur = None;
                            {
                                let _g = busy.enter();
                                let _ = fs::remove_file(&temp);
                            }
                            die(&mut fatal, l!("Write error on {}: {}", "Lỗi ghi {}: {}", rel, describe_io(&e)));
                        }
                    }
                }
            }
            WMsg::End(fi) => {
                let Some((cfi, f, temp, fin)) = cur.take() else { continue };
                debug_assert_eq!(cfi, fi);
                let src = &ctx.req.files[fi];
                let result = {
                    let _g = busy.enter();
                    let flushed = darwin_io::full_fsync(&f);
                    drop(f);
                    flushed.and_then(|_| darwin_io::set_file_times(&temp, src.atime, src.mtime))
                };
                if let Err(e) = result {
                    {
                        let _g = busy.enter();
                        let _ = fs::remove_file(&temp);
                    }
                    die(&mut fatal, l!("Flush failed on {}: {}", "Lỗi flush {}: {}", src.rel, describe_io(&e)));
                    continue;
                }
                if let Some(faults) = &ctx.req.faults {
                    let key = (d, src.rel.clone());
                    let once = faults.corrupt_dest_once.lock().map(|mut s| s.remove(&key)).unwrap_or(false);
                    let always = faults.corrupt_dest_always.lock().map(|s| s.contains(&key)).unwrap_or(false);
                    if once || always {
                        let _ = flip_byte(&temp);
                    }
                }
                if vtx.send(VMsg { fi, temp: temp.clone(), fin }).is_err() {
                    let _ = fs::remove_file(&temp);
                    die(&mut fatal, l!("Verifier stopped unexpectedly", "Luồng verify dừng bất thường"));
                }
            }
            WMsg::Abort => {
                if let Some((_, f, temp, _)) = cur.take() {
                    let _g = busy.enter();
                    drop(f);
                    let _ = fs::remove_file(&temp);
                }
            }
        }
    }
    if let Some((_, f, temp, _)) = cur.take() {
        let _g = busy.enter();
        drop(f);
        let _ = fs::remove_file(&temp);
    }
    fatal
}

fn verify_loop(ctx: &Ctx, d: usize, rx: Receiver<VMsg>) -> VerifyResult {
    let dest = &ctx.req.dests[d];
    let mut res = VerifyResult::default();
    let mut buf = AlignedBuf::new(ctx.req.chunk_size);
    let mut deferred: Vec<VMsg> = Vec::new();
    for msg in rx {
        if dest.defer_verify {
            deferred.push(msg);
        } else {
            verify_one(ctx, d, msg, &mut buf, &mut res);
        }
    }
    for msg in deferred {
        verify_one(ctx, d, msg, &mut buf, &mut res);
    }
    res
}

fn verify_one(ctx: &Ctx, d: usize, msg: VMsg, buf: &mut AlignedBuf, res: &mut VerifyResult) {
    let req = &ctx.req;
    let dest = &req.dests[d];
    let busy = &ctx.verifier_busy[d];
    let alive = &ctx.alive[d];
    let VMsg { fi, temp, fin } = msg;
    let remove = |p: &Path| {
        let _g = busy.enter();
        let _ = fs::remove_file(p);
    };
    if !alive.load(Ordering::Relaxed) || req.cancel.load(Ordering::Relaxed) {
        remove(&temp);
        return;
    }
    let src = &req.files[fi];
    dest.progress.set_phase(DestPhase::Verifying);
    dest.progress.set_current(&src.rel);

    let fail = |res: &mut VerifyResult, reason: String| {
        remove(&temp);
        dest.progress.files_failed.fetch_add(1, Ordering::Relaxed);
        req.listener.file_failed(d, fi, &reason);
        res.failures.push((fi, reason));
    };

    let hashed = cold_hash_file_tracked(&temp, req.algo, buf, Some(&dest.progress.bytes_verified), &req.cancel, Some(busy));
    let dest_hash = match hashed {
        Ok((h, size)) if size == src.size => h,
        Ok((_, size)) => {
            fail(res, l!("Wrong size reading back {}: {} instead of {} bytes", "Sai kích thước khi đọc lại {}: {} thay vì {} byte", src.rel, size, src.size));
            return;
        }
        Err(e) => {
            remove(&temp);
            if req.cancel.load(Ordering::Relaxed) {
                return;
            }
            let msg = l!("Cannot read back {} to verify: {}", "Không đọc lại được {} để verify: {}", src.rel, describe_io(&e));
            alive.store(false, Ordering::Relaxed);
            req.listener.dest_fatal(d, &msg);
            res.fatal = Some(msg);
            return;
        }
    };

    let Some(current) = ctx.board.wait(fi, &req.cancel) else {
        remove(&temp);
        return;
    };
    let reference = req
        .expected
        .as_ref()
        .and_then(|e| e.get(fi).cloned().flatten())
        .or_else(|| ctx.prior.as_ref().and_then(|p| p.get(fi).cloned().flatten()))
        .unwrap_or_else(|| current.clone());

    if current != reference {
        fail(
            res,
            l!(
                "Source read gave a different hash than the original for {}: {} ≠ {} (unstable source or damaged source copy)",
                "Nguồn đọc ra hash khác hash gốc ở {}: {} ≠ {} (nguồn không ổn định hoặc bản nguồn bị hỏng)",
                src.rel,
                current,
                reference
            ),
        );
        return;
    }
    if dest_hash != reference {
        fail(res, l!("HASH MISMATCH on {}: copy {} ≠ original {}", "SAI HASH ở {}: đích {} ≠ gốc {}", src.rel, dest_hash, reference));
        return;
    }
    let renamed = {
        let _g = busy.enter();
        let r = darwin_io::rename_no_replace(&temp, &fin);
        if r.is_ok()
            && let Some(parent) = fin.parent()
        {
            let _ = darwin_io::fsync_dir(parent);
        }
        r
    };
    if let Err(e) = renamed {
        fail(res, l!("Cannot rename {}: {}", "Không đổi tên được {}: {}", src.rel, describe_io(&e)));
        return;
    }
    dest.progress.files_verified.fetch_add(1, Ordering::Relaxed);
    req.listener.file_verified(d, fi, &dest_hash);
    res.verified.push((fi, dest_hash));
}

fn send(ctx: &Ctx, d: usize, msg: WMsg) {
    if !ctx.queues[d].push(msg) {
        ctx.alive[d].store(false, Ordering::Relaxed);
    }
}

fn reader_loop(ctx: &Ctx, htx: Sender<HMsg>) -> ReaderResult {
    let req = &ctx.req;
    let mut out = ReaderResult::default();
    let delay = req.faults.as_ref().and_then(|f| f.chunk_delay.lock().ok().and_then(|d| *d));
    let busy = &ctx.reader_busy;
    for (fi, wanted) in ctx.work.iter().enumerate() {
        if wanted.is_empty() {
            continue;
        }
        if req.cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        let targets: Vec<usize> = wanted.iter().copied().filter(|d| ctx.alive[*d].load(Ordering::Relaxed)).collect();
        if targets.is_empty() {
            continue;
        }
        let src = &req.files[fi];
        if let Ok(mut c) = req.source_progress.current.lock() {
            c.clone_from(&src.rel);
        }
        let path = req.source_root.join(&src.rel);
        let opened = {
            let _g = busy.enter();
            File::open(&path).and_then(|f| darwin_io::set_nocache(&f).map(|_| f))
        };
        let mut file = match opened {
            Ok(f) => f,
            Err(e) => {
                let msg = l!("Cannot open source {}: {}", "Không mở được nguồn {}: {}", src.rel, describe_io(&e));
                req.listener.source_error(fi, &msg);
                out.errors.insert(fi, msg);
                ctx.board.set(fi, None);
                if !req.source_root.exists() {
                    out.source_lost = true;
                    break;
                }
                continue;
            }
        };
        let corrupt_once = req
            .faults
            .as_ref()
            .map(|f| f.corrupt_source_once.lock().map(|mut s| s.remove(&src.rel)).unwrap_or(false))
            .unwrap_or(false);
        let hang = req
            .faults
            .as_ref()
            .map(|f| f.hang_source_read.lock().map(|s| s.contains(&src.rel)).unwrap_or(false))
            .unwrap_or(false);

        let _ = htx.send(HMsg::Start);
        for &d in &targets {
            send(ctx, d, WMsg::Start(fi));
        }
        let mut total = 0u64;
        let mut failed: Option<String> = None;
        loop {
            if req.cancel.load(Ordering::Relaxed) {
                out.cancelled = true;
                failed = Some(l!("cancelled", "đã huỷ"));
                break;
            }
            let Some(mut buf) = ctx.pool.take(&req.cancel) else {
                out.cancelled = true;
                failed = Some(l!("cancelled", "đã huỷ"));
                break;
            };
            let cap = buf.capacity();
            let n = {
                let _g = busy.enter();
                if hang {
                    simulate_hang();
                }
                darwin_io::read_full(&mut file, buf.as_mut_slice())
            };
            let n = match n {
                Ok(n) => n,
                Err(e) => {
                    ctx.pool.give(buf);
                    failed = Some(l!("Read error on source {}: {}", "Lỗi đọc nguồn {}: {}", src.rel, describe_io(&e)));
                    break;
                }
            };
            if n == 0 {
                ctx.pool.give(buf);
                break;
            }
            if corrupt_once && total == 0 {
                buf.as_mut_slice()[n / 2] ^= 0xFF;
            }
            total += n as u64;
            let chunk = Arc::new(Chunk { buf: Some(buf), len: n, pool: ctx.pool.clone() });
            let _ = htx.send(HMsg::Data(chunk.clone()));
            for &d in &targets {
                if ctx.alive[d].load(Ordering::Relaxed) {
                    send(ctx, d, WMsg::Data(chunk.clone()));
                }
            }
            drop(chunk);
            req.source_progress.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
            if let Some(d) = delay {
                std::thread::sleep(d);
            }
            if n < cap {
                break;
            }
        }
        if failed.is_none() && total != src.size {
            failed = Some(l!(
                "Source size changed for {}: read {} bytes, scanned {} bytes",
                "Kích thước nguồn thay đổi ở {}: đọc {} byte, lúc quét {} byte",
                src.rel,
                total,
                src.size
            ));
        }
        match failed {
            None => {
                let _ = htx.send(HMsg::End(fi));
                for &d in &targets {
                    send(ctx, d, WMsg::End(fi));
                }
            }
            Some(msg) => {
                let _ = htx.send(HMsg::Abort(fi));
                for &d in &targets {
                    send(ctx, d, WMsg::Abort);
                }
                if out.cancelled {
                    break;
                }
                req.listener.source_error(fi, &msg);
                out.errors.insert(fi, msg);
                if !req.source_root.exists() {
                    out.source_lost = true;
                    break;
                }
            }
        }
    }
    out
}

struct PassResult {
    hashes: Vec<Option<String>>,
    source_errors: BTreeMap<usize, String>,
    cancelled: bool,
    source_lost: bool,
    writer_fatal: Vec<Option<String>>,
    verify: Vec<VerifyResult>,
}

fn spawn<F: FnOnce() + Send + 'static>(name: String, f: F) -> bool {
    std::thread::Builder::new().name(name).spawn(f).is_ok()
}

fn run_pass(
    req: &TransferRequest,
    work: Vec<Vec<usize>>,
    alive: &Arc<Vec<AtomicBool>>,
    devs: &[Option<u64>],
    prior: Option<Arc<Vec<Option<String>>>>,
) -> PassResult {
    let nd = req.dests.len();
    let involved: Vec<bool> =
        (0..nd).map(|d| alive[d].load(Ordering::Relaxed) && work.iter().any(|w| w.contains(&d))).collect();
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    let ctx = Arc::new(Ctx {
        req: req.clone(),
        work,
        prior,
        alive: alive.clone(),
        devs: devs.to_vec(),
        board: HashBoard::new(req.files.len()),
        pool: BufPool::new(POOL_BUFFERS, req.chunk_size),
        queues: (0..nd).map(|_| WorkQueue::new()).collect(),
        reader_busy: Busy::default(),
        writer_busy: (0..nd).map(|_| Busy::default()).collect(),
        verifier_busy: (0..nd).map(|_| Busy::default()).collect(),
        done: Mutex::new(done_tx),
    });

    let mut writer_fatal: Vec<Option<String>> = vec![None; nd];
    let mut verify: Vec<VerifyResult> = (0..nd).map(|_| VerifyResult::default()).collect();
    let mut writer_done: Vec<bool> = involved.iter().map(|i| !i).collect();
    let mut verifier_done: Vec<bool> = writer_done.clone();
    let mut reader: Option<ReaderResult> = None;
    let mut hasher_done = false;

    let (htx, hrx) = mpsc::channel::<HMsg>();
    {
        let c = ctx.clone();
        if !spawn("rv-hasher".into(), move || {
            hasher_loop(&c, hrx);
            c.report(Done::Hasher);
        }) {
            hasher_done = true;
        }
    }
    for d in (0..nd).filter(|d| involved[*d]) {
        let (vtx, vrx) = mpsc::channel::<VMsg>();
        let c = ctx.clone();
        if !spawn(format!("rv-writer-{d}"), move || {
            let f = writer_loop(&c, d, vtx);
            c.report(Done::Writer(d, f));
        }) {
            writer_done[d] = true;
            writer_fatal[d] = Some(l!("Cannot start writer thread", "Không khởi động được luồng ghi"));
        }
        let c = ctx.clone();
        if !spawn(format!("rv-verify-{d}"), move || {
            let r = verify_loop(&c, d, vrx);
            c.report(Done::Verifier(d, r));
        }) {
            verifier_done[d] = true;
        }
    }
    {
        let c = ctx.clone();
        if !spawn("rv-reader".into(), move || {
            let r = reader_loop(&c, htx);
            for q in &c.queues {
                q.close();
            }
            c.report(Done::Reader(r));
        }) {
            reader = Some(ReaderResult { errors: BTreeMap::new(), cancelled: true, source_lost: false });
            for q in &ctx.queues {
                q.close();
            }
        }
    }

    // Supervise until everything finished or was abandoned.
    let stall = req.stall_timeout;
    loop {
        let all_done = reader.is_some()
            && hasher_done
            && writer_done.iter().all(|x| *x)
            && verifier_done.iter().all(|x| *x);
        if all_done {
            break;
        }
        match done_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Done::Reader(r)) => reader = Some(r),
            Ok(Done::Hasher) => hasher_done = true,
            Ok(Done::Writer(d, f)) => {
                if !writer_done[d] {
                    writer_done[d] = true;
                    writer_fatal[d] = f;
                }
            }
            Ok(Done::Verifier(d, r)) => {
                if !verifier_done[d] {
                    verifier_done[d] = true;
                    verify[d] = r;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if stall.is_zero() {
            continue;
        }
        for d in 0..nd {
            if writer_done[d] && verifier_done[d] {
                continue;
            }
            let stuck = ctx.writer_busy[d].stuck_for().max(ctx.verifier_busy[d].stuck_for());
            if stuck > stall {
                let msg = l!(
                    "Drive stopped responding for over {}s (disconnected or hung). This copy was abandoned; the others continue.",
                    "Ổ không phản hồi hơn {} giây (bị rút hoặc treo). Đã bỏ bản sao này, các ổ khác vẫn chạy tiếp.",
                    stall.as_secs()
                );
                ctx.alive[d].store(false, Ordering::Relaxed);
                ctx.queues[d].close_and_drain();
                ctx.pool.grow();
                req.dests[d].progress.set_phase(DestPhase::Failed);
                req.listener.dest_fatal(d, &msg);
                writer_done[d] = true;
                verifier_done[d] = true;
                writer_fatal[d] = Some(msg);
            }
        }
        if reader.is_none() && ctx.reader_busy.stuck_for() > stall {
            let msg = l!(
                "Source stopped responding for over {}s (card or reader disconnected or hung)",
                "Nguồn không phản hồi hơn {} giây (thẻ hoặc đầu đọc bị rút hoặc treo)",
                stall.as_secs()
            );
            let mut r = ReaderResult { source_lost: true, ..Default::default() };
            r.errors.insert(usize::MAX, msg);
            reader = Some(r);
            for q in &ctx.queues {
                q.push(WMsg::Abort);
                q.close();
            }
            // The hasher waits on the stuck reader's channel; no need to.
            hasher_done = true;
            ctx.board.close();
        }
    }

    let mut r = reader.unwrap_or_default();
    // usize::MAX marks a whole-source error (not tied to one file).
    let general = r.errors.remove(&usize::MAX);
    let mut source_errors = r.errors;
    if let Some(g) = general {
        let first = ctx.work.iter().position(|w| !w.is_empty()).unwrap_or(0);
        source_errors.entry(first).or_insert(g);
    }
    PassResult {
        hashes: ctx.board.snapshot(),
        source_errors,
        cancelled: r.cancelled,
        source_lost: r.source_lost,
        writer_fatal,
        verify,
    }
}

fn merge_pass(out: &mut TransferOutcome, pass: PassResult, first: bool) {
    for (fi, h) in pass.hashes.into_iter().enumerate() {
        if let Some(h) = h
            && (first || out.source_hashes[fi].is_none())
        {
            out.source_hashes[fi] = Some(h);
        }
    }
    for (fi, e) in pass.source_errors {
        out.source_errors.insert(fi, e);
    }
    out.cancelled |= pass.cancelled;
    out.source_lost |= pass.source_lost;
    for (d, (wf, vr)) in pass.writer_fatal.into_iter().zip(pass.verify).enumerate() {
        let dout = &mut out.dests[d];
        if dout.fatal.is_none() {
            dout.fatal = wf.or(vr.fatal);
        }
        for (fi, h) in vr.verified {
            dout.failures.remove(&fi);
            dout.verified[fi] = Some(h);
        }
        for (fi, reason) in vr.failures {
            dout.failures.insert(fi, reason);
        }
    }
}

/// Create the roll folder and every parent folder needed by the files this
/// destination will receive. Returns the device id of the roll folder so
/// writers can detect an unplugged drive.
pub fn prepare_dest_tree(roll_dir: &Path, files: &[SourceFile], work: &[Vec<usize>], d: usize) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::create_dir_all(roll_dir)?;
    let mut made: HashSet<PathBuf> = HashSet::new();
    for (fi, wanted) in work.iter().enumerate() {
        if !wanted.contains(&d) {
            continue;
        }
        if let Some(parent) = Path::new(&files[fi].rel).parent()
            && !parent.as_os_str().is_empty()
            && made.insert(parent.to_path_buf())
        {
            fs::create_dir_all(roll_dir.join(parent))?;
        }
    }
    Ok(fs::metadata(roll_dir)?.dev())
}

/// Run a transfer: one pass over `work`, then (optionally) one retry pass
/// for every file that did not verify on a destination that is still alive.
/// Never blocks forever on a dead drive (see `stall_timeout`).
pub fn run_transfer(req: &TransferRequest) -> TransferOutcome {
    let n = req.files.len();
    let nd = req.dests.len();
    assert_eq!(req.work.len(), n, "work must have one entry per file");
    let mut out = TransferOutcome {
        source_hashes: vec![None; n],
        dests: (0..nd).map(|_| DestOutcome { verified: vec![None; n], ..Default::default() }).collect(),
        ..Default::default()
    };
    let alive: Arc<Vec<AtomicBool>> = Arc::new((0..nd).map(|_| AtomicBool::new(true)).collect());
    let mut devs: Vec<Option<u64>> = vec![None; nd];
    let stall = if req.stall_timeout.is_zero() { DEFAULT_STALL } else { req.stall_timeout };
    for (d, slot) in devs.iter_mut().enumerate() {
        if !req.work.iter().any(|w| w.contains(&d)) {
            continue;
        }
        let roll = req.dests[d].roll_dir.clone();
        let files = req.files.clone();
        let work = req.work.clone();
        let res = crate::util::supervised(stall, move |b| {
            let _g = b.enter();
            prepare_dest_tree(&roll, &files, &work, d)
        });
        let err = match res {
            Some(Ok(dev)) => {
                *slot = Some(dev);
                None
            }
            Some(Err(e)) => Some(describe_io(&e)),
            None => Some(l!("drive not responding", "ổ không phản hồi")),
        };
        if let Some(e) = err {
            let msg = l!(
                "Cannot create destination folder {}: {}",
                "Không tạo được thư mục đích {}: {}",
                req.dests[d].roll_dir.display(),
                e
            );
            req.listener.dest_fatal(d, &msg);
            out.dests[d].fatal = Some(msg);
            alive[d].store(false, Ordering::Relaxed);
        }
    }

    let pass1 = run_pass(req, req.work.clone(), &alive, &devs, None);
    merge_pass(&mut out, pass1, true);

    if req.retry_failed && !out.cancelled && !out.source_lost && !req.cancel.load(Ordering::Relaxed) {
        let mut retry: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut any = false;
        for (fi, wanted) in req.work.iter().enumerate() {
            for &d in wanted {
                if alive[d].load(Ordering::Relaxed) && out.dests[d].verified[fi].is_none() {
                    retry[fi].push(d);
                    any = true;
                }
            }
        }
        if any {
            let first_hashes = out.source_hashes.clone();
            let pass2 = run_pass(req, retry, &alive, &devs, Some(Arc::new(first_hashes.clone())));
            for (fi, h2) in pass2.hashes.iter().enumerate() {
                if let (Some(h1), Some(h2)) = (&first_hashes[fi], h2)
                    && h1 != h2
                {
                    out.source_unstable.push(fi);
                }
            }
            for (fi, h2) in pass2.hashes.iter().enumerate() {
                if h2.is_some() && !pass2.source_errors.contains_key(&fi) {
                    out.source_errors.remove(&fi);
                }
            }
            merge_pass(&mut out, pass2, false);
        }
    }
    for (a, dout) in alive.iter().zip(out.dests.iter_mut()) {
        if !a.load(Ordering::Relaxed) && dout.fatal.is_none() {
            dout.fatal = Some(l!("Destination stopped responding", "Ổ đích ngừng phản hồi"));
        }
    }
    out
}
