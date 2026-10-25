//! Job orchestration: preflight checks, primary (card -> SSDs) transfer,
//! optional source re-verification, cascade (verified SSD copy -> HDD/NAS)
//! verified against the ORIGINAL card hashes, ASC MHL + legacy MHL, the
//! safe-to-eject policy, camera report, crash journal and resume.

use crate::ascmhl::{self, Action, CreatorInfo, FileRecord, Generation, Process};
use crate::darwin_io::AlignedBuf;
use crate::hashing::HashAlgo;
use crate::journal::{self, Journal, JournalEvent};
use crate::legacy_mhl::{self, LegacyCreator, LegacyEntry};
use crate::preset::{self, RollMeta};
use crate::report::{self, ReportBranding, ReportCopy, ReportInput};
use crate::scan::{self, PARTIAL_SUFFIX, SourceFile, SourceScan};
use crate::scheduler::{DeviceScheduler, Reservation, RollClaim};
use crate::tools::{self, SleepGuard, Sound};
use crate::transfer::{
    self, DEFAULT_CHUNK, DEFAULT_STALL, DestPhase, DestProgress, DestTarget, FaultInjector, SourceProgress, TransferListener,
    TransferRequest,
};
use crate::util::run_with_timeout;
use crate::volumes::{DeviceKind, VolumeCache, VolumeInfo};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub const TOOL_NAME: &str = "RollVault";
/// Head-room kept free on every destination: 32 MiB plus 0.2 % of the card
/// (MHL files, the report, file-system allocation overhead).
fn space_margin(total: u64) -> u64 {
    32 * 1024 * 1024 + total / 500
}
const MAX_MESSAGES: usize = 200;

// ---------------------------------------------------------------------------
// Public request / response types (serialised to the UI)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Primary,
    Cascade,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Primary => "Tier 1",
            Tier::Cascade => "Cascade",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestRequest {
    pub root: String,
    pub tier: Tier,
    #[serde(default)]
    pub kind_override: Option<DeviceKind>,
}

