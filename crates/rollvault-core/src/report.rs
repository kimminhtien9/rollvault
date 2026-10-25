//! Camera report (Foolcat-style): per-clip metadata from ffprobe and camera
//! sidecars, three thumbnails per clip, rendered as one self-contained HTML
//! file (print CSS for PDF).

use crate::ascmhl::escape_xml as esc;
use crate::hashing::HashAlgo;
use crate::preset::RollMeta;
use crate::scan::SourceFile;
use crate::tools;
use crate::util::output_with_timeout;
use base64::Engine as _;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const VIDEO_EXT: &[&str] =
    &["mp4", "mov", "mxf", "mts", "m2ts", "avi", "mkv", "braw", "r3d", "crm", "ari", "insv", "mpg", "m4v"];
const AUDIO_EXT: &[&str] = &["wav", "bwf", "mp3", "m4a", "aac", "aif", "aiff", "flac"];
/// Formats ffmpeg cannot decode without vendor SDKs.
const NO_DECODER_EXT: &[&str] = &["braw", "r3d", "crm", "ari"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportBranding {
    pub company: String,
    /// `data:image/...;base64,...`
    pub logo_data_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportCopy {
    pub label: String,
    pub path: String,
    pub device: String,
    pub kind: String,
    pub tier: String,
    pub status: String,
    pub ok: bool,
    pub verified_files: usize,
    pub mhl: Option<String>,
}

pub struct ReportInput<'a> {
    pub meta: &'a RollMeta,
    pub algo: HashAlgo,
    /// Root of a verified copy to read clips from.
    pub copy_root: &'a Path,
    pub files: &'a [SourceFile],
    pub hashes: &'a [Option<String>],
    pub source_label: &'a str,
    pub card_vendor: Option<&'a str>,
    pub copies: Vec<ReportCopy>,
    pub started: DateTime<Local>,
    pub finished: DateTime<Local>,
    pub safe_to_eject_at: Option<DateTime<Local>>,
    pub issues: Vec<String>,
    pub branding: &'a ReportBranding,
    pub tool: &'a str,
    pub cancel: &'a AtomicBool,
    pub progress: Option<&'a AtomicUsize>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClipInfo {
    pub rel: String,
    pub name: String,
    pub size: u64,
    pub hash: String,
    pub is_audio: bool,
    pub is_proxy: bool,
    pub codec: String,
    pub profile: String,
    pub resolution: String,
    pub fps: String,
    pub duration: f64,
    pub bitrate_mbps: f64,
    pub pix_fmt: String,
    pub color: String,
    pub audio: String,
    pub timecode: String,
    pub camera: String,
    pub gamma: String,
    pub recorded: String,
    pub thumbs: Vec<String>,
    pub note: String,
}

fn ext_of(rel: &str) -> String {
    Path::new(rel).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()
}

pub fn is_clip(rel: &str) -> bool {
    let e = ext_of(rel);
    VIDEO_EXT.contains(&e.as_str()) || AUDIO_EXT.contains(&e.as_str())
}

fn is_proxy(rel: &str) -> bool {
    let up = rel.to_uppercase();
    // Sony proxies live in .../SUB/ and end with S03; DJI uses .LRF (not a clip ext).
    up.contains("/SUB/") || up.contains("/PROXY/") || up.ends_with("S03.MP4")
}

fn attr<'a>(xml: &'a str, tag_start: &str, attr: &str) -> Option<&'a str> {
    let i = xml.find(tag_start)?;
    let rest = &xml[i..];
    let end = rest.find('>')?;
    let tag = &rest[..end];
    let key = format!("{attr}=\"");
    let a = tag.find(&key)? + key.len();
    let b = tag[a..].find('"')? + a;
    Some(&tag[a..b])
}

/// Sony LTC values are stored as BCD bytes "ffssmmhh" with flag bits.
pub fn sony_ltc_to_tc(value: &str) -> Option<String> {
    if value.len() != 8 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&value[i..i + 2], 16).ok();
    let ff = byte(0)? & 0x3F;
    let ss = byte(2)? & 0x7F;
    let mm = byte(4)? & 0x7F;
    let hh = byte(6)? & 0x3F;
    Some(format!("{hh:02x}:{mm:02x}:{ss:02x}:{ff:02x}"))
}

