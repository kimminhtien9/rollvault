//! ASC MHL v2.0 writer (urn:ASC:MHL:v2.0), following the ASC reference
//! implementation (`ascmhl` 1.2): generation manifests in `<root>/ascmhl/`,
//! a chain file referencing each manifest by its C4 ID, and content /
//! structure directory hashes.

use crate::darwin_io;
use crate::hashing::{HashAlgo, c4_of_file, hash_bytes, hash_of_hash_list};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const ASCMHL_DIR: &str = "ascmhl";
pub const CHAIN_FILE: &str = "ascmhl_chain.xml";

/// Reference defaults, plus:
/// * `._*` AppleDouble files that macOS creates next to every file on
///   exFAT/FAT volumes to store extended attributes (e.g. the protected
///   `com.apple.provenance` attribute, which cannot be removed);
/// * root-level legacy `.mhl` files (MHL v1.1 for older tools).
pub fn default_ignore_patterns() -> Vec<String> {
    [".DS_Store", "ascmhl", "ascmhl/", "._*", "/*.mhl"].iter().map(|s| s.to_string()).collect()
}

/// Whether a root-relative path is excluded by [`default_ignore_patterns`].
pub fn is_ignored(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == ".DS_Store"
        || name.starts_with("._")
        || rel.split('/').any(|c| c == ASCMHL_DIR)
        || (!rel.contains('/') && rel.ends_with(".mhl"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Process {
    InPlace,
    Transfer,
}

impl Process {
    fn as_str(self) -> &'static str {
        match self {
            Process::InPlace => "in-place",
            Process::Transfer => "transfer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Original,
    Verified,
    Failed,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Action::Original => "original",
            Action::Verified => "verified",
            Action::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreatorInfo {
    pub hostname: String,
    pub tool_name: String,
    pub tool_version: String,
    pub author_name: Option<String>,
    pub author_role: Option<String>,
    pub location: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub rel: String,
    pub size: u64,
    pub mtime: SystemTime,
    pub hash: String,
    pub action: Action,
    pub hash_date: DateTime<Local>,
}

pub struct Generation<'a> {
    pub root: &'a Path,
    pub algo: HashAlgo,
    pub process: Process,
    pub creator: &'a CreatorInfo,
    pub files: &'a [FileRecord],
    /// Every directory below the root (including empty ones) and its mtime.
    pub dirs: &'a [(String, SystemTime)],
    pub ignore: &'a [String],
    pub created: DateTime<Local>,
}

pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => out.push(c),
        }
    }
    out
}

pub fn iso_local(t: SystemTime) -> String {
    DateTime::<Local>::from(t).to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn iso_local_micros(t: &DateTime<Local>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// Content and structure hash per directory (key "" is the root), computed
/// exactly like `ascmhl.hasher.DirectoryHashContext`.
pub fn directory_hashes(
    algo: HashAlgo,
    files: &[(String, String)],
    dirs: &[String],
) -> BTreeMap<String, (String, String)> {
    fn parent_and_name(rel: &str) -> (String, String) {
        match rel.rfind('/') {
            Some(i) => (rel[..i].to_string(), rel[i + 1..].to_string()),
            None => (String::new(), rel.to_string()),
        }
    }
    let mut all_dirs: BTreeSet<String> = BTreeSet::new();
    all_dirs.insert(String::new());
    let add_with_parents = |set: &mut BTreeSet<String>, rel: &str| {
        let mut cur = rel.to_string();
        loop {
            set.insert(cur.clone());
            match cur.rfind('/') {
                Some(i) => cur.truncate(i),
                None => {
                    set.insert(String::new());
                    break;
                }
            }
        }
    };
    for d in dirs {
        add_with_parents(&mut all_dirs, d);
    }
    for (rel, _) in files {
        let (p, _) = parent_and_name(rel);
        if !p.is_empty() {
            add_with_parents(&mut all_dirs, &p);
        }
    }

    let mut child_files: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (rel, h) in files {
        let (p, n) = parent_and_name(rel);
        child_files.entry(p).or_default().push((n, h.clone()));
    }
    let mut child_dirs: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for d in all_dirs.iter().filter(|d| !d.is_empty()) {
        let (p, n) = parent_and_name(d);
        child_dirs.entry(p).or_default().push((n, d.clone()));
    }

    let mut order: Vec<&String> = all_dirs.iter().collect();
    order.sort_by_key(|d| std::cmp::Reverse(if d.is_empty() { 0 } else { d.matches('/').count() + 1 }));

    let mut result: BTreeMap<String, (String, String)> = BTreeMap::new();
    for dir in order {
        let mut content: Vec<String> = Vec::new();
        let mut structure: Vec<String> = Vec::new();
        for (name, h) in child_files.get(dir).map(Vec::as_slice).unwrap_or(&[]) {
            content.push(h.clone());
            let mut data = name.as_bytes().to_vec();
            data.extend(hex::decode(h).expect("hex hash"));
            structure.push(hash_bytes(algo, &data));
        }
        for (name, full) in child_dirs.get(dir).map(Vec::as_slice).unwrap_or(&[]) {
            let (c, st) = result.get(full).expect("child computed first").clone();
            content.push(c);
            let mut data = name.as_bytes().to_vec();
            data.extend(hex::decode(&st).expect("hex hash"));
            structure.push(hash_bytes(algo, &data));
        }
        let c = hash_of_hash_list(algo, &mut content);
        let s = hash_of_hash_list(algo, &mut structure);
        result.insert(dir.clone(), (c, s));
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainEntry {
    pub sequence: u32,
    pub path: String,
    pub c4: String,
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let a = s.find(open)? + open.len();
    let b = s[a..].find(close)? + a;
    Some(&s[a..b])
}

/// Parse an `ascmhl_chain.xml` (the subset of XML written by ASC tools).
pub fn read_chain(ascmhl_dir: &Path) -> io::Result<Vec<ChainEntry>> {
    let p = ascmhl_dir.join(CHAIN_FILE);
    if !p.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&p)?;
    let mut out = Vec::new();
    for block in text.split("<hashlist").skip(1) {
        let Some(end) = block.find("</hashlist>") else { continue };
        let block = &block[..end];
        let seq = between(block, "sequencenr=\"", "\"").and_then(|v| v.parse().ok()).unwrap_or(0);
        let path = between(block, "<path>", "</path>").unwrap_or_default();
        let c4 = between(block, "<c4>", "</c4>").unwrap_or_default();
        out.push(ChainEntry {
            sequence: seq,
            path: unescape_xml(path.trim()),
            c4: c4.trim().to_string(),
        });
    }
    Ok(out)
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn write_atomic(path: &Path, data: &[u8], replace: bool) -> io::Result<()> {
    let tmp = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("mhl")
    ));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        darwin_io::full_fsync(&f)?;
    }
    let res = if replace { fs::rename(&tmp, path) } else { darwin_io::rename_no_replace(&tmp, path) };
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res?;
    if let Some(parent) = path.parent() {
        let _ = darwin_io::fsync_dir(parent);
    }
    Ok(())
}

fn hash_element(indent: &str, tag: &str, value: &str, action: Option<Action>, date: &str) -> String {
    let action = action.map(|a| format!(" action=\"{}\"", a.as_str())).unwrap_or_default();
    format!("{indent}<{tag}{action} hashdate=\"{date}\">{value}</{tag}>\n")
}

/// Render a generation manifest as XML.
pub fn render_manifest(g: &Generation<'_>) -> String {
    let tag = g.algo.ascmhl_name();
    let files: Vec<&FileRecord> = g.files.iter().filter(|f| !is_ignored(&f.rel)).collect();
    let file_pairs: Vec<(String, String)> = files.iter().map(|f| (f.rel.clone(), f.hash.clone())).collect();
    let dir_rels: Vec<String> = g.dirs.iter().filter(|(d, _)| !is_ignored(d)).map(|(d, _)| d.clone()).collect();
    let dir_hashes = directory_hashes(g.algo, &file_pairs, &dir_rels);
    let dir_mtimes: BTreeMap<&str, SystemTime> = g.dirs.iter().map(|(d, t)| (d.as_str(), *t)).collect();
    let now = iso_local_micros(&g.created);

    let mut x = String::with_capacity(1024 + files.len() * 300);
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str("<hashlist version=\"2.0\" xmlns=\"urn:ASC:MHL:v2.0\">\n");
    x.push_str("  <creatorinfo>\n");
    x.push_str(&format!(
        "    <creationdate>{}</creationdate>\n",
        g.created.to_rfc3339_opts(SecondsFormat::Secs, false)
    ));
    x.push_str(&format!("    <hostname>{}</hostname>\n", escape_xml(&g.creator.hostname)));
    x.push_str(&format!(
        "    <tool version=\"{}\">{}</tool>\n",
        escape_xml(&g.creator.tool_version),
        escape_xml(&g.creator.tool_name)
    ));
    if let Some(name) = g.creator.author_name.as_deref().filter(|n| !n.trim().is_empty()) {
        let role = g
            .creator
            .author_role
            .as_deref()
            .map(|r| format!(" role=\"{}\"", escape_xml(r)))
            .unwrap_or_default();
        x.push_str(&format!("    <author{role}>{}</author>\n", escape_xml(name)));
    }
    if let Some(loc) = g.creator.location.as_deref().filter(|n| !n.trim().is_empty()) {
        x.push_str(&format!("    <location>{}</location>\n", escape_xml(loc)));
    }
    if let Some(c) = g.creator.comment.as_deref().filter(|n| !n.trim().is_empty()) {
        x.push_str(&format!("    <comment>{}</comment>\n", escape_xml(c)));
    }
    x.push_str("  </creatorinfo>\n");

    x.push_str("  <processinfo>\n");
    x.push_str(&format!("    <process>{}</process>\n", g.process.as_str()));
    let (root_c, root_s) = dir_hashes.get("").cloned().expect("root hash");
    x.push_str("    <roothash>\n      <content>\n");
    x.push_str(&hash_element("        ", tag, &root_c, None, &now));
    x.push_str("      </content>\n      <structure>\n");
    x.push_str(&hash_element("        ", tag, &root_s, None, &now));
    x.push_str("      </structure>\n    </roothash>\n");
    x.push_str("    <ignore>\n");
    for p in g.ignore {
        x.push_str(&format!("      <pattern>{}</pattern>\n", escape_xml(p)));
    }
    x.push_str("    </ignore>\n");
    x.push_str("  </processinfo>\n");

    let has_entries = !files.is_empty() || dir_hashes.len() > 1;
    if has_entries {
        x.push_str("  <hashes>\n");
        for f in &files {
            x.push_str("    <hash>\n");
            x.push_str(&format!(
                "      <path size=\"{}\" lastmodificationdate=\"{}\">{}</path>\n",
                f.size,
                iso_local(f.mtime),
                escape_xml(&f.rel)
            ));
            x.push_str(&hash_element("      ", tag, &f.hash, Some(f.action), &iso_local_micros(&f.hash_date)));
            x.push_str("    </hash>\n");
        }
        for (dir, (c, s)) in dir_hashes.iter().filter(|(d, _)| !d.is_empty()) {
            let mtime = dir_mtimes
                .get(dir.as_str())
                .map(|t| format!(" lastmodificationdate=\"{}\"", iso_local(*t)))
                .unwrap_or_default();
            x.push_str("    <directoryhash>\n");
            x.push_str(&format!("      <path{mtime}>{}</path>\n", escape_xml(dir)));
            x.push_str("      <content>\n");
            x.push_str(&hash_element("        ", tag, c, None, &now));
            x.push_str("      </content>\n      <structure>\n");
            x.push_str(&hash_element("        ", tag, s, None, &now));
            x.push_str("      </structure>\n");
            x.push_str("    </directoryhash>\n");
        }
        x.push_str("  </hashes>\n");
    }
    x.push_str("</hashlist>\n");
    x
}

fn render_chain(entries: &[ChainEntry]) -> String {
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str("<ascmhldirectory xmlns=\"urn:ASC:MHL:DIRECTORY:v2.0\">\n");
    for e in entries {
        x.push_str(&format!("  <hashlist sequencenr=\"{}\">\n", e.sequence));
        x.push_str(&format!("    <path>{}</path>\n", escape_xml(&e.path)));
        x.push_str(&format!("    <c4>{}</c4>\n", e.c4));
        x.push_str("  </hashlist>\n");
    }
    x.push_str("</ascmhldirectory>\n");
    x
}

fn next_generation(ascmhl_dir: &Path, chain: &[ChainEntry]) -> u32 {
    let mut max = chain.iter().map(|e| e.sequence).max().unwrap_or(0);
    if let Ok(rd) = fs::read_dir(ascmhl_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".mhl") || name.starts_with("._") {
                continue;
            }
            let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 4
                && let Ok(n) = digits.parse::<u32>()
            {
                max = max.max(n);
            }
        }
    }
    max + 1
}

