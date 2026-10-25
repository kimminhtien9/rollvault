//! Device scheduler shared by all running jobs:
//! * write permits per physical device (an HDD or a NAS gets one writer at a
//!   time, an SSD a few), acquired all-or-nothing so jobs cannot deadlock;
//! * free-space reservations so two concurrent jobs cannot both "fit" into
//!   the same free space;
//! * roll-folder claims so two jobs never write the same folder.

use crate::volumes::DeviceKind;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
struct State {
    in_use: HashMap<String, usize>,
    reserved: HashMap<String, u64>,
    rolls: HashSet<PathBuf>,
}

#[derive(Default)]
pub struct DeviceScheduler {
    state: Mutex<State>,
    cv: Condvar,
}

pub struct Permit {
    sched: Arc<DeviceScheduler>,
    devices: Vec<String>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut st) = self.sched.state.lock() {
            for d in &self.devices {
                if let Some(n) = st.in_use.get_mut(d) {
                    *n = n.saturating_sub(1);
                }
            }
        }
        self.sched.cv.notify_all();
    }
}

pub struct Reservation {
    sched: Arc<DeviceScheduler>,
    device: String,
    bytes: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut st) = self.sched.state.lock()
            && let Some(r) = st.reserved.get_mut(&self.device)
        {
            *r = r.saturating_sub(self.bytes);
        }
    }
}

pub struct RollClaim {
    sched: Arc<DeviceScheduler>,
    path: PathBuf,
}

impl Drop for RollClaim {
    fn drop(&mut self) {
        if let Ok(mut st) = self.sched.state.lock() {
            st.rolls.remove(&self.path);
        }
    }
}

fn normalize(p: &Path) -> PathBuf {
    PathBuf::from(p.to_string_lossy().to_lowercase())
}

impl DeviceScheduler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Wait until every device has a free writer slot, then take them all at
    /// once. Returns `None` if cancelled while waiting.
    pub fn acquire(
        self: &Arc<Self>,
        devices: &[(String, DeviceKind)],
        cancel: &AtomicBool,
        mut on_wait: impl FnMut(),
    ) -> Option<Permit> {
        let mut unique: Vec<(String, DeviceKind)> = Vec::new();
        for (d, k) in devices {
            if !unique.iter().any(|(u, _)| u == d) {
                unique.push((d.clone(), *k));
            }
        }
        let mut st = self.state.lock().ok()?;
        let mut notified = false;
        loop {
            let free = unique
                .iter()
                .all(|(d, k)| st.in_use.get(d).copied().unwrap_or(0) < k.concurrent_writers());
            if free {
                for (d, _) in &unique {
                    *st.in_use.entry(d.clone()).or_default() += 1;
                }
                return Some(Permit { sched: self.clone(), devices: unique.into_iter().map(|(d, _)| d).collect() });
            }
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            if !notified {
                notified = true;
                drop(st);
                on_wait();
                st = self.state.lock().ok()?;
                continue;
            }
            st = self.cv.wait_timeout(st, Duration::from_millis(250)).ok()?.0;
        }
    }

    pub fn reserved_bytes(&self, device: &str) -> u64 {
        self.state.lock().ok().and_then(|s| s.reserved.get(device).copied()).unwrap_or(0)
    }

    pub fn reserve(self: &Arc<Self>, device: &str, bytes: u64) -> Reservation {
        if let Ok(mut st) = self.state.lock() {
            *st.reserved.entry(device.to_string()).or_default() += bytes;
        }
        Reservation { sched: self.clone(), device: device.to_string(), bytes }
    }

    pub fn is_roll_claimed(&self, roll_dir: &Path) -> bool {
        self.state.lock().map(|s| s.rolls.contains(&normalize(roll_dir))).unwrap_or(false)
    }

    pub fn claim_roll(self: &Arc<Self>, roll_dir: &Path) -> Option<RollClaim> {
        let key = normalize(roll_dir);
        let mut st = self.state.lock().ok()?;
        if !st.rolls.insert(key.clone()) {
            return None;
        }
        Some(RollClaim { sched: self.clone(), path: key })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdd_gets_one_writer_at_a_time() {
        let s = DeviceScheduler::new();
        let cancel = AtomicBool::new(false);
        let a = s.acquire(&[("disk5".into(), DeviceKind::Hdd)], &cancel, || {}).unwrap();
        let s2 = s.clone();
        let t = std::thread::spawn(move || {
            let c = AtomicBool::new(false);
            let started = std::time::Instant::now();
            let _b = s2.acquire(&[("disk5".into(), DeviceKind::Hdd)], &c, || {}).unwrap();
            started.elapsed()
        });
        std::thread::sleep(Duration::from_millis(300));
        drop(a);
        let waited = t.join().unwrap();
        assert!(waited >= Duration::from_millis(250), "second writer did not wait: {waited:?}");
    }

    #[test]
    fn ssd_allows_parallel_writers_and_cancel_stops_waiting() {
        let s = DeviceScheduler::new();
        let cancel = AtomicBool::new(false);
        let _a = s.acquire(&[("disk4".into(), DeviceKind::Ssd)], &cancel, || {}).unwrap();
        let _b = s.acquire(&[("disk4".into(), DeviceKind::Ssd)], &cancel, || {}).unwrap();
        let _h = s.acquire(&[("disk9".into(), DeviceKind::Hdd)], &cancel, || {}).unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert!(s.acquire(&[("disk9".into(), DeviceKind::Hdd)], &cancel, || {}).is_none());
    }

    #[test]
    fn reservations_and_roll_claims() {
        let s = DeviceScheduler::new();
        {
            let _r = s.reserve("disk4", 100);
            let _r2 = s.reserve("disk4", 50);
            assert_eq!(s.reserved_bytes("disk4"), 150);
        }
        assert_eq!(s.reserved_bytes("disk4"), 0);
        let c = s.claim_roll(Path::new("/Volumes/SSD/P/A001")).unwrap();
        assert!(s.claim_roll(Path::new("/volumes/ssd/p/a001")).is_none(), "case-insensitive");
        drop(c);
        assert!(s.claim_roll(Path::new("/Volumes/SSD/P/A001")).is_some());
    }
}