#[derive(Default)]
struct SonyXml {
    timecode: Option<String>,
    model: Option<String>,
    gamma: Option<String>,
    fps: Option<String>,
}

fn read_sony_xml(clip_path: &Path) -> Option<SonyXml> {
    let stem = clip_path.file_stem()?.to_string_lossy().into_owned();
    let dir = clip_path.parent()?;
    let candidates = [format!("{stem}M01.XML"), format!("{stem}M01.xml")];
    let path = candidates.iter().map(|c| dir.join(c)).find(|p| p.is_file())?;
    let xml = std::fs::read_to_string(path).ok()?;
    let mut out = SonyXml::default();
    if let Some(v) = attr(&xml, "<LtcChange ", "value") {
        out.timecode = sony_ltc_to_tc(v);
    }
    if let Some(m) = attr(&xml, "<Device ", "modelName") {
        out.model = Some(m.to_string());
    }
    if let Some(i) = xml.find("name=\"CaptureGammaEquation\"") {
        let rest = &xml[i..];
        if let Some(v) = attr(rest, "name=\"CaptureGammaEquation\"", "value") {
            out.gamma = Some(v.to_string());
        }
    }
    if let Some(f) = attr(&xml, "<VideoFrame ", "captureFps") {
        out.fps = Some(f.to_string());
    }
    Some(out)
}

fn parse_rate(r: &str) -> Option<f64> {
    let (n, d) = r.split_once('/')?;
    let n: f64 = n.parse().ok()?;
    let d: f64 = d.parse().ok()?;
    if d > 0.0 && n > 0.0 { Some(n / d) } else { None }
}

fn fmt_fps(f: f64) -> String {
    if (f - f.round()).abs() < 0.005 { format!("{:.0}", f) } else { format!("{:.3}", f) }
}