/// Write a new generation manifest and append it to the chain. Returns the
/// manifest path.
pub fn write_generation(g: &Generation<'_>) -> io::Result<PathBuf> {
    let dir = g.root.join(ASCMHL_DIR);
    fs::create_dir_all(&dir)?;
    let mut chain = read_chain(&dir)?;
    let seq = next_generation(&dir, &chain);
    let folder = g.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
    let stamp = g.created.with_timezone(&Utc).format("%Y-%m-%d_%H%M%SZ");
    let name = format!("{seq:04}_{folder}_{stamp}.mhl");
    let path = dir.join(&name);
    write_atomic(&path, render_manifest(g).as_bytes(), false)?;
    let c4 = c4_of_file(&path)?;
    chain.push(ChainEntry { sequence: seq, path: name, c4 });
    write_atomic(&dir.join(CHAIN_FILE), render_chain(&chain).as_bytes(), true)?;
    Ok(path)
}

/// Copy an existing ASC MHL history (manifests + chain) to another root, so
/// the history travels with a cascaded copy. Chain integrity is verified.
pub fn copy_history(from_root: &Path, to_root: &Path) -> io::Result<()> {
    let src = from_root.join(ASCMHL_DIR);
    let chain = read_chain(&src)?;
    for e in &chain {
        let actual = c4_of_file(&src.join(&e.path))?;
        if actual != e.c4 {
            return Err(io::Error::other(l!(
                "Source ASC MHL history is damaged: {} has C4 {} instead of {}",
                "Lịch sử ASC MHL nguồn bị hỏng: {} có C4 {} thay vì {}",
                e.path,
                actual,
                e.c4
            )));
        }
    }
    let dst = to_root.join(ASCMHL_DIR);
    fs::create_dir_all(&dst)?;
    for e in chain.iter().map(|e| e.path.clone()).chain(std::iter::once(CHAIN_FILE.to_string())) {
        let data = fs::read(src.join(&e))?;
        write_atomic(&dst.join(&e), &data, true)?;
    }
    Ok(())
}

