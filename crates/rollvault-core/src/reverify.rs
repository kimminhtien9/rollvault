//! Re-verify a folder against its ASC MHL history (e.g. a NAS copy after a
//! remount, or an archive drive months later). Every recorded file is read
//! back cold and compared; the result is appended as a new `in-place`
//! generation with `verified` / `failed` actions.

use crate::ascmhl::{self, Action, CreatorInfo, FileRecord, Generation, Process};
use crate::darwin_io::AlignedBuf;
use crate::hashing::HashAlgo;
use crate::transfer::{DEFAULT_CHUNK, cold_hash_file, describe_io};
use chrono::Local;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq)]
pub struct RecordedHash {
    pub rel: String,
    pub size: Option<u64>,
    pub algo: HashAlgo,
    pub hash: String,
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let a = tag.find(&key)? + key.len();
    let b = tag[a..].find('"')? + a;
    Some(tag[a..b].to_string())
}

/// File hashes recorded in one ASC MHL manifest (directory hashes ignored).
pub fn read_manifest(path: &Path) -> std::io::Result<Vec<RecordedHash>> {
    let xml = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for block in xml.split("<hash>").skip(1) {
        let Some(end) = block.find("</hash>") else { continue };
        let block = &block[..end];
        let Some(ps) = block.find("<path") else { continue };
        let Some(pe) = block[ps..].find('>') else { continue };
        let tag = &block[ps..ps + pe];
        let Some(close) = block.find("</path>") else { continue };
        let rel = unescape(block[ps + pe + 1..close].trim());
        let size = attr(tag, "size").and_then(|s| s.parse().ok());
        for algo in HashAlgo::ALL {
            let open = format!("<{}", algo.ascmhl_name());
            let mut search = &block[close..];
            while let Some(i) = search.find(&open) {
                let rest = &search[i + open.len()..];
                // Avoid matching "<xxh3" inside "<xxh128".
                if rest.starts_with([' ', '>']) {
                    let gt = rest.find('>').unwrap_or(0);
                    let close_tag = format!("</{}>", algo.ascmhl_name());
                    if let Some(ce) = rest.find(&close_tag) {
                        out.push(RecordedHash { rel: rel.clone(), size, algo, hash: rest[gt + 1..ce].trim().to_lowercase() });
                    }
                    break;
                }
                search = rest;
            }
        }
    }
    Ok(out)
}

