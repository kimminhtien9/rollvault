//! Small helpers for calls that can block forever on a misbehaving volume
//! (a wedged USB bridge, a stalled FSKit driver, or a pending macOS privacy
//! prompt all make `open()` sleep in the kernel).

use std::io;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Run `f` on a helper thread and give up after `timeout`. When it times
/// out the helper thread is left behind (it is stuck in the kernel and
/// cannot be cancelled) and `None` is returned.
pub fn run_with_timeout<T, F>(timeout: Duration, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("rv-timeout".into()).spawn(move || {
        let _ = tx.send(f());
    });
    if spawned.is_err() {
        return None;
    }
    rx.recv_timeout(timeout).ok()
}

/// `Command::output()` with a deadline; the child is killed on timeout.
pub fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> io::Result<Output> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(mut s) = stdout {
            let _ = io::Read::read_to_end(&mut s, &mut v);
        }
        v
    });
    let err_reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(mut s) = stderr {
            let _ = io::Read::read_to_end(&mut s, &mut v);
        }
        v
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(Output { status, stdout, stderr })
}

fn epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64 + 1
}

/// Marks when a thread entered a potentially blocking system call, so a
/// supervisor can tell "slow but working" from "stuck in the kernel"
/// (a yanked USB drive, a hung NAS).
#[derive(Default)]
pub struct Busy(std::sync::atomic::AtomicU64);

pub struct BusyGuard<'a>(&'a Busy);

impl Busy {
    pub fn enter(&self) -> BusyGuard<'_> {
        self.0.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
        BusyGuard(self)
    }

    /// How long the current call has been running (zero when idle).
    pub fn stuck_for(&self) -> Duration {
        let start = self.0.load(std::sync::atomic::Ordering::Relaxed);
        if start == 0 { Duration::ZERO } else { Duration::from_millis(now_ms().saturating_sub(start)) }
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.0.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Run `f` on its own thread. Returns `None` if `f` stays inside one busy
/// section longer than `stall` (the thread is then abandoned).
pub fn supervised<T, F>(stall: Duration, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(&Busy) -> T + Send + 'static,
{
    let busy = std::sync::Arc::new(Busy::default());
    let b2 = busy.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rv-supervised".into())
        .spawn(move || {
            let _ = tx.send(f(&b2));
        })
        .ok()?;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(v) => return Some(v),
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if busy.stuck_for() > stall {
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_returns_none_for_slow_work() {
        let r = run_with_timeout(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
            1
        });
        assert!(r.is_none());
        assert_eq!(run_with_timeout(Duration::from_secs(2), || 7), Some(7));
    }

    #[test]
    fn supervised_abandons_stuck_work_only() {
        let slow_but_progressing = supervised(Duration::from_millis(150), |b| {
            for _ in 0..6 {
                let _g = b.enter();
                std::thread::sleep(Duration::from_millis(60));
            }
            7
        });
        assert_eq!(slow_but_progressing, Some(7));
        let stuck = supervised(Duration::from_millis(150), |b| {
            let _g = b.enter();
            std::thread::sleep(Duration::from_secs(3));
            1
        });
        assert_eq!(stuck, None);
    }

    #[test]
    fn command_timeout_kills_child() {
        let start = Instant::now();
        let r = output_with_timeout(Command::new("/bin/sleep").arg("5"), Duration::from_millis(200));
        assert!(r.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        let ok = output_with_timeout(Command::new("/bin/echo").arg("hi"), Duration::from_secs(5)).unwrap();
        assert_eq!(String::from_utf8_lossy(&ok.stdout).trim(), "hi");
    }
}
