//! External helpers: ffmpeg discovery (GUI apps do not inherit the shell
//! PATH, so Homebrew locations are probed explicitly), headless-Chrome PDF
//! export, sleep prevention, sounds and notifications.

use crate::util::output_with_timeout;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TOOL_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"];

/// Locate a command-line tool. `RV_<NAME>` overrides (e.g. `RV_FFMPEG`).
pub fn find_tool(name: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(format!("RV_{}", name.to_uppercase())) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut dirs: Vec<PathBuf> = TOOL_DIRS.iter().map(PathBuf::from).collect();
    if let Ok(path) = std::env::var("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

pub fn ffmpeg() -> Option<PathBuf> {
    find_tool("ffmpeg")
}

pub fn ffprobe() -> Option<PathBuf> {
    find_tool("ffprobe")
}

/// A Chromium-based browser able to print HTML to PDF headlessly.
pub fn find_chromium() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let apps = [
        "Google Chrome.app/Contents/MacOS/Google Chrome",
        "Chromium.app/Contents/MacOS/Chromium",
        "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "Brave Browser.app/Contents/MacOS/Brave Browser",
    ];
    for base in ["/Applications".to_string(), format!("{home}/Applications")] {
        for a in apps {
            let p = Path::new(&base).join(a);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn file_url(path: &Path) -> String {
    let mut s = String::from("file://");
    for b in path.to_string_lossy().as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(*b as char),
            other => s.push_str(&format!("%{other:02X}")),
        }
    }
    s
}

/// Print an HTML file to PDF with headless Chrome. Both paths should be on
/// the internal disk (Chrome would otherwise trigger its own privacy prompt
/// for removable volumes).
pub fn html_to_pdf(html: &Path, pdf: &Path) -> Result<(), String> {
    let chrome = find_chromium().ok_or_else(|| l!("Google Chrome not found for PDF export", "Không tìm thấy Google Chrome để xuất PDF"))?;
    let profile = std::env::temp_dir().join(format!("rollvault-chrome-{}", uuid::Uuid::new_v4()));
    let mut cmd = Command::new(chrome);
    cmd.arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        .arg("--no-pdf-header-footer")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", pdf.display()))
        .arg(file_url(html));
    let res = output_with_timeout(&mut cmd, Duration::from_secs(180));
    let _ = std::fs::remove_dir_all(&profile);
    let out = res.map_err(|e| l!("Chrome could not export the PDF: {}", "Chrome không xuất được PDF: {}", e))?;
    if pdf.is_file() && std::fs::metadata(pdf).map(|m| m.len() > 0).unwrap_or(false) {
        Ok(())
    } else {
        Err(l!(
            "Chrome could not create the PDF (code {:?}): {}",
            "Chrome không tạo được PDF (mã {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("")
        ))
    }
}

/// Keeps the Mac awake while at least one token is alive (via
/// `caffeinate -i -w <pid>`, which also exits if RollVault crashes).
#[derive(Default)]
pub struct SleepGuard {
    inner: Mutex<(usize, Option<Child>)>,
}

pub struct SleepToken(Arc<SleepGuard>);

impl SleepGuard {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn acquire(self: &Arc<Self>) -> SleepToken {
        if let Ok(mut g) = self.inner.lock() {
            g.0 += 1;
            if g.1.is_none() {
                g.1 = Command::new("/usr/bin/caffeinate")
                    .args(["-i", "-m", "-w", &std::process::id().to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok();
            }
        }
        SleepToken(self.clone())
    }

    pub fn active(&self) -> bool {
        self.inner.lock().map(|g| g.0 > 0).unwrap_or(false)
    }
}

impl Drop for SleepToken {
    fn drop(&mut self) {
        if let Ok(mut g) = self.0.inner.lock() {
            g.0 = g.0.saturating_sub(1);
            if g.0 == 0
                && let Some(mut c) = g.1.take()
            {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Sound {
    SafeToEject,
    Done,
    Error,
}

pub fn play_sound(s: Sound) {
    let file = match s {
        Sound::SafeToEject => "/System/Library/Sounds/Glass.aiff",
        Sound::Done => "/System/Library/Sounds/Hero.aiff",
        Sound::Error => "/System/Library/Sounds/Basso.aiff",
    };
    let _ = Command::new("/usr/bin/afplay")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn notify(title: &str, body: &str) {
    let script = format!(
        "display notification {} with title {}",
        applescript_string(body),
        applescript_string(title)
    );
    let _ = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub fn reveal_in_finder(path: &Path) -> Result<(), String> {
    Command::new("/usr/bin/open").arg("-R").arg(path).status().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn open_path(path: &Path) -> Result<(), String> {
    Command::new("/usr/bin/open").arg(path).status().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn username() -> String {
    std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| "dit".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_are_escaped() {
        assert_eq!(file_url(Path::new("/Volumes/SSD 1TB/a b.html")), "file:///Volumes/SSD%201TB/a%20b.html");
        assert_eq!(file_url(Path::new("/tmp/Tết.html")), "file:///tmp/T%C3%A1%BA%BFt.html".replace("%C3%A1%BA%BF", "%E1%BA%BF"));
    }

    #[test]
    fn finds_system_tools() {
        assert!(find_tool("ls").is_some());
        assert!(find_tool("definitely-not-a-tool-xyz").is_none());
    }

    #[test]
    fn sleep_guard_refcounts() {
        let g = SleepGuard::new();
        let a = g.acquire();
        let b = g.acquire();
        assert!(g.active());
        drop(a);
        assert!(g.active());
        drop(b);
        assert!(!g.active());
    }
}