/// Latest known hash per file across all generations of the history.
pub fn recorded_hashes(root: &Path) -> Result<BTreeMap<String, RecordedHash>, String> {
    let dir = root.join(ascmhl::ASCMHL_DIR);
    let chain = ascmhl::read_chain(&dir).map_err(|e| e.to_string())?;
    if chain.is_empty() {
        return Err(l!("{} has no ASC MHL history", "{} không có lịch sử ASC MHL", root.display()));
    }
    let mut map = BTreeMap::new();
    let mut ordered = chain.clone();
    ordered.sort_by_key(|e| e.sequence);
    for e in ordered {
        let p = dir.join(&e.path);
        let actual = crate::hashing::c4_of_file(&p).map_err(|err| l!("Cannot read {}: {}", "Không đọc được {}: {}", e.path, err))?;
        if actual != e.c4 {
            return Err(l!("Manifest {} was modified (C4 does not match the chain)", "Manifest {} đã bị sửa (C4 không khớp chain)", e.path));
        }
        for h in read_manifest(&p).map_err(|err| err.to_string())? {
            map.insert(h.rel.clone(), h);
        }
    }
    Ok(map)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct VerifyOutcome {
    pub root: String,
    pub files_checked: usize,
    pub verified: usize,
    pub failed: Vec<String>,
    pub missing: Vec<String>,
    pub unrecorded: Vec<String>,
    pub manifest: Option<String>,
    pub cancelled: bool,
}

impl VerifyOutcome {
    pub fn ok(&self) -> bool {
        !self.cancelled && self.failed.is_empty() && self.missing.is_empty()
    }
}

#[derive(Default)]
pub struct VerifyProgress {
    pub bytes_done: AtomicU64,
    pub bytes_total: AtomicU64,
    pub files_done: AtomicU64,
    pub current: std::sync::Mutex<String>,
}

/// Re-hash every recorded file, append a generation, report differences.
pub fn verify_folder(
    root: &Path,
    tool_version: &str,
    author: &str,
    cancel: &AtomicBool,
    progress: &VerifyProgress,
) -> Result<VerifyOutcome, String> {
    let recorded = recorded_hashes(root)?;
    let mut out = VerifyOutcome { root: root.display().to_string(), ..Default::default() };
    let total: u64 = recorded.values().filter_map(|r| r.size).sum();
    progress.bytes_total.store(total, Ordering::Relaxed);

    let mut records: Vec<FileRecord> = Vec::new();
    let mut algo_used: Option<HashAlgo> = None;
    let mut buf = AlignedBuf::new(DEFAULT_CHUNK);
    for (rel, rec) in &recorded {
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            return Ok(out);
        }
        if let Ok(mut c) = progress.current.lock() {
            c.clone_from(rel);
        }
        let path = root.join(rel);
        let md = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                out.missing.push(rel.clone());
                continue;
            }
        };
        out.files_checked += 1;
        match cold_hash_file(&path, rec.algo, &mut buf, Some(&progress.bytes_done), cancel) {
            Ok((h, size)) => {
                let good = h == rec.hash && rec.size.is_none_or(|s| s == size);
                if good {
                    out.verified += 1;
                } else {
                    out.failed.push(format!("{rel}: {} ≠ {}", h, rec.hash));
                }
                if algo_used.is_none_or(|a| a == rec.algo) {
                    algo_used = Some(rec.algo);
                    records.push(FileRecord {
                        rel: rel.clone(),
                        size,
                        mtime: md.modified().unwrap_or(UNIX_EPOCH),
                        hash: h,
                        action: if good { Action::Verified } else { Action::Failed },
                        hash_date: Local::now(),
                    });
                }
            }
            Err(e) => {
                if cancel.load(Ordering::Relaxed) {
                    out.cancelled = true;
                    return Ok(out);
                }
                out.failed.push(l!("{}: unreadable ({})", "{}: không đọc được ({})", rel, describe_io(&e)));
            }
        }
        progress.files_done.fetch_add(1, Ordering::Relaxed);
    }

    // Completeness: files on disk that the history does not know about.
    let known: BTreeSet<&String> = recorded.keys().collect();
    let mut dirs: Vec<(String, SystemTime)> = Vec::new();
    for e in walkdir::WalkDir::new(root).min_depth(1).into_iter().filter_entry(|e| {
        let n = e.file_name().to_string_lossy();
        !(n == ascmhl::ASCMHL_DIR && e.depth() == 1)
    }) {
        let Ok(e) = e else { continue };
        let Ok(rel) = e.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if ascmhl::is_ignored(&rel) {
            continue;
        }
        if e.file_type().is_dir() {
            dirs.push((rel, e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(UNIX_EPOCH)));
        } else if e.file_type().is_file() && !known.contains(&rel) {
            out.unrecorded.push(rel);
        }
    }

    if let Some(algo) = algo_used {
        let creator = CreatorInfo {
            hostname: ascmhl::hostname(),
            tool_name: crate::job::TOOL_NAME.into(),
            tool_version: tool_version.into(),
            author_name: Some(author.to_string()).filter(|a| !a.is_empty()),
            author_role: Some("DIT".into()),
            location: None,
            comment: Some(format!(
                "Re-verify: {} verified, {} failed, {} missing",
                out.verified,
                out.failed.len(),
                out.missing.len()
            )),
        };
        let ignore = ascmhl::default_ignore_patterns();
        let generation = Generation {
            root,
            algo,
            process: Process::InPlace,
            creator: &creator,
            files: &records,
            dirs: &dirs,
            ignore: &ignore,
            created: Local::now(),
        };
        match ascmhl::write_generation(&generation) {
            Ok(p) => out.manifest = Some(p.display().to_string()),
            Err(e) => out.failed.push(l!("Cannot write the new generation: {}", "Không ghi được generation mới: {}", describe_io(&e))),
        }
    }
    Ok(out)
}