pub fn hostname() -> String {
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: buffer is large enough and NUL-terminated by gethostname.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) };
    if r == 0 {
        let s = darwin_io::c_chars_to_string(&buf);
        if !s.is_empty() {
            return s;
        }
    }
    "localhost".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_hashes_match_reference_tool_output() {
        // Values produced by `ascmhl create -h xxh64` (Python reference) for:
        //   EMPTYDIR/ (empty), PRIVATE/M4ROOT/CLIP/C0001.MP4 = "hello", empty.bin = ""
        let files = vec![
            ("PRIVATE/M4ROOT/CLIP/C0001.MP4".to_string(), "26c7827d889f6da3".to_string()),
            ("empty.bin".to_string(), "ef46db3751d8e999".to_string()),
        ];
        let dirs = vec!["EMPTYDIR".to_string(), "PRIVATE".into(), "PRIVATE/M4ROOT".into(), "PRIVATE/M4ROOT/CLIP".into()];
        let h = directory_hashes(HashAlgo::Xxh64, &files, &dirs);
        assert_eq!(h["EMPTYDIR"], ("ef46db3751d8e999".into(), "ef46db3751d8e999".into()));
        assert_eq!(h["PRIVATE/M4ROOT/CLIP"], ("c07c10338a825a5d".into(), "7977796842ee1ce2".into()));
        assert_eq!(h["PRIVATE/M4ROOT"], ("a3c976a379ca9241".into(), "0d6ab554dc557c76".into()));
        assert_eq!(h["PRIVATE"], ("0d939b32d481ea65".into(), "5c36bff4576be261".into()));
        assert_eq!(h[""], ("c84aaaaca8d0965e".into(), "22b651514cf41581".into()));
    }

    #[test]
    fn ignore_rules() {
        assert!(is_ignored(".DS_Store"));
        assert!(is_ignored("A/.DS_Store"));
        assert!(is_ignored("ascmhl/0001_x.mhl"));
        assert!(is_ignored("PRIVATE/M4ROOT/CLIP/._C0001.MP4"));
        assert!(is_ignored("._PRIVATE"));
        assert!(is_ignored("A001_2026.mhl"));
        assert!(!is_ignored("CLIPS/A001.mhl"));
        assert!(!is_ignored("PRIVATE/M4ROOT/CLIP/C0001.MP4"));
    }

    #[test]
    fn chain_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let entries = vec![
            ChainEntry { sequence: 1, path: "0001_A001_x.mhl".into(), c4: "c4abc".into() },
            ChainEntry { sequence: 2, path: "0002_A001_y.mhl".into(), c4: "c4def".into() },
        ];
        fs::write(d.path().join(CHAIN_FILE), render_chain(&entries)).unwrap();
        assert_eq!(read_chain(d.path()).unwrap(), entries);
    }
}
