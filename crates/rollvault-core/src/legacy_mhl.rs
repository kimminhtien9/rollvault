//! Legacy MHL v1.1 (mediahashlist.org) for tools that predate ASC MHL.
//! Only MD5 and xxHash64 (big-endian) exist in that format.

use crate::ascmhl::escape_xml;
use crate::darwin_io;
use crate::hashing::HashAlgo;
use chrono::{DateTime, SecondsFormat, Utc};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct LegacyCreator<'a> {
    pub name: &'a str,
    pub username: &'a str,
    pub hostname: &'a str,
    pub tool: &'a str,
    pub start: DateTime<Utc>,
    pub finish: DateTime<Utc>,
}

pub struct LegacyEntry<'a> {
    pub rel: &'a str,
    pub size: u64,
    pub mtime: SystemTime,
    pub hash: &'a str,
    pub hash_date: DateTime<Utc>,
}

fn utc(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn render(algo: HashAlgo, creator: &LegacyCreator<'_>, entries: &[LegacyEntry<'_>]) -> Option<String> {
    let tag = algo.legacy_mhl_tag()?;
    let mut x = String::with_capacity(512 + entries.len() * 260);
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str("<hashlist version=\"1.1\">\n");
    x.push_str("  <creatorinfo>\n");
    if !creator.name.trim().is_empty() {
        x.push_str(&format!("    <name>{}</name>\n", escape_xml(creator.name)));
    }
    x.push_str(&format!("    <username>{}</username>\n", escape_xml(creator.username)));
    x.push_str(&format!("    <hostname>{}</hostname>\n", escape_xml(creator.hostname)));
    x.push_str(&format!("    <tool>{}</tool>\n", escape_xml(creator.tool)));
    x.push_str(&format!("    <startdate>{}</startdate>\n", utc(creator.start)));
    x.push_str(&format!("    <finishdate>{}</finishdate>\n", utc(creator.finish)));
    x.push_str("  </creatorinfo>\n");
    for e in entries {
        x.push_str("  <hash>\n");
        x.push_str(&format!("    <file>{}</file>\n", escape_xml(e.rel)));
        x.push_str(&format!("    <size>{}</size>\n", e.size));
        x.push_str(&format!(
            "    <lastmodificationdate>{}</lastmodificationdate>\n",
            utc(DateTime::<Utc>::from(e.mtime))
        ));
        x.push_str(&format!("    <{tag}>{}</{tag}>\n", e.hash));
        x.push_str(&format!("    <hashdate>{}</hashdate>\n", utc(e.hash_date)));
        x.push_str("  </hash>\n");
    }
    x.push_str("</hashlist>\n");
    Some(x)
}

/// Write `<roll>_<YYYY-MM-DD_HHMMSS>.mhl` in the root of the roll folder.
/// Returns `None` when the algorithm has no MHL v1.1 representation.
pub fn write(
    root: &Path,
    roll_id: &str,
    algo: HashAlgo,
    creator: &LegacyCreator<'_>,
    entries: &[LegacyEntry<'_>],
) -> io::Result<Option<PathBuf>> {
    let Some(xml) = render(algo, creator, entries) else { return Ok(None) };
    let stamp = creator.finish.format("%Y-%m-%d_%H%M%S");
    let roll = crate::preset::sanitize_component(roll_id);
    let path = root.join(format!("{roll}_{stamp}.mhl"));
    let tmp = root.join(format!(".{roll}_{stamp}.mhl.tmp"));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(xml.as_bytes())?;
        darwin_io::full_fsync(&f)?;
    }
    if let Err(e) = darwin_io::rename_no_replace(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let _ = darwin_io::fsync_dir(root);
    Ok(Some(path))
}