fn probe(ffprobe: &Path, path: &Path, clip: &mut ClipInfo) {
    let out = output_with_timeout(
        Command::new(ffprobe)
            .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
            .arg(path),
        Duration::from_secs(30),
    );
    let Ok(out) = out else {
        clip.note = l!("ffprobe timed out", "ffprobe hết thời gian");
        return;
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        clip.note = l!("ffprobe could not read the file", "ffprobe không đọc được file");
        return;
    };
    let fmt = &json["format"];
    clip.duration = fmt["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0);
    if let Some(b) = fmt["bit_rate"].as_str().and_then(|b| b.parse::<f64>().ok()) {
        clip.bitrate_mbps = b / 1_000_000.0;
    }
    let tags = &fmt["tags"];
    if let Some(tc) = tags["timecode"].as_str() {
        clip.timecode = tc.to_string();
    }
    if let Some(t) = tags["creation_time"].as_str() {
        clip.recorded = t.replace('T', " ").trim_end_matches('Z').split('.').next().unwrap_or(t).to_string();
    }
    for key in ["com.apple.quicktime.model", "model", "encoder"] {
        if clip.camera.is_empty()
            && let Some(m) = tags[key].as_str()
            && (key != "encoder" || m.to_uppercase().contains("DJI"))
        {
            clip.camera = m.to_string();
        }
    }
    let mut audio_parts = Vec::new();
    if let Some(streams) = json["streams"].as_array() {
        for s in streams {
            match s["codec_type"].as_str() {
                Some("video") if clip.codec.is_empty() && s["disposition"]["attached_pic"].as_i64() != Some(1) => {
                    clip.codec = s["codec_long_name"]
                        .as_str()
                        .or(s["codec_name"].as_str())
                        .unwrap_or("")
                        .split(" / ")
                        .next()
                        .unwrap_or("")
                        .to_string();
                    clip.profile = s["profile"].as_str().unwrap_or("").to_string();
                    let (w, h) = (s["width"].as_i64().unwrap_or(0), s["height"].as_i64().unwrap_or(0));
                    if w > 0 && h > 0 {
                        clip.resolution = format!("{w}×{h}");
                    }
                    let fps = s["avg_frame_rate"]
                        .as_str()
                        .and_then(parse_rate)
                        .or_else(|| s["r_frame_rate"].as_str().and_then(parse_rate));
                    if let Some(f) = fps {
                        clip.fps = fmt_fps(f);
                    }
                    clip.pix_fmt = s["pix_fmt"].as_str().unwrap_or("").to_string();
                    let color: Vec<&str> = ["color_primaries", "color_transfer"]
                        .iter()
                        .filter_map(|k| s[*k].as_str())
                        .filter(|v| *v != "unknown")
                        .collect();
                    clip.color = color.join(" / ");
                    if clip.timecode.is_empty()
                        && let Some(tc) = s["tags"]["timecode"].as_str()
                    {
                        clip.timecode = tc.to_string();
                    }
                }
                Some("audio") => {
                    let ch = s["channels"].as_i64().unwrap_or(0);
                    let sr = s["sample_rate"].as_str().unwrap_or("");
                    let codec = s["codec_name"].as_str().unwrap_or("");
                    audio_parts.push(format!("{ch}ch {sr}Hz {codec}"));
                    if clip.is_audio && clip.timecode.is_empty()
                        && let (Some(tr), Ok(rate)) = (tags["time_reference"].as_str(), sr.parse::<f64>())
                        && let Ok(samples) = tr.parse::<f64>()
                        && rate > 0.0
                    {
                        let secs = samples / rate;
                        let h = (secs / 3600.0) as u64;
                        let m = ((secs % 3600.0) / 60.0) as u64;
                        let sec = secs % 60.0;
                        clip.timecode = format!("{h:02}:{m:02}:{sec:05.2}");
                    }
                }
                Some("data") if clip.timecode.is_empty() => {
                    if let Some(tc) = s["tags"]["timecode"].as_str() {
                        clip.timecode = tc.to_string();
                    }
                }
                _ => {}
            }
        }
    }
    let mut seen = Vec::new();
    for a in audio_parts {
        if !seen.contains(&a) {
            seen.push(a);
        }
    }
    clip.audio = seen.join(", ");
}

fn thumbnail(ffmpeg: &Path, path: &Path, at: f64) -> Option<String> {
    let out = output_with_timeout(
        Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{at:.3}"), "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4", "-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"]),
        Duration::from_secs(45),
    )
    .ok()?;
    if out.stdout.len() < 100 {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&out.stdout))
}

fn analyse_clip(rel: &str, input: &ReportInput<'_>, ffprobe: Option<&Path>, ffmpeg: Option<&Path>, fi: usize) -> ClipInfo {
    let path = input.copy_root.join(rel);
    let e = ext_of(rel);
    let mut clip = ClipInfo {
        rel: rel.to_string(),
        name: Path::new(rel).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        size: input.files[fi].size,
        hash: input.hashes.get(fi).cloned().flatten().unwrap_or_default(),
        is_audio: AUDIO_EXT.contains(&e.as_str()),
        is_proxy: is_proxy(rel),
        ..Default::default()
    };
    let mut sony_tc = String::new();
    if let Some(sony) = read_sony_xml(&path) {
        if let Some(tc) = sony.timecode {
            sony_tc = tc;
        }
        if let Some(m) = sony.model {
            clip.camera = m;
        }
        if let Some(g) = sony.gamma {
            clip.gamma = g;
        }
        if let Some(f) = sony.fps {
            clip.fps = f.trim_end_matches(['p', 'i']).to_string();
        }
    }
    match ffprobe {
        Some(fp) if !NO_DECODER_EXT.contains(&e.as_str()) => probe(fp, &path, &mut clip),
        Some(_) => clip.note = "RAW format needs the vendor SDK".into(),
        None => clip.note = "ffprobe not installed".into(),
    }
    // A timecode track read by ffprobe is authoritative; the Sony sidecar is
    // the fallback (XAVC MP4 files usually carry TC only in the XML).
    if clip.timecode.is_empty() || clip.timecode == sony_tc {
        clip.timecode = sony_tc;
    }
    if !clip.is_audio
        && let Some(ff) = ffmpeg
        && !NO_DECODER_EXT.contains(&e.as_str())
        && clip.duration > 0.0
    {
        for ratio in [0.1, 0.5, 0.9] {
            if input.cancel.load(Ordering::Relaxed) {
                break;
            }
            let at = (clip.duration * ratio).min((clip.duration - 0.05).max(0.0));
            if let Some(t) = thumbnail(ff, &path, at) {
                clip.thumbs.push(t);
            }
        }
    }
    clip
}