fn default_roll_template() -> String {
    preset::DEFAULT_ROLL_TEMPLATE.into()
}
fn default_report_template() -> String {
    preset::DEFAULT_REPORT_TEMPLATE.into()
}
fn default_min_copies() -> usize {
    2
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub source: String,
    pub destinations: Vec<DestRequest>,
    pub meta: RollMeta,
    #[serde(default = "default_roll_template")]
    pub roll_template: String,
    #[serde(default = "default_report_template")]
    pub report_template: String,
    pub algo: HashAlgo,
    #[serde(default = "default_min_copies")]
    pub min_copies: usize,
    #[serde(default)]
    pub source_verify: bool,
    #[serde(default = "yes")]
    pub generate_report: bool,
    #[serde(default = "yes")]
    pub report_pdf: bool,
    #[serde(default)]
    pub branding: ReportBranding,
    #[serde(default)]
    pub location: String,
    /// The user confirmed offloading a card that was already offloaded.
    #[serde(default)]
    pub confirm_repeat_card: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DestCheck {
    pub root: String,
    pub tier: Tier,
    pub roll_dir: String,
    pub volume_name: String,
    pub device_id: String,
    pub kind: DeviceKind,
    pub fs_type: String,
    pub free_bytes: u64,
    pub reserved_bytes: u64,
    pub needed_bytes: u64,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviousOffload {
    pub job_id: String,
    pub camera_id: String,
    pub roll_id: String,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Preflight {
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub source_name: String,
    pub source_device_id: String,
    pub card_vendor: Option<String>,
    pub file_count: usize,
    pub dir_count: usize,
    pub clip_count: usize,
    pub total_bytes: u64,
    pub largest_file: u64,
    pub fingerprint: String,
    pub roll_rel_path: String,
    pub dests: Vec<DestCheck>,
    pub distinct_devices: usize,
    pub previously_offloaded: Option<PreviousOffload>,
    /// Set when `previously_offloaded` must be confirmed before starting.
    pub needs_confirmation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Queued,
    Scanning,
    WaitingDevice,
    Copying,
    SourceVerify,
    Cascading,
    Reporting,
    Completed,
    CompletedWithErrors,
    Failed,
    Cancelled,
    Interrupted,
}

impl JobPhase {
    pub fn is_active(self) -> bool {
        !matches!(
            self,
            JobPhase::Completed
                | JobPhase::CompletedWithErrors
                | JobPhase::Failed
                | JobPhase::Cancelled
                | JobPhase::Interrupted
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DestSnapshot {
    pub index: usize,
    pub tier: Tier,
    pub root: String,
    pub roll_dir: String,
    pub volume_name: String,
    pub kind: DeviceKind,
    pub device_id: String,
    pub phase: DestPhase,
    pub bytes_written: u64,
    pub bytes_verified: u64,
    pub need_bytes: u64,
    pub prior_bytes: u64,
    pub total_bytes: u64,
    pub files_verified: u64,
    pub files_failed: u64,
    pub total_files: usize,
    pub write_speed: f64,
    pub verify_speed: f64,
    pub current_file: String,
    pub error: Option<String>,
    pub mhl_path: Option<String>,
    pub legacy_mhl_path: Option<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobSnapshot {
    pub id: String,
    pub meta: RollMeta,
    pub source: String,
    pub source_label: String,
    pub card_vendor: Option<String>,
    pub phase: JobPhase,
    pub status_text: String,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub source_read: u64,
    pub source_speed: f64,
    pub safe_to_eject: bool,
    pub safe_to_eject_at: Option<DateTime<Utc>>,
    pub verified_copies: usize,
    pub min_copies: usize,
    pub dests: Vec<DestSnapshot>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub report_path: Option<String>,
    pub report_pdf_path: Option<String>,
    pub percent: f64,
    pub eta_seconds: Option<u64>,
    pub resumable: bool,
    pub card_ejected: bool,
    pub algo: HashAlgo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryDest {
    pub root: String,
    pub roll_dir: String,
    pub tier: Tier,
    pub volume_name: String,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub job_id: String,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: JobPhase,
    pub meta: RollMeta,
    pub source: String,
    pub source_label: String,
    pub fingerprint: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub dests: Vec<HistoryDest>,
    pub report_path: Option<String>,
    pub safe_to_eject: bool,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Internal job state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedDest {
    pub root: PathBuf,
    pub tier: Tier,
    pub roll_dir: PathBuf,
    pub device_id: String,
    pub kind: DeviceKind,
    pub volume_name: String,
    pub fs_type: String,
    pub needed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Layout {
    source_label: String,
    source_device: String,
    card_vendor: Option<String>,
    roll_rel: PathBuf,
    report_rel: PathBuf,
    dests: Vec<PreparedDest>,
    created_at: DateTime<Utc>,
}

struct Prepared {
    scan: SourceScan,
    layout: Layout,
}

#[derive(Default)]
struct DestInfo {
    error: Option<String>,
    mhl: Option<String>,
    legacy: Option<String>,
    complete: bool,
    need_bytes: u64,
    prior_bytes: u64,
    prior_files: u64,
}

struct JobDest {
    spec: PreparedDest,
    progress: Arc<DestProgress>,
    info: Mutex<DestInfo>,
}

#[derive(Default)]
struct Meter {
    last: Option<(Instant, u64)>,
    rate: f64,
}

impl Meter {
    fn update(&mut self, value: u64) -> f64 {
        let now = Instant::now();
        match self.last {
            None => self.last = Some((now, value)),
            Some((t, v)) => {
                let dt = now.duration_since(t).as_secs_f64();
                if dt >= 0.25 {
                    let inst = value.saturating_sub(v) as f64 / dt;
                    self.rate = if self.rate == 0.0 { inst } else { 0.65 * self.rate + 0.35 * inst };
                    self.last = Some((now, value));
                }
            }
        }
        self.rate
    }
}

struct JobState {
    phase: JobPhase,
    status_text: String,
    errors: Vec<String>,
    warnings: Vec<String>,
    created_at: DateTime<Utc>,
    started_local: DateTime<Local>,
    finished_at: Option<DateTime<Utc>>,
    safe_to_eject_at: Option<DateTime<Utc>>,
    report_path: Option<String>,
    report_pdf_path: Option<String>,
    card_ejected: bool,
    resumable: bool,
}

pub struct Job {
    pub id: String,
    request: JobRequest,
    prepared: Prepared,
    files: Arc<Vec<SourceFile>>,
    cancel: Arc<AtomicBool>,
    source_progress: Arc<SourceProgress>,
    dests: Vec<JobDest>,
    state: Mutex<JobState>,
    meters: Mutex<HashMap<String, Meter>>,
    report_progress: AtomicUsize,
}

/// Lock-free copies used by `Job::snapshot`.
struct JobStateView {
    phase: JobPhase,
    status_text: String,
    errors: Vec<String>,
    warnings: Vec<String>,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    safe_to_eject_at: Option<DateTime<Utc>>,
    report_path: Option<String>,
    report_pdf_path: Option<String>,
    card_ejected: bool,
    resumable: bool,
}

impl From<&JobState> for JobStateView {
    fn from(s: &JobState) -> Self {
        JobStateView {
            phase: s.phase,
            status_text: s.status_text.clone(),
            errors: s.errors.clone(),
            warnings: s.warnings.clone(),
            created_at: s.created_at,
            finished_at: s.finished_at,
            safe_to_eject_at: s.safe_to_eject_at,
            report_path: s.report_path.clone(),
            report_pdf_path: s.report_pdf_path.clone(),
            card_ejected: s.card_ejected,
            resumable: s.resumable,
        }
    }
}

struct DestInfoView {
    error: Option<String>,
    mhl: Option<String>,
    legacy: Option<String>,
    complete: bool,
    need_bytes: u64,
    prior_bytes: u64,
    prior_files: u64,
}

impl From<&DestInfo> for DestInfoView {
    fn from(i: &DestInfo) -> Self {
        DestInfoView {
            error: i.error.clone(),
            mhl: i.mhl.clone(),
            legacy: i.legacy.clone(),
            complete: i.complete,
            need_bytes: i.need_bytes,
            prior_bytes: i.prior_bytes,
            prior_files: i.prior_files,
        }
    }
}

fn push_capped(list: &mut Vec<String>, msg: String) {
    if !list.contains(&msg) && list.len() < MAX_MESSAGES {
        list.push(msg);
    }
}

impl Job {
    fn set_phase(&self, phase: JobPhase, text: impl Into<String>) {
        if let Ok(mut st) = self.state.lock() {
            st.phase = phase;
            st.status_text = text.into();
        }
    }

    fn set_text(&self, text: impl Into<String>) {
        if let Ok(mut st) = self.state.lock() {
            st.status_text = text.into();
        }
    }

    fn error(&self, msg: impl Into<String>) {
        if let Ok(mut st) = self.state.lock() {
            push_capped(&mut st.errors, msg.into());
        }
    }

    fn warn(&self, msg: impl Into<String>) {
        if let Ok(mut st) = self.state.lock() {
            push_capped(&mut st.warnings, msg.into());
        }
    }

    fn phase(&self) -> JobPhase {
        self.state.lock().map(|s| s.phase).unwrap_or(JobPhase::Failed)
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn label(&self) -> String {
        format!("{} / {}", self.request.meta.camera_id, self.request.meta.roll_id)
    }

    fn complete_devices(&self) -> usize {
        let set: BTreeSet<&str> = self
            .dests
            .iter()
            .filter(|d| d.info.lock().map(|i| i.complete).unwrap_or(false))
            .map(|d| d.spec.device_id.as_str())
            .collect();
        set.len()
    }

    pub fn snapshot(&self) -> JobSnapshot {
        // Copy the state and release its lock before touching any other lock
        // (job threads may hold a destination lock while recording an error).
        let st = match self.state.lock() {
            Ok(g) => JobStateView::from(&*g),
            Err(p) => JobStateView::from(&*p.into_inner()),
        };
        let mut meters = match self.meters.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let scan = &self.prepared.scan;
        let total = scan.total_bytes;
        let source_read = self.source_progress.bytes_read.load(Ordering::Relaxed);
        let source_speed = if st.phase.is_active() {
            meters.entry("src".into()).or_default().update(source_read)
        } else {
            0.0
        };
        let mut done_work = 0f64;
        let mut all_work = 0f64;
        let mut eta: Option<f64> = None;
        let mut dests = Vec::with_capacity(self.dests.len());
        for (i, d) in self.dests.iter().enumerate() {
            let info = match d.info.lock() {
                Ok(g) => DestInfoView::from(&*g),
                Err(p) => DestInfoView::from(&*p.into_inner()),
            };
            let p = &d.progress;
            let written = p.bytes_written.load(Ordering::Relaxed);
            let verified = p.bytes_verified.load(Ordering::Relaxed);
            let phase = p.phase();
            let active = matches!(phase, DestPhase::Copying | DestPhase::Verifying) && st.phase.is_active();
            let ws = if active { meters.entry(format!("w{i}")).or_default().update(written) } else { 0.0 };
            let vs = if active { meters.entry(format!("v{i}")).or_default().update(verified) } else { 0.0 };
            let need = info.need_bytes;
            let done = if info.complete {
                2.0 * total as f64
            } else {
                2.0 * info.prior_bytes as f64 + written.min(need) as f64 + verified.min(need) as f64
            };
            done_work += done.min(2.0 * total as f64);
            all_work += 2.0 * total as f64;
            if active {
                let remaining = (2 * need).saturating_sub(written.min(need) + verified.min(need)) as f64;
                let rate = ws + vs;
                if rate > 1.0 {
                    let e = remaining / rate;
                    eta = Some(eta.map_or(e, |x: f64| x.max(e)));
                }
            }
            dests.push(DestSnapshot {
                index: i,
                tier: d.spec.tier,
                root: d.spec.root.display().to_string(),
                roll_dir: d.spec.roll_dir.display().to_string(),
                volume_name: d.spec.volume_name.clone(),
                kind: d.spec.kind,
                device_id: d.spec.device_id.clone(),
                phase,
                bytes_written: written,
                bytes_verified: verified,
                need_bytes: need,
                prior_bytes: info.prior_bytes,
                total_bytes: total,
                files_verified: info.prior_files + p.files_verified.load(Ordering::Relaxed),
                files_failed: p.files_failed.load(Ordering::Relaxed),
                total_files: scan.files.len(),
                write_speed: ws,
                verify_speed: vs,
                current_file: p.current.lock().map(|c| c.clone()).unwrap_or_default(),
                error: info.error.clone(),
                mhl_path: info.mhl.clone(),
                legacy_mhl_path: info.legacy.clone(),
                complete: info.complete,
            });
        }
        let verified_copies = {
            let set: BTreeSet<&str> =
                dests.iter().filter(|d| d.complete).map(|d| d.device_id.as_str()).collect();
            set.len()
        };
        let percent = if !st.phase.is_active() && st.phase == JobPhase::Completed {
            100.0
        } else if all_work > 0.0 {
            (done_work / all_work * 100.0).min(100.0)
        } else {
            0.0
        };
        JobSnapshot {
            id: self.id.clone(),
            meta: self.request.meta.clone(),
            source: self.request.source.clone(),
            source_label: self.prepared.layout.source_label.clone(),
            card_vendor: self.prepared.layout.card_vendor.clone(),
            phase: st.phase,
            status_text: st.status_text,
            created_at: st.created_at,
            finished_at: st.finished_at,
            total_files: scan.files.len(),
            total_bytes: total,
            source_read,
            source_speed,
            safe_to_eject: st.safe_to_eject_at.is_some(),
            safe_to_eject_at: st.safe_to_eject_at,
            verified_copies,
            min_copies: self.request.min_copies,
            dests,
            errors: st.errors,
            warnings: st.warnings,
            report_path: st.report_path,
            report_pdf_path: st.report_pdf_path,
            percent,
            eta_seconds: eta.map(|e| e.round() as u64),
            resumable: st.resumable,
            card_ejected: st.card_ejected,
            algo: self.request.algo,
        }
    }
}

/// Per file: verified hash and the time it was verified.
type VerifiedMap = Vec<Option<(String, DateTime<Utc>)>>;

/// Mutable per-run knowledge shared by the primary and cascade threads.
struct RunState {
    /// Original (card) hash per file.
    hashes: Vec<Option<String>>,
    /// Per destination, per file: verified hash and time.
    verified: Vec<VerifiedMap>,
    source_problem: bool,
}

struct JobListener {
    job: Arc<Job>,
    journal: Arc<Journal>,
    run: Arc<Mutex<RunState>>,
    /// Local transfer dest index -> global job dest index.
    map: Vec<usize>,
    record_source_hashes: bool,
}

impl TransferListener for JobListener {
    fn source_hashed(&self, file: usize, hash: &str) {
        if !self.record_source_hashes {
            return;
        }
        if let Ok(mut r) = self.run.lock()
            && r.hashes[file].is_none()
        {
            r.hashes[file] = Some(hash.to_string());
            self.journal.event(&JournalEvent::SourceHash { f: file, h: hash.to_string() });
        }
    }

    fn file_verified(&self, dest: usize, file: usize, hash: &str) {
        let g = self.map[dest];
        let t = Utc::now();
        self.journal.event(&JournalEvent::Verified { d: g, f: file, h: hash.to_string(), t });
        if let Ok(mut r) = self.run.lock() {
            r.verified[g][file] = Some((hash.to_string(), t));
        }
    }

    fn file_failed(&self, dest: usize, _file: usize, reason: &str) {
        let g = self.map[dest];
        self.journal.log(&format!("[{}] {}", self.job.dests[g].spec.volume_name, reason));
    }

    fn dest_fatal(&self, dest: usize, reason: &str) {
        let g = self.map[dest];
        self.journal.log(&format!("[{}] STOPPED: {}", self.job.dests[g].spec.volume_name, reason));
    }

    fn source_error(&self, _file: usize, reason: &str) {
        self.journal.log(&format!("[source] {reason}"));
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub data_dir: PathBuf,
    pub tool_version: String,
    pub sounds: bool,
    pub notifications: bool,
}

/// Fault injection for tests: (primary transfers, cascade transfers).
#[derive(Default, Clone)]
pub struct EngineFaults {
    pub primary: Option<Arc<FaultInjector>>,
    pub cascade: Option<Arc<FaultInjector>>,
    /// Shorter stall timeout so tests do not wait two minutes.
    pub stall_timeout: Option<Duration>,
}

pub struct VerifyTask {
    pub id: String,
    pub root: PathBuf,
    cancel: AtomicBool,
    progress: crate::reverify::VerifyProgress,
    state: Mutex<VerifyTaskState>,
}

struct VerifyTaskState {
    status: VerifyStatus,
    outcome: Option<crate::reverify::VerifyOutcome>,
    error: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    Running,
    Passed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifySnapshot {
    pub id: String,
    pub root: String,
    pub status: VerifyStatus,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub current: String,
    pub outcome: Option<crate::reverify::VerifyOutcome>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub struct Engine {
    cfg: EngineConfig,
    verifies: Mutex<Vec<Arc<VerifyTask>>>,
    sounds: AtomicBool,
    notifications: AtomicBool,
    scheduler: Arc<DeviceScheduler>,
    pub volumes: VolumeCache,
    jobs: Mutex<Vec<Arc<Job>>>,
    history: Mutex<Vec<HistoryEntry>>,
    sleep: Arc<SleepGuard>,
    start_lock: Mutex<()>,
    faults: Mutex<EngineFaults>,
}

struct Claims {
    _rolls: Vec<RollClaim>,
    _space: Vec<Reservation>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Arc<Engine> {
        let _ = fs::create_dir_all(cfg.data_dir.join("jobs"));
        let mut history: Vec<HistoryEntry> = fs::read(cfg.data_dir.join("history.json"))
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default();
        for h in history.iter_mut() {
            if h.status.is_active() {
                h.status = JobPhase::Interrupted;
                h.errors.push(l!("The app was closed while this job was running. You can Resume it.", "App bị tắt khi job đang chạy. Có thể bấm Tiếp tục."));
            }
        }
        let e = Arc::new(Engine {
            verifies: Mutex::new(Vec::new()),
            sounds: AtomicBool::new(cfg.sounds),
            notifications: AtomicBool::new(cfg.notifications),
            cfg,
            scheduler: DeviceScheduler::new(),
            volumes: VolumeCache::default(),
            jobs: Mutex::new(Vec::new()),
            history: Mutex::new(history),
            sleep: SleepGuard::new(),
            start_lock: Mutex::new(()),
            faults: Mutex::new(EngineFaults::default()),
        });
        e.save_history();
        e
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    pub fn set_alerts(&self, sounds: bool, notifications: bool) {
        self.sounds.store(sounds, Ordering::Relaxed);
        self.notifications.store(notifications, Ordering::Relaxed);
    }

    /// Ask every running job to stop (used when quitting the app).
    pub fn cancel_all(&self) {
        if let Ok(jobs) = self.jobs.lock() {
            for j in jobs.iter().filter(|j| j.phase().is_active()) {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
        if let Ok(v) = self.verifies.lock() {
            for t in v.iter() {
                t.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn set_faults(&self, f: EngineFaults) {
        if let Ok(mut g) = self.faults.lock() {
            *g = f;
        }
    }

    pub fn list_volumes(&self) -> Vec<VolumeInfo> {
        self.volumes.list()
    }

    pub fn snapshots(&self) -> Vec<JobSnapshot> {
        let jobs: Vec<Arc<Job>> = self.jobs.lock().map(|j| j.clone()).unwrap_or_default();
        jobs.iter().map(|j| j.snapshot()).collect()
    }

    pub fn any_active(&self) -> bool {
        let jobs = self.jobs.lock().map(|j| j.iter().any(|j| j.phase().is_active())).unwrap_or(false);
        let verifies = self
            .verifies
            .lock()
            .map(|v| v.iter().any(|t| t.state.lock().map(|s| s.status == VerifyStatus::Running).unwrap_or(false)))
            .unwrap_or(false);
        jobs || verifies
    }

    /// Re-verify a folder against its ASC MHL history in the background.
    pub fn start_verify(self: &Arc<Self>, root: PathBuf, author: String) -> Result<String, String> {
        if !VolumeCache::is_responsive(&root) {
            return Err(l!("{} is not responding or does not exist", "{} không phản hồi hoặc không tồn tại", root.display()));
        }
        if !root.join(ascmhl::ASCMHL_DIR).join(ascmhl::CHAIN_FILE).exists() {
            return Err(l!("This folder has no ASC MHL history (ascmhl folder). Pick the roll folder itself.", "Thư mục này không có lịch sử ASC MHL (thư mục ascmhl). Hãy chọn đúng thư mục roll."));
        }
        if let Ok(v) = self.verifies.lock()
            && v.iter().any(|t| t.root == root && t.state.lock().map(|s| s.status == VerifyStatus::Running).unwrap_or(false))
        {
            return Err(l!("This folder is already being verified", "Thư mục này đang được verify"));
        }
        let task = Arc::new(VerifyTask {
            id: uuid::Uuid::new_v4().to_string(),
            root,
            cancel: AtomicBool::new(false),
            progress: Default::default(),
            state: Mutex::new(VerifyTaskState {
                status: VerifyStatus::Running,
                outcome: None,
                error: None,
                started_at: Utc::now(),
                finished_at: None,
            }),
        });
        if let Ok(mut v) = self.verifies.lock() {
            v.push(task.clone());
        }
        let engine = self.clone();
        let id = task.id.clone();
        std::thread::Builder::new()
            .name("rv-reverify".into())
            .spawn(move || {
                let _awake = engine.sleep.acquire();
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::reverify::verify_folder(&task.root, &engine.cfg.tool_version, &author, &task.cancel, &task.progress)
                }));
                let mut st = task.state.lock().expect("verify state");
                st.finished_at = Some(Utc::now());
                match res {
                    Ok(Ok(out)) => {
                        st.status = if out.cancelled {
                            VerifyStatus::Cancelled
                        } else if out.ok() {
                            VerifyStatus::Passed
                        } else {
                            VerifyStatus::Failed
                        };
                        st.outcome = Some(out);
                    }
                    Ok(Err(e)) => {
                        st.status = VerifyStatus::Failed;
                        st.error = Some(e);
                    }
                    Err(_) => {
                        st.status = VerifyStatus::Failed;
                        st.error = Some(l!("Internal error while verifying", "Lỗi nội bộ khi verify"));
                    }
                }
                let status = st.status;
                drop(st);
                let name = task.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                match status {
                    VerifyStatus::Passed => engine.notify(Sound::Done, &l!("Re-verify {}: match", "Verify lại {}: khớp", name), &l!("Every file matches its ASC MHL history.", "Mọi file khớp lịch sử ASC MHL.")),
                    VerifyStatus::Failed => engine.notify(Sound::Error, &l!("Re-verify {}: FAILED", "Verify lại {}: LỖI", name), &l!("Some files differ from or are missing in the ASC MHL history.", "Có file sai hoặc thiếu so với lịch sử ASC MHL.")),
                    _ => {}
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub fn verify_snapshots(&self) -> Vec<VerifySnapshot> {
        let tasks: Vec<Arc<VerifyTask>> = self.verifies.lock().map(|v| v.clone()).unwrap_or_default();
        tasks
            .iter()
            .map(|t| {
                let st = t.state.lock().expect("verify state");
                VerifySnapshot {
                    id: t.id.clone(),
                    root: t.root.display().to_string(),
                    status: st.status,
                    bytes_done: t.progress.bytes_done.load(Ordering::Relaxed),
                    bytes_total: t.progress.bytes_total.load(Ordering::Relaxed),
                    files_done: t.progress.files_done.load(Ordering::Relaxed),
                    current: t.progress.current.lock().map(|c| c.clone()).unwrap_or_default(),
                    outcome: st.outcome.clone(),
                    error: st.error.clone(),
                    started_at: st.started_at,
                    finished_at: st.finished_at,
                }
            })
            .collect()
    }

    pub fn cancel_verify(&self, id: &str) {
        if let Ok(v) = self.verifies.lock() {
            for t in v.iter().filter(|t| t.id == id) {
                t.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn dismiss_verify(&self, id: &str) {
        if let Ok(mut v) = self.verifies.lock() {
            v.retain(|t| t.id != id || t.state.lock().map(|s| s.status == VerifyStatus::Running).unwrap_or(true));
        }
    }

    pub fn snapshot(&self, id: &str) -> Option<JobSnapshot> {
        self.find(id).map(|j| j.snapshot())
    }

    fn find(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs.lock().ok()?.iter().find(|j| j.id == id).cloned()
    }

    pub fn cancel(&self, id: &str) -> bool {
        match self.find(id) {
            Some(j) => {
                j.cancel.store(true, Ordering::Relaxed);
                j.set_text(l!("Cancelling…", "Đang huỷ..."));
                true
            }
            None => false,
        }
    }

    /// Remove a finished job from the active list (it stays in history).
    pub fn dismiss(&self, id: &str) -> bool {
        if let Ok(mut jobs) = self.jobs.lock() {
            let before = jobs.len();
            jobs.retain(|j| j.id != id || j.phase().is_active());
            return jobs.len() != before;
        }
        false
    }

    pub fn history(&self) -> Vec<HistoryEntry> {
        self.history.lock().map(|h| h.clone()).unwrap_or_default()
    }

    fn save_history(&self) {
        let Ok(h) = self.history.lock() else { return };
        let mut list = h.clone();
        drop(h);
        list.sort_by_key(|e| std::cmp::Reverse(e.created_at));
        list.truncate(2000);
        let path = self.cfg.data_dir.join("history.json");
        let tmp = self.cfg.data_dir.join(".history.json.tmp");
        if let Ok(data) = serde_json::to_vec_pretty(&list)
            && fs::write(&tmp, data).is_ok()
        {
            let _ = fs::rename(&tmp, &path);
        }
    }

    fn upsert_history(&self, entry: HistoryEntry) {
        if let Ok(mut h) = self.history.lock() {
            h.retain(|e| e.job_id != entry.job_id);
            h.push(entry);
        }
        self.save_history();
    }

    /// Eject the card a job read from. Refused while the card is being read.
    pub fn eject_source(&self, id: &str) -> Result<(), String> {
        let job = self.find(id).ok_or_else(|| l!("Job not found", "Không tìm thấy job"))?;
        let phase = job.phase();
        if matches!(phase, JobPhase::Scanning | JobPhase::Copying | JobPhase::SourceVerify | JobPhase::WaitingDevice) {
            return Err(l!("The card is still being read and cannot be ejected yet", "Thẻ đang được đọc, chưa thể eject"));
        }
        let vol = self
            .volumes
            .volume_for_path(Path::new(&job.request.source))
            .ok_or_else(|| l!("Card volume not found (already removed?)", "Không tìm thấy ổ của thẻ (đã rút?)"))?;
        if vol.internal {
            return Err(l!("The source is on the internal drive; nothing to eject", "Nguồn nằm trên ổ trong của máy, không eject"));
        }
        crate::volumes::eject(&vol.mount_point)?;
        if let Ok(mut st) = job.state.lock() {
            st.card_ejected = true;
        }
        self.volumes.forget();
        Ok(())
    }

    fn notify(&self, sound: Sound, title: &str, body: &str) {
        if self.sounds.load(Ordering::Relaxed) {
            tools::play_sound(sound);
        }
        if self.notifications.load(Ordering::Relaxed) {
            tools::notify(title, body);
        }
    }

    // -----------------------------------------------------------------------
    // Preflight
    // -----------------------------------------------------------------------

    pub fn preflight(&self, req: &JobRequest) -> Preflight {
        self.prepare(req).0
    }

    fn prepare(&self, req: &JobRequest) -> (Preflight, Option<Prepared>) {
        let mut pf = Preflight::default();
        let err = |pf: &mut Preflight, m: String| push_capped(&mut pf.errors, m);

        if let Err(e) = req.meta.validate() {
            err(&mut pf, e);
        }
        let roll_rel = match preset::render_template(&req.roll_template, &req.meta) {
            Ok(p) => Some(p),
            Err(e) => {
                err(&mut pf, l!("Roll folder template: {}", "Mẫu thư mục roll: {}", e));
                None
            }
        };
        let report_rel = match preset::render_template(&req.report_template, &req.meta) {
            Ok(p) => Some(p),
            Err(e) => {
                err(&mut pf, l!("Report folder template: {}", "Mẫu thư mục báo cáo: {}", e));
                None
            }
        };
        if let Some(r) = &roll_rel {
            pf.roll_rel_path = r.display().to_string();
        }
        if req.min_copies == 0 {
            err(&mut pf, l!("Minimum copies before eject must be at least 1", "Số bản tối thiểu để rút thẻ phải từ 1 trở lên"));
        }

        // Source
        let source = PathBuf::from(req.source.trim());
        let mut scan_result: Option<SourceScan> = None;
        let mut source_vol: Option<VolumeInfo> = None;
        if req.source.trim().is_empty() {
            err(&mut pf, l!("No source card selected", "Chưa chọn thẻ nguồn"));
        } else if source == Path::new("/") || source == Path::new("/System/Volumes/Data") {
            err(&mut pf, l!("The whole system drive cannot be used as a source", "Không thể dùng toàn bộ ổ hệ thống làm nguồn"));
        } else if !VolumeCache::is_responsive(&source) {
            err(
                &mut pf,
                l!(
                    "Source {} is not responding. macOS may be waiting for you to allow access to this drive, or the drive is hung.",
                    "Nguồn {} không phản hồi. macOS có thể đang chờ bạn cho phép truy cập ổ này, hoặc ổ bị treo.",
                    source.display()
                ),
            );
        } else {
            source_vol = self.volumes.volume_for_path(&source);
            pf.source_name = source_vol
                .as_ref()
                .map(|v| {
                    if Path::new(&v.mount_point) == source.as_path() {
                        v.name.clone()
                    } else {
                        format!("{} › {}", v.name, source.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())
                    }
                })
                .unwrap_or_else(|| source.display().to_string());
            pf.source_device_id = source_vol.as_ref().map(|v| v.device_id.clone()).unwrap_or_default();
            pf.card_vendor = scan::detect_card_vendor(&source).map(str::to_string);
            let src = source.clone();
            match run_with_timeout(Duration::from_secs(120), move || scan::scan_source(&src)) {
                None => err(&mut pf, l!("Scanning the card took over 2 minutes; the source may be hung", "Quét thẻ quá 2 phút, ổ nguồn có thể bị treo")),
                Some(Err(e)) => err(&mut pf, e.to_string()),
                Some(Ok(s)) => {
                    if s.files.is_empty() {
                        err(&mut pf, l!("The source has no files to copy", "Nguồn không có file nào để chép"));
                    }
                    pf.file_count = s.files.len();
                    pf.dir_count = s.dirs.len();
                    pf.total_bytes = s.total_bytes;
                    pf.largest_file = s.largest_file();
                    pf.fingerprint = s.fingerprint.clone();
                    pf.clip_count = s.files.iter().filter(|f| report::is_clip(&f.rel)).count();
                    for w in &s.warnings {
                        push_capped(&mut pf.warnings, w.clone());
                    }
                    scan_result = Some(s);
                }
            }
        }

        // Repeat-card detection
        if !pf.fingerprint.is_empty()
            && let Ok(h) = self.history.lock()
            && let Some(prev) = h
                .iter()
                .filter(|e| e.fingerprint == pf.fingerprint && e.status != JobPhase::Cancelled)
                .max_by_key(|e| e.created_at)
        {
            pf.previously_offloaded = Some(PreviousOffload {
                job_id: prev.job_id.clone(),
                camera_id: prev.meta.camera_id.clone(),
                roll_id: prev.meta.roll_id.clone(),
                finished_at: prev.finished_at,
            });
            pf.needs_confirmation = !req.confirm_repeat_card;
            push_capped(
                &mut pf.warnings,
                l!(
                    "This card was already offloaded as {} / {}. You may have re-inserted an old card.",
                    "Thẻ này đã được offload trước đó thành {} / {}. Có thể bạn đang cắm lại thẻ cũ.",
                    prev.meta.camera_id,
                    prev.meta.roll_id
                ),
            );
        }

        // Destinations
        if !req.destinations.iter().any(|d| d.tier == Tier::Primary) {
            err(&mut pf, l!("At least one Tier 1 destination (copied straight from the card) is required", "Cần ít nhất 1 ổ đích Tier 1 (chép trực tiếp từ thẻ)"));
        }
        let source_canon = fs::canonicalize(&source).ok();
        let mut prepared_dests: Vec<PreparedDest> = Vec::new();
        let mut seen_roots: Vec<PathBuf> = Vec::new();
        for d in &req.destinations {
            let root = PathBuf::from(d.root.trim());
            let mut problems: Vec<String> = Vec::new();
            let mut check = DestCheck {
                root: d.root.clone(),
                tier: d.tier,
                roll_dir: String::new(),
                volume_name: root.display().to_string(),
                device_id: String::new(),
                kind: d.kind_override.unwrap_or(DeviceKind::Unknown),
                fs_type: String::new(),
                free_bytes: 0,
                reserved_bytes: 0,
                needed_bytes: 0,
                problems: Vec::new(),
            };
            if !root.is_dir() {
                problems.push(l!("Destination drive or folder not found (removed?)", "Ổ hoặc thư mục đích không tồn tại (đã rút?)"));
            } else if !VolumeCache::is_responsive(&root) {
                problems.push(l!("Destination not responding (macOS may be asking for access, or the drive is hung)", "Ổ đích không phản hồi (macOS đang xin quyền truy cập hoặc ổ bị treo)"));
            } else {
                let canon = fs::canonicalize(&root).unwrap_or(root.clone());
                let vol = self.volumes.volume_for_path(&canon);
                if let Some(v) = &vol {
                    check.volume_name = v.name.clone();
                    check.device_id = v.device_id.clone();
                    check.kind = d.kind_override.unwrap_or(v.kind);
                    check.fs_type = v.fs_type.clone();
                    check.free_bytes = v.free_bytes;
                    check.reserved_bytes = self.scheduler.reserved_bytes(&v.device_id);
                    if v.read_only {
                        problems.push(l!("Read-only volume", "Ổ chỉ đọc"));
                    }
                    if let Some(sv) = &source_vol
                        && sv.device_id == v.device_id
                        && !sv.device_id.is_empty()
                    {
                        problems.push(l!("Destination is on the same physical device as the source", "Ổ đích nằm trên cùng thiết bị vật lý với nguồn"));
                    }
                }
                if let Some(sc) = &source_canon
                    && canon.starts_with(sc)
                {
                    problems.push(l!("Destination folder is inside the source", "Thư mục đích nằm bên trong nguồn"));
                }
                if seen_roots.iter().any(|r| r == &canon) {
                    problems.push(l!("Destination selected twice", "Ổ đích bị chọn hai lần"));
                }
                seen_roots.push(canon.clone());

                if let Some(rel) = &roll_rel {
                    let roll_dir = canon.join(rel);
                    check.roll_dir = roll_dir.display().to_string();
                    if let Some(sc) = &source_canon
                        && sc.starts_with(&roll_dir)
                    {
                        problems.push(l!("Source is inside the destination roll folder", "Nguồn nằm bên trong thư mục roll đích"));
                    }
                    if dir_has_entries(&roll_dir) {
                        problems.push(l!(
                            "Roll folder already exists and has data: {}. Increase the Roll ID; RollVault never overwrites.",
                            "Thư mục roll đã tồn tại và có dữ liệu: {}. Hãy tăng Roll ID, RollVault không bao giờ ghi đè.",
                            roll_dir.display()
                        ));
                    }
                    if self.scheduler.is_roll_claimed(&roll_dir) {
                        problems.push(l!("Another job is writing to this roll folder", "Một job khác đang ghi vào cùng thư mục roll này"));
                    }
                    if let Some(s) = &scan_result {
                        let block = vol.as_ref().map(|v| v.block_size.max(512)).unwrap_or(4096);
                        let needed: u64 =
                            s.files.iter().map(|f| f.size.div_ceil(block) * block).sum::<u64>() + space_margin(s.total_bytes);
                        check.needed_bytes = needed;
                        if check.free_bytes > 0 && check.free_bytes.saturating_sub(check.reserved_bytes) < needed {
                            let reserved_note = if check.reserved_bytes > 0 {
                                l!(" after space reserved by other jobs", " sau khi trừ phần các job khác đã giữ")
                            } else {
                                String::new()
                            };
                            problems.push(l!(
                                "Not enough space: need {}, free {}{}",
                                "Không đủ dung lượng: cần {}, còn trống {}{}",
                                human_bytes(needed),
                                human_bytes(check.free_bytes.saturating_sub(check.reserved_bytes)),
                                reserved_note
                            ));
                        }
                        if check.fs_type == "msdos" && s.largest_file() >= 4 * 1024 * 1024 * 1024 {
                            problems.push(l!("A FAT32 volume cannot hold files over 4 GB", "Ổ FAT32 không chứa được file lớn hơn 4 GB"));
                        }
                    }
                    if problems.is_empty() {
                        let probe = canon.join(format!(".rollvault-write-test-{}", uuid::Uuid::new_v4()));
                        match fs::write(&probe, b"rv") {
                            Ok(()) => {
                                let _ = fs::remove_file(&probe);
                            }
                            Err(e) => problems.push(l!("Cannot write to the drive: {}", "Không ghi được vào ổ: {}", transfer::describe_io(&e))),
                        }
                    }
                    if check.kind == DeviceKind::Network {
                        push_capped(
                            &mut pf.warnings,
                            l!(
                                "{}: a network drive may serve verification reads from this Mac's cache. Run Re-verify on the NAS copy afterwards (from History).",
                                "{}: ổ mạng có thể trả dữ liệu từ cache của máy khi verify. Nên chạy Verify lại cho bản NAS sau đó (trong Lịch sử).",
                                check.volume_name
                            ),
                        );
                    }
                    prepared_dests.push(PreparedDest {
                        root: canon.clone(),
                        tier: d.tier,
                        roll_dir,
                        device_id: check.device_id.clone(),
                        kind: check.kind,
                        volume_name: check.volume_name.clone(),
                        fs_type: check.fs_type.clone(),
                        needed_bytes: check.needed_bytes,
                    });
                }
            }
            for p in &problems {
                err(&mut pf, format!("{}: {}", check.volume_name, p));
            }
            check.problems = problems;
            pf.dests.push(check);
        }

        // Physical-device redundancy
        let mut by_device: HashMap<&str, Vec<&str>> = HashMap::new();
        for d in &pf.dests {
            if !d.device_id.is_empty() {
                by_device.entry(d.device_id.as_str()).or_default().push(d.volume_name.as_str());
            }
        }
        pf.distinct_devices = by_device.len();
        for names in by_device.values().filter(|n| n.len() > 1) {
            push_capped(
                &mut pf.warnings,
                l!(
                    "{} are on the same physical drive and count as a single copy.",
                    "{} nằm trên cùng một ổ vật lý, chỉ được tính là 1 bản sao.",
                    names.join(if crate::l10n::vi() { " và " } else { " and " })
                ),
            );
        }
        if pf.errors.is_empty() && pf.distinct_devices < req.min_copies {
            push_capped(
                &mut pf.warnings,
                l!(
                    "Only {} distinct physical drive(s). RollVault needs {} copies on {} different drives before it reports safe to eject.",
                    "Chỉ có {} ổ vật lý khác nhau. RollVault cần {} bản trên {} ổ khác nhau mới báo an toàn rút thẻ.",
                    pf.distinct_devices,
                    req.min_copies,
                    req.min_copies
                ),
            );
        }
        if req.generate_report && tools::ffprobe().is_none() {
            push_capped(&mut pf.warnings, l!("ffmpeg/ffprobe not found: the report will have no thumbnails or metadata.", "Không tìm thấy ffmpeg/ffprobe: báo cáo sẽ thiếu thumbnail và metadata."));
        }
        if req.generate_report && req.report_pdf && tools::find_chromium().is_none() {
            push_capped(&mut pf.warnings, l!("Google Chrome not found: the report will be HTML only, no PDF.", "Không tìm thấy Google Chrome: báo cáo chỉ có HTML, không có PDF."));
        }
        if req.algo.legacy_mhl_tag().is_none() {
            push_capped(
                &mut pf.warnings,
                l!("{} does not exist in legacy MHL v1.1; only ASC MHL is written.", "{} không có trong MHL v1.1 cũ, chỉ xuất ASC MHL.", req.algo.label()),
            );
        }

        pf.ok = pf.errors.is_empty() && !pf.needs_confirmation;
        let prepared = match (pf.errors.is_empty(), scan_result, roll_rel, report_rel) {
            (true, Some(scan), Some(roll_rel), Some(report_rel)) => Some(Prepared {
                scan,
                layout: Layout {
                    source_label: pf.source_name.clone(),
                    source_device: pf.source_device_id.clone(),
                    card_vendor: pf.card_vendor.clone(),
                    roll_rel,
                    report_rel,
                    dests: prepared_dests,
                    created_at: Utc::now(),
                },
            }),
            _ => None,
        };
        (pf, prepared)
    }

    // -----------------------------------------------------------------------
    // Start / resume
    // -----------------------------------------------------------------------

    /// Start a job. Returns the job id, or the failed preflight.
    pub fn start(self: &Arc<Self>, req: JobRequest) -> Result<String, Preflight> {
        let _guard = self.start_lock.lock().expect("start lock");
        let (pf, prepared) = self.prepare(&req);
        if !pf.ok {
            return Err(pf);
        }
        let Some(prepared) = prepared else { return Err(pf) };
        let id = uuid::Uuid::new_v4().to_string();
        let claims = match self.claim(&prepared.layout, &prepared.layout.dests.iter().map(|d| d.needed_bytes).collect::<Vec<_>>()) {
            Ok(c) => c,
            Err(e) => {
                let mut pf = pf;
                pf.ok = false;
                pf.errors.push(e);
                return Err(pf);
            }
        };
        let n = prepared.scan.files.len();
        let nd = prepared.layout.dests.len();
        let run = RunState { hashes: vec![None; n], verified: vec![vec![None; n]; nd], source_problem: false };
        let job = self.make_job(id.clone(), req, prepared, pf.warnings.clone(), Utc::now());
        self.launch(job, run, claims, false);
        Ok(id)
    }

    fn claim(&self, layout: &Layout, needed: &[u64]) -> Result<Claims, String> {
        let mut rolls = Vec::new();
        let mut space = Vec::new();
        for (d, need) in layout.dests.iter().zip(needed) {
            match self.scheduler.claim_roll(&d.roll_dir) {
                Some(c) => rolls.push(c),
                None => return Err(l!("{}: roll folder is in use by another job", "{}: thư mục roll đang được job khác dùng", d.volume_name)),
            }
            space.push(self.scheduler.reserve(&d.device_id, *need));
        }
        Ok(Claims { _rolls: rolls, _space: space })
    }

    fn make_job(
        &self,
        id: String,
        request: JobRequest,
        prepared: Prepared,
        warnings: Vec<String>,
        created_at: DateTime<Utc>,
    ) -> Arc<Job> {
        let dests = prepared
            .layout
            .dests
            .iter()
            .map(|d| JobDest { spec: d.clone(), progress: Arc::new(DestProgress::default()), info: Mutex::new(DestInfo::default()) })
            .collect();
        let files = Arc::new(prepared.scan.files.clone());
        Arc::new(Job {
            id,
            request,
            prepared,
            files,
            cancel: Arc::new(AtomicBool::new(false)),
            source_progress: Arc::new(SourceProgress::default()),
            dests,
            state: Mutex::new(JobState {
                phase: JobPhase::Queued,
                status_text: l!("Preparing", "Đang chuẩn bị"),
                errors: Vec::new(),
                warnings,
                created_at,
                started_local: Local::now(),
                finished_at: None,
                safe_to_eject_at: None,
                report_path: None,
                report_pdf_path: None,
                card_ejected: false,
                resumable: false,
            }),
            meters: Mutex::new(HashMap::new()),
            report_progress: AtomicUsize::new(0),
        })
    }

    fn launch(self: &Arc<Self>, job: Arc<Job>, run: RunState, claims: Claims, resumed: bool) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.retain(|j| j.id != job.id);
            jobs.push(job.clone());
        }
        let engine = self.clone();
        let spawned = std::thread::Builder::new().name(format!("rv-job-{}", &job.id[..8])).spawn(move || {
            let job2 = job.clone();
            let engine2 = engine.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                engine2.run_job(&job2, run, resumed);
            }));
            if outcome.is_err() {
                job.error(l!("Internal error in the job (panic). Verified data is safe; you can Resume.", "Lỗi nội bộ trong job (panic). Dữ liệu đã verify vẫn an toàn; có thể bấm Tiếp tục."));
                job.set_phase(JobPhase::Failed, l!("Internal error", "Lỗi nội bộ"));
                if let Ok(mut st) = job.state.lock() {
                    st.resumable = true;
                    st.finished_at = Some(Utc::now());
                }
                engine.record_history(&job);
            }
            drop(claims);
        });
        if let Err(e) = spawned {
            eprintln!("cannot spawn job thread: {e}");
        }
    }

    /// Resume an interrupted, cancelled or failed job from its journal.
    pub fn resume(self: &Arc<Self>, job_id: &str) -> Result<String, String> {
        let _guard = self.start_lock.lock().expect("start lock");
        if let Some(j) = self.find(job_id)
            && j.phase().is_active()
        {
            return Err(l!("The job is running", "Job đang chạy"));
        }
        let data = &self.cfg.data_dir;
        let request: JobRequest = journal::read_json(data, job_id, "request.json").map_err(|e| l!("Cannot read the job: {}", "Không đọc được job: {}", e))?;
        let layout: Layout = journal::read_json(data, job_id, "layout.json").map_err(|e| l!("Cannot read the job: {}", "Không đọc được job: {}", e))?;
        let scan: SourceScan = journal::read_json(data, job_id, "scan.json").map_err(|e| l!("Cannot read the job: {}", "Không đọc được job: {}", e))?;
        let events = journal::read_events(data, job_id).unwrap_or_default();
        let n = scan.files.len();
        let nd = layout.dests.len();
        let mut run = RunState { hashes: vec![None; n], verified: vec![vec![None; n]; nd], source_problem: false };
        let mut complete = vec![false; nd];
        let mut mhl: Vec<(Option<String>, Option<String>)> = vec![(None, None); nd];
        let mut safe_at = None;
        for ev in events {
            match ev {
                JournalEvent::SourceHash { f, h } if f < n => run.hashes[f] = Some(h),
                JournalEvent::Verified { d, f, h, t } if d < nd && f < n => run.verified[d][f] = Some((h, t)),
                JournalEvent::DestComplete { d, mhl: m, legacy } if d < nd => {
                    complete[d] = true;
                    mhl[d] = (m, legacy);
                }
                JournalEvent::SafeToEject { t } => safe_at = Some(t),
                _ => {}
            }
        }

        // Re-check on-disk state of every destination that is not complete.
        for (d, spec) in layout.dests.iter().enumerate() {
            if complete[d] {
                continue;
            }
            if !spec.root.is_dir() {
                return Err(l!("{} is not connected. Reconnect it and try again.", "{} không có mặt. Hãy cắm lại ổ rồi thử lại.", spec.volume_name));
            }
            for (fi, f) in scan.files.iter().enumerate() {
                let (temp, fin) = transfer::dest_paths(&spec.roll_dir, &f.rel, fi);
                let _ = fs::remove_file(&temp);
                let ok_on_disk = fs::metadata(&fin).map(|m| m.len() == f.size).unwrap_or(false);
                if run.verified[d][fi].is_some() && !ok_on_disk {
                    run.verified[d][fi] = None;
                }
                if run.verified[d][fi].is_none() && fin.exists() {
                    // Renamed after verification but the journal line was lost
                    // in the crash; it is ours (the roll folder is exclusive).
                    let _ = fs::remove_file(&fin);
                }
            }
            remove_partials(&spec.roll_dir);
        }

        let primary_incomplete = layout.dests.iter().enumerate().any(|(d, s)| s.tier == Tier::Primary && !complete[d]);
        let mut warnings = Vec::new();
        if primary_incomplete {
            let src = PathBuf::from(&request.source);
            if !VolumeCache::is_responsive(&src) {
                return Err(l!("Insert the original source card to resume the Tier 1 copy", "Cần cắm lại đúng thẻ nguồn để tiếp tục chép Tier 1"));
            }
            let rescanned = scan::scan_source(&src).map_err(|e| e.to_string())?;
            if rescanned.fingerprint != scan.fingerprint {
                return Err(l!("The inserted card differs from the original (file list or dates changed). Cannot resume.", "Thẻ đang cắm khác với thẻ ban đầu (danh sách file hoặc thời gian sửa đổi khác). Không thể tiếp tục."));
            }
        } else {
            warnings.push(l!("Tier 1 copies are complete; resuming from the SSD copy without the card.", "Các bản Tier 1 đã hoàn tất, tiếp tục từ bản SSD mà không cần thẻ."));
        }

        let needed: Vec<u64> = layout
            .dests
            .iter()
            .enumerate()
            .map(|(d, _)| {
                scan.files
                    .iter()
                    .enumerate()
                    .filter(|(fi, _)| !complete[d] && run.verified[d][*fi].is_none())
                    .map(|(_, f)| f.size)
                    .sum::<u64>()
            })
            .collect();
        let claims = self.claim(&layout, &needed)?;
        let created = layout.created_at;
        let prepared = Prepared { scan, layout };
        let job = self.make_job(job_id.to_string(), request, prepared, warnings, created);
        for (d, jd) in job.dests.iter().enumerate() {
            let mut info = jd.info.lock().expect("info");
            info.complete = complete[d];
            info.mhl = mhl[d].0.clone();
            info.legacy = mhl[d].1.clone();
            if complete[d] {
                jd.progress.set_phase(DestPhase::Done);
            }
        }
        if let Ok(mut st) = job.state.lock() {
            st.safe_to_eject_at = safe_at;
        }
        self.launch(job, run, claims, true);
        Ok(job_id.to_string())
    }

    // -----------------------------------------------------------------------
    // The job itself
    // -----------------------------------------------------------------------

    fn record_history(&self, job: &Job) {
        let snap = job.snapshot();
        self.upsert_history(HistoryEntry {
            job_id: job.id.clone(),
            created_at: snap.created_at,
            finished_at: snap.finished_at,
            status: snap.phase,
            meta: job.request.meta.clone(),
            source: job.request.source.clone(),
            source_label: snap.source_label.clone(),
            fingerprint: job.prepared.scan.fingerprint.clone(),
            file_count: snap.total_files,
            total_bytes: snap.total_bytes,
            dests: snap
                .dests
                .iter()
                .map(|d| HistoryDest {
                    root: d.root.clone(),
                    roll_dir: d.roll_dir.clone(),
                    tier: d.tier,
                    volume_name: d.volume_name.clone(),
                    complete: d.complete,
                })
                .collect(),
            report_path: snap.report_path.clone(),
            safe_to_eject: snap.safe_to_eject,
            errors: snap.errors.clone(),
        });
    }

    fn update_safe_to_eject(&self, job: &Job, run: &Mutex<RunState>, journal: &Journal) {
        let source_problem = run.lock().map(|r| r.source_problem).unwrap_or(true);
        if source_problem || job.cancelled() {
            return;
        }
        let copies = job.complete_devices();
        if copies < job.request.min_copies {
            return;
        }
        let first = {
            let mut st = job.state.lock().expect("state");
            if st.safe_to_eject_at.is_some() {
                false
            } else {
                st.safe_to_eject_at = Some(Utc::now());
                true
            }
        };
        if first {
            journal.event(&JournalEvent::SafeToEject { t: Utc::now() });
            journal.log(&format!("SAFE TO EJECT: {copies} verified copies on {copies} different drives"));
            self.notify(
                Sound::SafeToEject,
                &l!("Safe to eject {}", "An toàn rút thẻ {}", job.label()),
                &l!("{} verified copies on {} different drives.", "{} bản đã verify trên {} ổ khác nhau.", copies, copies),
            );
        }
    }

    fn run_job(self: &Arc<Self>, job: &Arc<Job>, run: RunState, resumed: bool) {
        let _awake = self.sleep.acquire();
        let journal = match Journal::open(&self.cfg.data_dir, &job.id) {
            Ok(j) => Arc::new(j),
            Err(e) => {
                job.error(l!("Cannot create the job journal: {}", "Không tạo được nhật ký job: {}", e));
                job.set_phase(JobPhase::Failed, l!("Journal error", "Lỗi nhật ký"));
                return;
            }
        };
        if !resumed {
            let _ = journal.write_json("request.json", &job.request);
            let _ = journal.write_json("layout.json", &job.prepared.layout);
            let _ = journal.write_json("scan.json", &job.prepared.scan);
        }
        journal.log(&format!(
            "{} {}: {} files, {} -> {} destinations",
            if resumed { "RESUME" } else { "START" },
            job.label(),
            job.files.len(),
            human_bytes(job.prepared.scan.total_bytes),
            job.dests.len()
        ));
        if let Ok(mut st) = job.state.lock() {
            st.started_local = Local::now();
        }
        self.record_history(job);

        let run = Arc::new(Mutex::new(run));
        self.run_primary(job, &run, &journal);
        if !job.cancelled() {
            self.run_cascades(job, &run, &journal);
        }
        if !job.cancelled() && job.request.generate_report && job.complete_devices() > 0 {
            self.run_report(job, &run, &journal);
        }
        self.finish(job, &run, &journal);
    }

    /// Record a transfer's per-destination outcome. Messages are collected
    /// first so no two job locks are ever held at the same time.
    fn apply_outcome(&self, job: &Job, journal: &Journal, g: usize, o: &transfer::DestOutcome) {
        let d = &job.dests[g];
        let failures: Vec<String> = o.failures.values().map(|r| format!("{}: {r}", d.spec.volume_name)).collect();
        if let Some(f) = &o.fatal {
            journal.event(&JournalEvent::DestFailed { d: g, reason: f.clone() });
            job.error(format!("{}: {f}", d.spec.volume_name));
        }
        for f in failures {
            job.error(f);
        }
        if let Ok(mut info) = d.info.lock()
            && info.error.is_none()
        {
            if let Some(f) = &o.fatal {
                info.error = Some(f.clone());
            } else if let Some((_, first)) = o.failures.iter().next() {
                info.error = Some(l!("{} file(s) failed, e.g. {}", "{} file lỗi, ví dụ: {}", o.failures.len(), first));
            }
        }
    }

    fn make_dirs(&self, job: &Job, g: usize) -> Result<(), String> {
        let roll = job.dests[g].spec.roll_dir.clone();
        let dirs: Vec<String> = job.prepared.scan.dirs.iter().map(|d| d.rel.clone()).collect();
        match crate::util::supervised(self.stall_timeout(), move |b| {
            let _g = b.enter();
            create_dirs(&roll, &dirs)
        }) {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(l!("Cannot create folders: {}", "Không tạo được thư mục: {}", transfer::describe_io(&e))),
            None => Err(l!("Drive not responding", "Ổ không phản hồi")),
        }
    }

    fn run_primary(&self, job: &Arc<Job>, run: &Arc<Mutex<RunState>>, journal: &Arc<Journal>) {
        let files = job.files.clone();
        let n = files.len();
        let primaries: Vec<usize> = job
            .dests
            .iter()
            .enumerate()
            .filter(|(_, d)| d.spec.tier == Tier::Primary && !d.info.lock().map(|x| x.complete).unwrap_or(false))
            .map(|(i, _)| i)
            .collect();
        if primaries.is_empty() {
            return;
        }
        let (work, known): (Vec<Vec<usize>>, Vec<Option<String>>) = {
            let r = run.lock().expect("run");
            let work = (0..n)
                .map(|fi| (0..primaries.len()).filter(|&l| r.verified[primaries[l]][fi].is_none()).collect())
                .collect();
            (work, r.hashes.clone())
        };
        for (l, &g) in primaries.iter().enumerate() {
            let (prior_bytes, prior_files) = {
                let r = run.lock().expect("run");
                let done: Vec<usize> = (0..n).filter(|fi| r.verified[g][*fi].is_some()).collect();
                (done.iter().map(|fi| files[*fi].size).sum::<u64>(), done.len() as u64)
            };
            let mut info = job.dests[g].info.lock().expect("info");
            info.need_bytes = (0..n).filter(|fi| work[*fi].contains(&l)).map(|fi| files[fi].size).sum();
            info.prior_bytes = prior_bytes;
            info.prior_files = prior_files;
        }
        let any_work = work.iter().any(|w| !w.is_empty());

        if any_work {
            let devices: Vec<(String, DeviceKind)> =
                primaries.iter().map(|&g| (job.dests[g].spec.device_id.clone(), job.dests[g].spec.kind)).collect();
            for &g in &primaries {
                job.dests[g].progress.set_phase(DestPhase::WaitingDevice);
            }
            job.set_phase(JobPhase::WaitingDevice, l!("Waiting for destination drives", "Đang chờ ổ đích rảnh"));
            let permit = self.scheduler.acquire(&devices, &job.cancel, || {
                job.set_text(l!("Waiting for another job to finish writing", "Đang chờ job khác ghi xong vào ổ đích"));
            });
            if permit.is_none() {
                return;
            }
            job.set_phase(JobPhase::Copying, l!("Copying and verifying from the card", "Đang chép và verify từ thẻ"));
            for &g in &primaries {
                if let Err(msg) = self.make_dirs(job, g) {
                    if let Ok(mut i) = job.dests[g].info.lock() {
                        i.error = Some(msg.clone());
                    }
                    job.error(format!("{}: {msg}", job.dests[g].spec.volume_name));
                }
            }
            let targets: Vec<DestTarget> = primaries
                .iter()
                .map(|&g| DestTarget {
                    roll_dir: job.dests[g].spec.roll_dir.clone(),
                    defer_verify: job.dests[g].spec.kind.defer_verify(),
                    progress: job.dests[g].progress.clone(),
                })
                .collect();
            let has_known = known.iter().any(|h| h.is_some());
            let listener = Arc::new(JobListener {
                job: job.clone(),
                journal: journal.clone(),
                run: run.clone(),
                map: primaries.clone(),
                record_source_hashes: true,
            });
            let req = TransferRequest {
                source_root: PathBuf::from(&job.request.source),
                files: files.clone(),
                work,
                dests: targets,
                algo: job.request.algo,
                expected: if has_known { Some(Arc::new(known)) } else { None },
                cancel: job.cancel.clone(),
                source_progress: job.source_progress.clone(),
                listener,
                faults: self.faults.lock().ok().and_then(|f| f.primary.clone()),
                chunk_size: DEFAULT_CHUNK,
                retry_failed: true,
                stall_timeout: self.stall_timeout(),
            };
            let out = transfer::run_transfer(&req);
            drop(permit);

            for e in out.source_errors.values() {
                job.error(e.clone());
            }
            if (!out.source_errors.is_empty() || out.source_lost || !out.source_unstable.is_empty())
                && let Ok(mut r) = run.lock()
            {
                r.source_problem = true;
            }
            if out.source_lost {
                job.error(l!(
                    "The source card was removed or stopped responding.",
                    "Thẻ nguồn đã bị rút hoặc ngừng phản hồi giữa chừng."
                ));
            }
            for fi in &out.source_unstable {
                job.error(l!(
                    "The card returned different data on two reads: {}. The reader or the card is faulty; use another reader.",
                    "Thẻ trả về dữ liệu khác nhau ở hai lần đọc: {}. Đầu đọc hoặc thẻ có vấn đề, hãy đổi đầu đọc.",
                    files[*fi].rel
                ));
            }
            for (l, &g) in primaries.iter().enumerate() {
                self.apply_outcome(job, journal, g, &out.dests[l]);
            }
            if out.cancelled || job.cancelled() {
                for &g in &primaries {
                    job.dests[g].progress.set_phase(DestPhase::Cancelled);
                }
                return;
            }
        }

        if job.request.source_verify && !job.cancelled() {
            self.verify_source(job, run, journal);
        }
        for &g in &primaries {
            self.seal_dest(job, g, run, journal, Process::Transfer, Action::Original, None);
        }
        self.update_safe_to_eject(job, run, journal);
    }

    fn stall_timeout(&self) -> Duration {
        self.faults.lock().ok().and_then(|f| f.stall_timeout).unwrap_or(DEFAULT_STALL)
    }

    fn verify_source(&self, job: &Arc<Job>, run: &Mutex<RunState>, journal: &Journal) {
        job.set_phase(JobPhase::SourceVerify, l!("Re-reading the card to check the source", "Đang đọc lại thẻ để kiểm tra nguồn"));
        let hashes = run.lock().map(|r| r.hashes.clone()).unwrap_or_default();
        let root = PathBuf::from(&job.request.source);
        for (fi, f) in job.files.iter().enumerate() {
            if job.cancelled() {
                return;
            }
            let Some(expected) = hashes[fi].clone() else { continue };
            job.set_text(l!("Checking source: {}", "Kiểm tra lại nguồn: {}", f.rel));
            let path = root.join(&f.rel);
            let algo = job.request.algo;
            let cancel = job.cancel.clone();
            let progress = job.source_progress.clone();
            let res = crate::util::supervised(self.stall_timeout(), move |b| {
                let mut buf = AlignedBuf::new(DEFAULT_CHUNK);
                transfer::cold_hash_file_tracked(&path, algo, &mut buf, Some(&progress.bytes_read), &cancel, Some(b))
            });
            let problem = match res {
                Some(Ok((h, _))) if h == expected => None,
                Some(Ok((h, _))) => Some(l!(
                    "SOURCE CHECK FAILED for {}: second read gave {}, first read {}. Copies of this file cannot be trusted.",
                    "Kiểm tra nguồn THẤT BẠI ở {}: lần đọc thứ hai ra {}, lần đầu {}. Các bản sao của file này không đáng tin.",
                    f.rel,
                    h,
                    expected
                )),
                Some(Err(e)) => {
                    if job.cancelled() {
                        return;
                    }
                    Some(l!("Cannot re-read {} on the card: {}", "Không đọc lại được {} trên thẻ: {}", f.rel, transfer::describe_io(&e)))
                }
                None => Some(l!("The card stopped responding while re-reading {}", "Thẻ ngừng phản hồi khi đọc lại {}", f.rel)),
            };
            if let Some(msg) = problem {
                journal.log(&msg);
                job.error(msg);
                if let Ok(mut r) = run.lock() {
                    r.source_problem = true;
                    for d in 0..job.dests.len() {
                        r.verified[d][fi] = None;
                    }
                }
            }
        }
    }

    /// If every file is verified on destination `g`, set folder dates and
    /// write ASC MHL (+ legacy MHL). Marks the destination complete. All disk
    /// work runs supervised so a vanished drive cannot block the job.
    #[allow(clippy::too_many_arguments)]
    fn seal_dest(
        &self,
        job: &Job,
        g: usize,
        run: &Mutex<RunState>,
        journal: &Journal,
        process: Process,
        action: Action,
        history_from: Option<PathBuf>,
    ) {
        let d = &job.dests[g];
        let scan = &job.prepared.scan;
        let (records, missing) = {
            let r = run.lock().expect("run");
            let mut recs = Vec::with_capacity(scan.files.len());
            let mut missing = 0usize;
            for (fi, f) in scan.files.iter().enumerate() {
                match &r.verified[g][fi] {
                    Some((h, t)) => recs.push(FileRecord {
                        rel: f.rel.clone(),
                        size: f.size,
                        mtime: f.mtime,
                        hash: h.clone(),
                        action,
                        hash_date: t.with_timezone(&Local),
                    }),
                    None => missing += 1,
                }
            }
            (recs, missing)
        };
        let has_error = d.info.lock().map(|i| i.error.is_some()).unwrap_or(true);
        if missing > 0 || has_error {
            d.progress.set_phase(if job.cancelled() { DestPhase::Cancelled } else { DestPhase::Failed });
            if let Ok(mut i) = d.info.lock()
                && i.error.is_none()
            {
                i.error = Some(l!("{} file(s) not verified", "{} file chưa verify", missing));
            }
            return;
        }
        let meta = job.request.meta.clone();
        let creator = CreatorInfo {
            hostname: ascmhl::hostname(),
            tool_name: TOOL_NAME.into(),
            tool_version: self.cfg.tool_version.clone(),
            author_name: Some(meta.dit_name.clone()),
            author_role: Some("DIT".into()),
            location: Some(job.request.location.clone()),
            comment: Some(format!(
                "{} · {} · {} · Roll {} · Source: {}",
                meta.project_name, meta.shoot_day, meta.camera_id, meta.roll_id, job.prepared.layout.source_label
            )),
        };
        let dirs: Vec<(String, SystemTime)> = scan.dirs.iter().map(|x| (x.rel.clone(), x.mtime)).collect();
        let started: DateTime<Utc> =
            job.state.lock().map(|s| s.started_local.with_timezone(&Utc)).unwrap_or_else(|_| Utc::now());
        let roll = d.spec.roll_dir.clone();
        let algo = job.request.algo;
        let tool = format!("{TOOL_NAME} {}", self.cfg.tool_version);
        let sealed = crate::util::supervised(self.stall_timeout(), move |b| -> Result<(PathBuf, Result<Option<PathBuf>, String>), String> {
            let _g = b.enter();
            let mut sorted = dirs.clone();
            sorted.sort_by_key(|(rel, _)| std::cmp::Reverse(rel.matches('/').count()));
            for (rel, t) in &sorted {
                let _ = filetime::set_file_mtime(roll.join(rel), filetime::FileTime::from_system_time(*t));
            }
            if let Some(from) = &history_from {
                ascmhl::copy_history(from, &roll)
                    .map_err(|e| l!("Cannot copy the ASC MHL history: {}", "Không chép được lịch sử ASC MHL: {}", e))?;
            }
            let ignore = ascmhl::default_ignore_patterns();
            let generation = Generation {
                root: &roll,
                algo,
                process,
                creator: &creator,
                files: &records,
                dirs: &dirs,
                ignore: &ignore,
                created: Local::now(),
            };
            let mhl = ascmhl::write_generation(&generation)
                .map_err(|e| l!("Cannot write ASC MHL: {}", "Không ghi được ASC MHL: {}", transfer::describe_io(&e)))?;
            let user = tools::username();
            let host = ascmhl::hostname();
            let entries: Vec<LegacyEntry> = records
                .iter()
                .filter(|r| !ascmhl::is_ignored(&r.rel))
                .map(|r| LegacyEntry { rel: &r.rel, size: r.size, mtime: r.mtime, hash: &r.hash, hash_date: r.hash_date.with_timezone(&Utc) })
                .collect();
            let legacy = legacy_mhl::write(
                &roll,
                &meta.roll_id,
                algo,
                &LegacyCreator { name: &meta.dit_name, username: &user, hostname: &host, tool: &tool, start: started, finish: Utc::now() },
                &entries,
            )
            .map_err(|e| e.to_string());
            Ok((mhl, legacy))
        });
        let (mhl, legacy) = match sealed {
            Some(Ok(x)) => x,
            Some(Err(msg)) => {
                self.dest_fail(job, g, journal, msg);
                return;
            }
            None => {
                self.dest_fail(job, g, journal, l!("Drive stopped responding while writing the MHL", "Ổ ngừng phản hồi khi ghi MHL"));
                return;
            }
        };
        let legacy = match legacy {
            Ok(p) => p.map(|p| p.display().to_string()),
            Err(e) => {
                job.warn(l!("{}: cannot write MHL v1.1 ({})", "{}: không ghi được MHL v1.1 ({})", d.spec.volume_name, e));
                None
            }
        };
        let mhl_s = mhl.display().to_string();
        if let Ok(mut i) = d.info.lock() {
            i.complete = true;
            i.mhl = Some(mhl_s.clone());
            i.legacy = legacy.clone();
        }
        d.progress.set_phase(DestPhase::Done);
        journal.event(&JournalEvent::DestComplete { d: g, mhl: Some(mhl_s), legacy });
        journal.log(&format!("[{}] COMPLETE: {} files verified, ASC MHL written", d.spec.volume_name, job.files.len()));
    }

    fn dest_fail(&self, job: &Job, g: usize, journal: &Journal, msg: String) {
        let d = &job.dests[g];
        d.progress.set_phase(DestPhase::Failed);
        if let Ok(mut i) = d.info.lock() {
            i.error = Some(msg.clone());
        }
        journal.event(&JournalEvent::DestFailed { d: g, reason: msg.clone() });
        job.error(format!("{}: {msg}", d.spec.volume_name));
    }

    fn run_cascades(&self, job: &Arc<Job>, run: &Arc<Mutex<RunState>>, journal: &Arc<Journal>) {
        let cascades: Vec<usize> = job
            .dests
            .iter()
            .enumerate()
            .filter(|(_, d)| d.spec.tier == Tier::Cascade && !d.info.lock().map(|i| i.complete).unwrap_or(false))
            .map(|(i, _)| i)
            .collect();
        if cascades.is_empty() {
            return;
        }
        let source = job
            .dests
            .iter()
            .enumerate()
            .filter(|(_, d)| d.spec.tier == Tier::Primary && d.info.lock().map(|i| i.complete).unwrap_or(false))
            .min_by_key(|(i, d)| (if d.spec.kind == DeviceKind::Ssd { 0 } else { 1 }, *i))
            .map(|(i, _)| i);
        let Some(src) = source else {
            for &g in &cascades {
                self.dest_fail(job, g, journal, l!("No verified Tier 1 copy to cascade from", "Không có bản Tier 1 nào đã verify để thác đổ"));
            }
            return;
        };
        let expected: Vec<Option<String>> = run.lock().map(|r| r.hashes.clone()).unwrap_or_default();
        if expected.iter().any(|h| h.is_none()) {
            for &g in &cascades {
                self.dest_fail(
                    job,
                    g,
                    journal,
                    l!("Original card hashes missing, cannot cascade safely", "Thiếu hash gốc của thẻ, không thể thác đổ an toàn"),
                );
            }
            return;
        }
        let expected = Arc::new(expected);
        job.set_phase(
            JobPhase::Cascading,
            l!("Cascading from {} to {} drive(s)", "Thác đổ từ {} sang {} ổ", job.dests[src].spec.volume_name, cascades.len()),
        );
        let src_root = job.dests[src].spec.roll_dir.clone();
        let files = job.files.clone();
        let n = files.len();
        std::thread::scope(|s| {
            for &g in &cascades {
                let expected = expected.clone();
                let src_root = src_root.clone();
                let files = files.clone();
                s.spawn(move || {
                    let d = &job.dests[g];
                    let (work, prior_bytes, prior_files) = {
                        let r = run.lock().expect("run");
                        let work: Vec<Vec<usize>> =
                            (0..n).map(|fi| if r.verified[g][fi].is_none() { vec![0] } else { vec![] }).collect();
                        let done: Vec<usize> = (0..n).filter(|fi| r.verified[g][*fi].is_some()).collect();
                        (work, done.iter().map(|fi| files[*fi].size).sum::<u64>(), done.len() as u64)
                    };
                    if let Ok(mut info) = d.info.lock() {
                        info.need_bytes = (0..n).filter(|fi| !work[*fi].is_empty()).map(|fi| files[fi].size).sum();
                        info.prior_bytes = prior_bytes;
                        info.prior_files = prior_files;
                    }
                    d.progress.set_phase(DestPhase::WaitingDevice);
                    let permit = self.scheduler.acquire(&[(d.spec.device_id.clone(), d.spec.kind)], &job.cancel, || {});
                    if permit.is_none() {
                        d.progress.set_phase(DestPhase::Cancelled);
                        return;
                    }
                    if work.iter().any(|w| !w.is_empty()) {
                        if let Err(msg) = self.make_dirs(job, g) {
                            self.dest_fail(job, g, journal, msg);
                            return;
                        }
                        let listener = Arc::new(JobListener {
                            job: job.clone(),
                            journal: journal.clone(),
                            run: run.clone(),
                            map: vec![g],
                            record_source_hashes: false,
                        });
                        let req = TransferRequest {
                            source_root: src_root.clone(),
                            files: files.clone(),
                            work,
                            dests: vec![DestTarget {
                                roll_dir: d.spec.roll_dir.clone(),
                                defer_verify: d.spec.kind.defer_verify(),
                                progress: d.progress.clone(),
                            }],
                            algo: job.request.algo,
                            expected: Some(expected),
                            cancel: job.cancel.clone(),
                            source_progress: Arc::new(SourceProgress::default()),
                            listener,
                            faults: self.faults.lock().ok().and_then(|f| f.cascade.clone()),
                            chunk_size: DEFAULT_CHUNK,
                            retry_failed: true,
                            stall_timeout: self.stall_timeout(),
                        };
                        let out = transfer::run_transfer(&req);
                        for e in out.source_errors.values() {
                            job.error(l!("Reading source copy {}: {}", "Đọc bản nguồn {}: {}", job.dests[src].spec.volume_name, e));
                        }
                        self.apply_outcome(job, journal, g, &out.dests[0]);
                        if out.cancelled || job.cancelled() {
                            d.progress.set_phase(DestPhase::Cancelled);
                            return;
                        }
                    }
                    drop(permit);
                    self.seal_dest(job, g, run, journal, Process::Transfer, Action::Verified, Some(src_root));
                    self.update_safe_to_eject(job, run, journal);
                });
            }
        });
    }

    fn run_report(&self, job: &Arc<Job>, run: &Mutex<RunState>, journal: &Journal) {
        job.set_phase(JobPhase::Reporting, l!("Building the camera report", "Đang tạo báo cáo camera"));
        let complete: Vec<usize> = job
            .dests
            .iter()
            .enumerate()
            .filter(|(_, d)| d.info.lock().map(|i| i.complete).unwrap_or(false))
            .map(|(i, _)| i)
            .collect();
        let Some(&best) = complete
            .iter()
            .min_by_key(|&&i| (job.dests[i].spec.tier != Tier::Primary, job.dests[i].spec.kind != DeviceKind::Ssd, i))
        else {
            return;
        };
        let hashes = run.lock().map(|r| r.hashes.clone()).unwrap_or_default();
        let copies: Vec<ReportCopy> = job
            .dests
            .iter()
            .map(|d| {
                let (complete, error, prior_files, mhl) = {
                    let info = d.info.lock().expect("info");
                    (info.complete, info.error.clone(), info.prior_files, info.mhl.clone())
                };
                let verified = d.progress.files_verified.load(Ordering::Relaxed) + prior_files;
                ReportCopy {
                    label: d.spec.volume_name.clone(),
                    path: d.spec.roll_dir.display().to_string(),
                    device: d.spec.device_id.clone(),
                    kind: d.spec.kind.label().to_string(),
                    tier: d.spec.tier.label().to_string(),
                    status: if complete { "Verified".into() } else { error.unwrap_or_else(|| "Incomplete".into()) },
                    ok: complete,
                    verified_files: if complete { job.files.len() } else { verified as usize },
                    mhl: mhl.as_ref().and_then(|m| Path::new(m).file_name().map(|n| n.to_string_lossy().into_owned())),
                }
            })
            .collect();
        let (started, safe_at, issues) = {
            let st = job.state.lock().expect("state");
            (st.started_local, st.safe_to_eject_at.map(|t| t.with_timezone(&Local)), st.errors.clone())
        };
        let total_clips = job.files.iter().filter(|f| report::is_clip(&f.rel)).count();
        job.report_progress.store(0, Ordering::Relaxed);
        let input = ReportInput {
            meta: &job.request.meta,
            algo: job.request.algo,
            copy_root: &job.dests[best].spec.roll_dir,
            files: &job.files,
            hashes: &hashes,
            source_label: &job.prepared.layout.source_label,
            card_vendor: job.prepared.layout.card_vendor.as_deref(),
            copies,
            started,
            finished: Local::now(),
            safe_to_eject_at: safe_at,
            issues,
            branding: &job.request.branding,
            tool: &format!("{TOOL_NAME} {}", self.cfg.tool_version),
            cancel: &job.cancel,
            progress: Some(&job.report_progress),
        };
        job.set_text(l!("Building the camera report ({} clips)", "Đang tạo báo cáo ({} clip)", total_clips));
        let (html, pdf, pdf_err) = match report::build(&input, job.request.report_pdf) {
            Ok(x) => x,
            Err(e) => {
                job.warn(l!("Cannot build the report: {}", "Không tạo được báo cáo: {}", e));
                return;
            }
        };
        if let Some(e) = pdf_err {
            job.warn(e);
        }
        let mut first_html = None;
        let mut first_pdf = None;
        for &g in &complete {
            let dir = job.dests[g].spec.root.join(&job.prepared.layout.report_rel);
            let (html, pdf) = (html.clone(), pdf.clone());
            let res = crate::util::supervised(self.stall_timeout(), move |b| -> std::io::Result<(PathBuf, Option<PathBuf>)> {
                let _g = b.enter();
                fs::create_dir_all(&dir)?;
                let h = dir.join(html.file_name().unwrap_or_default());
                copy_synced(&html, &h)?;
                let p = match &pdf {
                    Some(p) => {
                        let dst = dir.join(p.file_name().unwrap_or_default());
                        copy_synced(p, &dst)?;
                        Some(dst)
                    }
                    None => None,
                };
                Ok((h, p))
            });
            match res {
                Some(Ok((h, p))) => {
                    first_html.get_or_insert(h.display().to_string());
                    if let Some(p) = p {
                        first_pdf.get_or_insert(p.display().to_string());
                    }
                }
                Some(Err(e)) => job.warn(l!("{}: cannot write the report ({})", "{}: không ghi được báo cáo ({})", job.dests[g].spec.volume_name, e)),
                None => job.warn(l!("{}: drive not responding, report not written", "{}: ổ không phản hồi, chưa ghi được báo cáo", job.dests[g].spec.volume_name)),
            }
        }
        if let Some(parent) = html.parent() {
            let _ = fs::remove_dir_all(parent);
        }
        journal.log(&format!("Report: {}", first_html.as_deref().unwrap_or("-")));
        if let Ok(mut st) = job.state.lock() {
            st.report_path = first_html;
            st.report_pdf_path = first_pdf;
        }
    }

    fn finish(&self, job: &Job, run: &Mutex<RunState>, journal: &Journal) {
        let all_complete = job.dests.iter().all(|d| d.info.lock().map(|i| i.complete).unwrap_or(false));
        let copies = job.complete_devices();
        let source_problem = run.lock().map(|r| r.source_problem).unwrap_or(true);
        let has_errors = job.state.lock().map(|s| !s.errors.is_empty()).unwrap_or(true);
        let (phase, text) = if job.cancelled() {
            (
                JobPhase::Cancelled,
                l!("Cancelled. Verified files are kept; you can Resume.", "Đã huỷ. Các file đã verify vẫn giữ nguyên; có thể bấm Tiếp tục."),
            )
        } else if all_complete && !has_errors && !source_problem {
            (JobPhase::Completed, l!("Complete: {} verified copies", "Hoàn tất: {} bản đã verify", copies))
        } else if copies > 0 && !source_problem {
            (JobPhase::CompletedWithErrors, l!("Finished with errors: {} complete copies", "Xong nhưng có lỗi: {} bản hoàn chỉnh", copies))
        } else {
            (JobPhase::Failed, l!("Failed: no complete copy yet", "Thất bại: chưa có bản sao hoàn chỉnh"))
        };
        for d in &job.dests {
            if !d.info.lock().map(|i| i.complete).unwrap_or(false) && d.progress.phase() != DestPhase::Failed {
                d.progress.set_phase(if job.cancelled() { DestPhase::Cancelled } else { DestPhase::Failed });
            }
        }
        if let Ok(mut st) = job.state.lock() {
            st.phase = phase;
            st.status_text = text.clone();
            st.finished_at = Some(Utc::now());
            st.resumable = !all_complete;
        }
        journal.event(&JournalEvent::Finished { status: format!("{phase:?}"), t: Utc::now() });
        journal.log(&format!("END: {phase:?}: {text}"));
        self.record_history(job);
        let _ = journal.write_json("final.json", &job.snapshot());
        match phase {
            JobPhase::Completed => self.notify(Sound::Done, &l!("Done {}", "Xong {}", job.label()), &text),
            JobPhase::Cancelled => {}
            _ => self.notify(Sound::Error, &l!("Problem with {}", "Lỗi {}", job.label()), &text),
        }
    }
}

/// True if the folder holds anything other than macOS housekeeping files.
fn dir_has_entries(p: &Path) -> bool {
    fs::read_dir(p)
        .map(|mut rd| {
            rd.any(|e| {
                e.map(|e| !scan::is_os_junk_name(&e.file_name().to_string_lossy())).unwrap_or(true)
            })
        })
        .unwrap_or(false)
}

fn create_dirs(roll_dir: &Path, dirs: &[String]) -> std::io::Result<()> {
    fs::create_dir_all(roll_dir)?;
    for d in dirs {
        fs::create_dir_all(roll_dir.join(d))?;
    }
    Ok(())
}

fn remove_partials(root: &Path) {
    for e in walkdir::WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() && e.file_name().to_string_lossy().ends_with(PARTIAL_SUFFIX) {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn copy_synced(from: &Path, to: &Path) -> std::io::Result<()> {
    let tmp = to.with_file_name(format!(".{}.tmp", to.file_name().and_then(|n| n.to_str()).unwrap_or("report")));
    fs::copy(from, &tmp)?;
    if let Ok(f) = fs::File::open(&tmp) {
        let _ = crate::darwin_io::full_fsync(&f);
    }
    fs::rename(&tmp, to)
}

pub fn human_bytes(b: u64) -> String {
    let f = b as f64;
    if f >= 1e12 {
        format!("{:.2} TB", f / 1e12)
    } else if f >= 1e9 {
        format!("{:.2} GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1} MB", f / 1e6)
    } else {
        format!("{:.0} KB", f / 1e3)
    }
}
