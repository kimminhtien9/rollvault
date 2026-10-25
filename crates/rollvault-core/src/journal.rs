//! Per-job crash journal: `<data>/jobs/<id>/` holds the request, the scanned
//! file list, an append-only event log (one JSON line per verified file) and
//! a human-readable log. Enough to resume after a crash or power loss.

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "e", rename_all = "snake_case")]
pub enum JournalEvent {
    SourceHash { f: usize, h: String },
    Verified { d: usize, f: usize, h: String, t: DateTime<Utc> },
    DestComplete { d: usize, mhl: Option<String>, legacy: Option<String> },
    DestFailed { d: usize, reason: String },
    SafeToEject { t: DateTime<Utc> },
    Finished { status: String, t: DateTime<Utc> },
}

pub struct Journal {
    dir: PathBuf,
    events: Mutex<File>,
    log: Mutex<File>,
}

pub fn job_dir(data_dir: &Path, job_id: &str) -> PathBuf {
    data_dir.join("jobs").join(job_id)
}

impl Journal {
    pub fn open(data_dir: &Path, job_id: &str) -> io::Result<Journal> {
        let dir = job_dir(data_dir, job_id);
        fs::create_dir_all(&dir)?;
        let events_path = dir.join("events.jsonl");
        // A crash can leave a torn last line; start the next event on a new line.
        if let Ok(bytes) = fs::read(&events_path)
            && bytes.last().is_some_and(|b| *b != b'\n')
        {
            let mut f = OpenOptions::new().append(true).open(&events_path)?;
            f.write_all(b"\n")?;
        }
        let events = OpenOptions::new().create(true).append(true).open(&events_path)?;
        let log = OpenOptions::new().create(true).append(true).open(dir.join("log.txt"))?;
        Ok(Journal { dir, events: Mutex::new(events), log: Mutex::new(log) })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn write_json<T: Serialize>(&self, name: &str, value: &T) -> io::Result<()> {
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()?;
        }
        fs::rename(tmp, self.dir.join(name))
    }

    pub fn event(&self, ev: &JournalEvent) {
        if let (Ok(mut f), Ok(line)) = (self.events.lock(), serde_json::to_string(ev)) {
            let _ = writeln!(f, "{line}");
            let _ = f.sync_data();
        }
    }

    pub fn log(&self, msg: &str) {
        if let Ok(mut f) = self.log.lock() {
            let _ = writeln!(f, "{} {msg}", Local::now().format("%Y-%m-%d %H:%M:%S%.3f"));
        }
    }
}

pub fn read_json<T: DeserializeOwned>(data_dir: &Path, job_id: &str, name: &str) -> io::Result<T> {
    let data = fs::read(job_dir(data_dir, job_id).join(name))?;
    serde_json::from_slice(&data).map_err(io::Error::other)
}

/// Events in order. A torn final line (crash mid-write) is ignored.
pub fn read_events(data_dir: &Path, job_id: &str) -> io::Result<Vec<JournalEvent>> {
    let f = File::open(job_dir(data_dir, job_id).join("events.jsonl"))?;
    let mut out = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if let Ok(ev) = serde_json::from_str::<JournalEvent>(&line) {
            out.push(ev);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_roundtrip_and_tolerate_torn_line() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(d.path(), "job1").unwrap();
        j.event(&JournalEvent::SourceHash { f: 0, h: "ab".into() });
        let t = Utc::now();
        j.event(&JournalEvent::Verified { d: 1, f: 0, h: "ab".into(), t });
        drop(j);
        let p = job_dir(d.path(), "job1").join("events.jsonl");
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "{{\"e\":\"verified\",\"d\":").unwrap();
        let evs = read_events(d.path(), "job1").unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1], JournalEvent::Verified { d: 1, f: 0, h: "ab".into(), t });
        // Re-opening after the torn write must not glue the next event onto it.
        let j = Journal::open(d.path(), "job1").unwrap();
        j.event(&JournalEvent::SourceHash { f: 1, h: "cd".into() });
        drop(j);
        let evs = read_events(d.path(), "job1").unwrap();
        assert_eq!(evs.len(), 3);
    }
}