pub fn collect_clips(input: &ReportInput<'_>) -> Vec<ClipInfo> {
    let ffprobe = tools::ffprobe();
    let ffmpeg = tools::ffmpeg();
    let indices: Vec<usize> = input.files.iter().enumerate().filter(|(_, f)| is_clip(&f.rel)).map(|(i, _)| i).collect();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, ClipInfo)>> = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map(|n| n.get().clamp(2, 6)).unwrap_or(4);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    if k >= indices.len() || input.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let fi = indices[k];
                    let clip = analyse_clip(&input.files[fi].rel, input, ffprobe.as_deref(), ffmpeg.as_deref(), fi);
                    if let Some(p) = input.progress {
                        p.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Ok(mut r) = results.lock() {
                        r.push((k, clip));
                    }
                }
            });
        }
    });
    let mut r = results.into_inner().unwrap_or_default();
    r.sort_by_key(|(k, _)| *k);
    r.into_iter().map(|(_, c)| c).collect()
}

fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e12 {
        format!("{:.2} TB", b / 1e12)
    } else if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

fn fmt_duration(secs: f64) -> String {
    let t = secs.round() as u64;
    format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

fn kv(label: &str, value: &str) -> String {
    if value.trim().is_empty() {
        return String::new();
    }
    format!("<div class=\"kv\"><span>{}</span><b>{}</b></div>", esc(label), esc(value))
}

pub fn render_html(input: &ReportInput<'_>, clips: &[ClipInfo]) -> String {
    let m = input.meta;
    let main: Vec<&ClipInfo> = clips.iter().filter(|c| !c.is_proxy).collect();
    let proxies = clips.len() - main.len();
    let total_bytes: u64 = input.files.iter().map(|f| f.size).sum();
    let total_dur: f64 = main.iter().filter(|c| !c.is_audio).map(|c| c.duration).sum();
    let all_ok = input.copies.iter().all(|c| c.ok) && input.issues.is_empty();
    let ok_copies = input.copies.iter().filter(|c| c.ok).count();
    let status = if all_ok {
        format!("VERIFIED · {ok_copies} {}", if ok_copies == 1 { "COPY" } else { "COPIES" })
    } else {
        "ATTENTION REQUIRED".to_string()
    };

    let mut h = String::with_capacity(64 * 1024 + clips.len() * 80 * 1024);
    h.push_str("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    h.push_str(&format!(
        "<title>{} · {} · {} · Camera Report</title>",
        esc(&m.project_name),
        esc(&m.camera_id),
        esc(&m.roll_id)
    ));
    h.push_str(REPORT_CSS);
    h.push_str("</head><body><div class=\"page\">");

    // Header
    h.push_str("<header class=\"top\"><div class=\"brand\">");
    if let Some(logo) = &input.branding.logo_data_uri
        && logo.starts_with("data:image/")
    {
        h.push_str(&format!("<img class=\"logo\" src=\"{}\" alt=\"\">", esc(logo)));
    }
    h.push_str("<div>");
    if !input.branding.company.trim().is_empty() {
        h.push_str(&format!("<div class=\"company\">{}</div>", esc(&input.branding.company)));
    }
    h.push_str(&format!("<h1>Camera Report · {} / {}</h1>", esc(&m.camera_id), esc(&m.roll_id)));
    h.push_str(&format!(
        "<div class=\"sub\">{} · {} · {}</div>",
        esc(&m.project_name),
        esc(&m.shoot_day),
        esc(&format_date(&m.shoot_date))
    ));
    h.push_str("</div></div>");
    h.push_str(&format!(
        "<div class=\"status {}\">{}</div></header>",
        if all_ok { "ok" } else { "warn" },
        esc(&status)
    ));

    // Summary
    h.push_str("<section class=\"grid\">");
    let summary = [
        ("DIT", m.dit_name.clone()),
        ("Media", m.media_category.clone()),
        ("Source", input.source_label.to_string()),
        ("Card", input.card_vendor.unwrap_or("").to_string()),
        ("Files", input.files.len().to_string()),
        ("Clips", if proxies > 0 { format!("{} (+{} proxy)", main.len(), proxies) } else { main.len().to_string() }),
        ("Total size", fmt_bytes(total_bytes)),
        ("Total duration", fmt_duration(total_dur)),
        ("Checksum", input.algo.label().to_string()),
        ("Started", input.started.format("%Y-%m-%d %H:%M:%S").to_string()),
        ("Finished", input.finished.format("%Y-%m-%d %H:%M:%S").to_string()),
        (
            "Card released",
            input.safe_to_eject_at.map(|t| t.format("%H:%M:%S").to_string()).unwrap_or_else(|| "—".into()),
        ),
    ];
    for (k, v) in summary {
        h.push_str(&format!(
            "<div class=\"cell\"><div class=\"label\">{}</div><div class=\"value\">{}</div></div>",
            esc(k),
            esc(if v.is_empty() { "—" } else { &v })
        ));
    }
    h.push_str("</section>");

    if !input.issues.is_empty() {
        h.push_str("<section class=\"issues\"><h2>Issues</h2><ul>");
        for i in &input.issues {
            h.push_str(&format!("<li>{}</li>", esc(i)));
        }
        h.push_str("</ul></section>");
    }

    // Copies
    h.push_str("<section><h2>Verified copies</h2><table class=\"copies\"><thead><tr>");
    h.push_str("<th>Tier</th><th>Destination</th><th>Device</th><th>Status</th><th>Files</th><th>ASC MHL</th></tr></thead><tbody>");
    for c in &input.copies {
        h.push_str(&format!(
            "<tr><td>{}</td><td><b>{}</b><div class=\"path\">{}</div></td><td>{}<div class=\"path\">{}</div></td><td class=\"{}\">{}</td><td>{}</td><td class=\"path\">{}</td></tr>",
            esc(&c.tier),
            esc(&c.label),
            esc(&c.path),
            esc(&c.kind),
            esc(&c.device),
            if c.ok { "good" } else { "bad" },
            esc(&c.status),
            c.verified_files,
            esc(c.mhl.as_deref().unwrap_or("—"))
        ));
    }
    h.push_str("</tbody></table></section>");

    // Clips
    h.push_str(&format!("<section><h2>Clips ({})</h2>", main.len()));
    if main.is_empty() {
        h.push_str("<p class=\"muted\">No video or audio clips on this card.</p>");
    }
    for c in &main {
        h.push_str("<article class=\"clip\"><div class=\"thumbs\">");
        if c.thumbs.is_empty() {
            let why = if c.is_audio { "Audio" } else if !c.note.is_empty() { c.note.as_str() } else { "No preview" };
            h.push_str(&format!("<div class=\"nothumb\">{}</div>", esc(why)));
        } else {
            for t in &c.thumbs {
                h.push_str(&format!("<img src=\"data:image/jpeg;base64,{t}\" alt=\"\">"));
            }
        }
        h.push_str("</div><div class=\"meta\">");
        h.push_str(&format!(
            "<div class=\"clipname\">{}<span class=\"hash\">{} {}</span></div>",
            esc(&c.name),
            esc(input.algo.label()),
            esc(&c.hash)
        ));
        h.push_str("<div class=\"kvs\">");
        h.push_str(&kv("TC", &c.timecode));
        h.push_str(&kv("Duration", &if c.duration > 0.0 { fmt_duration(c.duration) } else { String::new() }));
        h.push_str(&kv("FPS", &c.fps));
        h.push_str(&kv("Resolution", &c.resolution));
        h.push_str(&kv("Codec", &[c.codec.as_str(), c.profile.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")));
        h.push_str(&kv("Bitrate", &if c.bitrate_mbps > 0.0 { format!("{:.0} Mb/s", c.bitrate_mbps) } else { String::new() }));
        h.push_str(&kv("Pixel", &c.pix_fmt));
        h.push_str(&kv("Gamma", &c.gamma));
        h.push_str(&kv("Color", &c.color));
        h.push_str(&kv("Audio", &c.audio));
        h.push_str(&kv("Camera", &c.camera));
        h.push_str(&kv("Recorded", &c.recorded));
        h.push_str(&kv("Size", &fmt_bytes(c.size)));
        h.push_str("</div>");
        h.push_str(&format!("<div class=\"path\">{}</div>", esc(&c.rel)));
        h.push_str("</div></article>");
    }
    h.push_str("</section>");

    h.push_str(&format!(
        "<footer>Generated {} by {} · Checksums verified by read-back from each destination · ASC MHL v2.0</footer>",
        esc(&input.finished.format("%Y-%m-%d %H:%M:%S %:z").to_string()),
        esc(input.tool)
    ));
    h.push_str("</div></body></html>");
    h
}

fn format_date(yyyymmdd: &str) -> String {
    chrono::NaiveDate::parse_from_str(yyyymmdd, "%Y%m%d")
        .map(|d| d.format("%d %b %Y").to_string())
        .unwrap_or_else(|_| yyyymmdd.to_string())
}

pub fn report_file_name(meta: &RollMeta) -> String {
    let parts = [&meta.shoot_date, &meta.project_name, &meta.shoot_day, &meta.camera_id, &meta.roll_id];
    let joined: Vec<String> = parts.iter().map(|p| crate::preset::sanitize_component(p)).collect();
    format!("{}_CameraReport", joined.join("_"))
}

/// Build the report HTML (and PDF when requested) into a private temp folder
/// on the internal disk. Returns (html, pdf) paths.
pub fn build(input: &ReportInput<'_>, want_pdf: bool) -> Result<(PathBuf, Option<PathBuf>, Option<String>), String> {
    let clips = collect_clips(input);
    let html = render_html(input, &clips);
    let dir = std::env::temp_dir().join(format!("rollvault-report-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = report_file_name(input.meta);
    let html_path = dir.join(format!("{name}.html"));
    std::fs::write(&html_path, html).map_err(|e| e.to_string())?;
    let mut pdf_path = None;
    let mut pdf_error = None;
    if want_pdf {
        let p = dir.join(format!("{name}.pdf"));
        match tools::html_to_pdf(&html_path, &p) {
            Ok(()) => pdf_path = Some(p),
            Err(e) => pdf_error = Some(e),
        }
    }
    Ok((html_path, pdf_path, pdf_error))
}

const REPORT_CSS: &str = r#"<style>
:root{--ink:#14161a;--muted:#5d6470;--line:#dfe3e8;--soft:#f5f7f9;--good:#127a4a;--bad:#b42318;--accent:#1f4fd1}
*{box-sizing:border-box}
body{margin:0;background:#eef1f4;color:var(--ink);font:13px/1.45 -apple-system,BlinkMacSystemFont,"SF Pro Text","Helvetica Neue",Arial,sans-serif}
.page{max-width:1180px;margin:24px auto;background:#fff;padding:28px 32px;border-radius:10px;box-shadow:0 1px 3px rgba(0,0,0,.08)}
.top{display:flex;justify-content:space-between;align-items:flex-start;gap:16px;border-bottom:2px solid var(--ink);padding-bottom:14px;margin-bottom:18px}
.brand{display:flex;gap:14px;align-items:center}
.logo{max-height:52px;max-width:160px;object-fit:contain}
.company{font-size:11px;letter-spacing:.12em;text-transform:uppercase;color:var(--muted)}
h1{font-size:22px;margin:2px 0}
.sub{color:var(--muted)}
.status{font-weight:700;letter-spacing:.06em;padding:8px 14px;border-radius:6px;white-space:nowrap}
.status.ok{background:#e7f6ee;color:var(--good);border:1px solid #b5e2c8}
.status.warn{background:#fdecea;color:var(--bad);border:1px solid #f4c3bd}
h2{font-size:14px;text-transform:uppercase;letter-spacing:.08em;margin:22px 0 10px}
.grid{display:grid;grid-template-columns:repeat(6,1fr);gap:1px;background:var(--line);border:1px solid var(--line);border-radius:8px;overflow:hidden}
.cell{background:#fff;padding:9px 10px}
.label{font-size:10px;text-transform:uppercase;letter-spacing:.08em;color:var(--muted)}
.value{font-weight:600;margin-top:2px;word-break:break-word}
.issues{background:#fff6f5;border:1px solid #f4c3bd;border-radius:8px;padding:4px 14px;margin-top:16px}
.issues li{color:var(--bad);margin:4px 0}
table{width:100%;border-collapse:collapse}
th,td{text-align:left;padding:7px 8px;border-bottom:1px solid var(--line);vertical-align:top}
td:first-child,th:first-child{white-space:nowrap}
th{font-size:10px;text-transform:uppercase;letter-spacing:.08em;color:var(--muted)}
td.good{color:var(--good);font-weight:600}td.bad{color:var(--bad);font-weight:600}
.path{font:11px/1.35 ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--muted);word-break:break-all}
.clip{display:grid;grid-template-columns:420px 1fr;gap:16px;padding:12px 0;border-top:1px solid var(--line);break-inside:avoid;page-break-inside:avoid}
.thumbs{display:grid;grid-template-columns:repeat(3,1fr);gap:4px;align-content:start}
.thumbs img{width:100%;aspect-ratio:16/9;object-fit:cover;background:#000;border-radius:3px}
.nothumb{grid-column:span 3;aspect-ratio:48/9;display:flex;align-items:center;justify-content:center;background:var(--soft);color:var(--muted);border-radius:3px;font-size:12px}
.clipname{font-weight:700;font-size:15px;display:flex;justify-content:space-between;gap:10px;flex-wrap:wrap}
.hash{font:11px ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--accent);font-weight:500}
.kvs{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:2px 14px;margin:6px 0}
.kv{display:flex;gap:6px;min-width:0}.kv span{color:var(--muted);flex:none}.kv b{font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.muted{color:var(--muted)}
footer{margin-top:22px;padding-top:10px;border-top:1px solid var(--line);color:var(--muted);font-size:11px}
@media screen and (max-width:820px){.grid{grid-template-columns:repeat(2,1fr)}.clip{grid-template-columns:1fr}.kvs{grid-template-columns:repeat(2,minmax(0,1fr))}.page{margin:0;border-radius:0;padding:18px 16px}}
@page{size:A4 landscape;margin:10mm}
@media print{body{background:#fff}.page{margin:0;max-width:none;box-shadow:none;padding:0}.clip{grid-template-columns:330px 1fr}}
</style>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sony_ltc_decoding() {
        // ffssmmhh BCD, with drop-frame/colour flags masked off.
        assert_eq!(sony_ltc_to_tc("15384701").as_deref(), Some("01:47:38:15"));
        assert_eq!(sony_ltc_to_tc("00000000").as_deref(), Some("00:00:00:00"));
        assert_eq!(sony_ltc_to_tc("d5384701").as_deref(), Some("01:47:38:15"));
        assert!(sony_ltc_to_tc("xyz").is_none());
    }

    #[test]
    fn clip_classification() {
        assert!(is_clip("PRIVATE/M4ROOT/CLIP/C0001.MP4"));
        assert!(is_clip("DCIM/100MEDIA/DJI_0001.MP4"));
        assert!(!is_clip("DCIM/100MEDIA/DJI_0001.LRF"));
        assert!(!is_clip("DCIM/100MEDIA/DJI_0001.SRT"));
        assert!(is_clip("ZOOM0001.WAV"));
        assert!(is_proxy("PRIVATE/M4ROOT/SUB/C0001S03.MP4"));
        assert!(!is_proxy("PRIVATE/M4ROOT/CLIP/C0001.MP4"));
    }
}
