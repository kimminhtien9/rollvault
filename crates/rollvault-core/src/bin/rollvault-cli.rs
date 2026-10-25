//! Command-line front end for the RollVault engine (same code path as the app).
//!
//! rollvault-cli volumes
//! rollvault-cli hash <file> [--algo xxh64|xxh3|xxh128|md5]
//! rollvault-cli offload --source <card> --primary <dir> [--primary <dir>] [--cascade <dir>]
//!               --project <name> --camera <id> --roll <id> [--date YYYYMMDD] [--day Day01]
//!               [--dit <name>] [--algo xxh64] [--min-copies 2] [--source-verify]
//!               [--report] [--pdf] [--kind <dir>=ssd|hdd|network]
//! rollvault-cli resume <job-id>
//! rollvault-cli verify <roll-folder>   (re-check a copy against its ASC MHL)

use rollvault_core::darwin_io::AlignedBuf;
use rollvault_core::hashing::HashAlgo;
use rollvault_core::job::{DestRequest, Engine, EngineConfig, JobRequest, Tier, human_bytes};
use rollvault_core::preset::{self, RollMeta};
use rollvault_core::volumes::DeviceKind;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn usage() -> ! {
    eprintln!(
        "usage:\n  rollvault-cli volumes\n  rollvault-cli hash <file> [--algo xxh64]\n  rollvault-cli offload --source <card> --primary <dir>... [--cascade <dir>...] --project P --camera C --roll R [options]\n  rollvault-cli resume <job-id>\n  rollvault-cli verify <roll-folder>"
    );
    std::process::exit(2)
}

fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ROLLVAULT_DATA") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/RollVault/cli")
}

fn engine() -> std::sync::Arc<Engine> {
    Engine::new(EngineConfig {
        data_dir: data_dir(),
        tool_version: env!("CARGO_PKG_VERSION").into(),
        sounds: true,
        notifications: false,
    })
}

fn follow(e: &Engine, id: &str) -> i32 {
    let start = Instant::now();
    let mut last = String::new();
    loop {
        let Some(s) = e.snapshot(id) else { return 1 };
        let line = format!(
            "[{:>6.1}s] {:>5.1}% {:?} {} | src {}/s{}",
            start.elapsed().as_secs_f64(),
            s.percent,
            s.phase,
            s.status_text,
            human_bytes(s.source_speed as u64),
            if s.safe_to_eject { " | SAFE TO EJECT" } else { "" }
        );
        if line != last {
            println!("{line}");
            for d in &s.dests {
                println!(
                    "          {:<8} {:<22} {:?} w {}/s v {}/s files {}/{}{}",
                    d.tier_label(),
                    d.volume_name,
                    d.phase,
                    human_bytes(d.write_speed as u64),
                    human_bytes(d.verify_speed as u64),
                    d.files_verified,
                    d.total_files,
                    d.error.as_ref().map(|x| format!(" ERROR: {x}")).unwrap_or_default()
                );
            }
            last = line;
        }
        if !s.phase.is_active() {
            for err in &s.errors {
                println!("ERROR: {err}");
            }
            for w in &s.warnings {
                println!("WARN:  {w}");
            }
            if let Some(r) = &s.report_path {
                println!("REPORT: {r}");
            }
            if let Some(r) = &s.report_pdf_path {
                println!("PDF:    {r}");
            }
            println!("JOB: {} => {:?} in {:.1}s", s.id, s.phase, start.elapsed().as_secs_f64());
            return if matches!(s.phase, rollvault_core::job::JobPhase::Completed) { 0 } else { 1 };
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
}

trait TierLabel {
    fn tier_label(&self) -> &'static str;
}
impl TierLabel for rollvault_core::job::DestSnapshot {
    fn tier_label(&self) -> &'static str {
        self.tier.label()
    }
}

fn main() {
    rollvault_core::l10n::set_vietnamese(std::env::var("RV_LANG").map(|v| v == "vi").unwrap_or(false));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { usage() };
    match cmd.as_str() {
        "volumes" => {
            let e = engine();
            for v in e.list_volumes() {
                println!(
                    "{:<28} {:<7} {:<6} {:<10} free {:>10} / {:>10} {}{}{}",
                    v.name,
                    v.kind.label(),
                    v.fs_type,
                    v.device_id,
                    human_bytes(v.free_bytes),
                    human_bytes(v.total_bytes),
                    v.mount_point,
                    v.card_vendor
                        .as_ref()
                        .map(|c| if v.is_camera_card { format!("  [{c} CARD]") } else { format!("  [{c}-style folders]") })
                        .unwrap_or_default(),
                    if v.unresponsive { "  [NOT RESPONDING]" } else { "" }
                );
            }
        }
        "hash" => {
            let Some(file) = args.get(1) else { usage() };
            let algo = args
                .iter()
                .position(|a| a == "--algo")
                .and_then(|i| args.get(i + 1))
                .and_then(|a| HashAlgo::from_name(a))
                .unwrap_or(HashAlgo::Xxh64);
            let mut buf = AlignedBuf::new(8 * 1024 * 1024);
            let t = Instant::now();
            match rollvault_core::transfer::cold_hash_file(file.as_ref(), algo, &mut buf, None, &AtomicBool::new(false)) {
                Ok((h, n)) => {
                    let secs = t.elapsed().as_secs_f64();
                    println!("{h}  {file}");
                    eprintln!("{} in {:.2}s ({}/s, cold read)", human_bytes(n), secs, human_bytes((n as f64 / secs) as u64));
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        "offload" => {
            let mut opts: HashMap<String, Vec<String>> = HashMap::new();
            let mut flags: Vec<String> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                let a = &args[i];
                if !a.starts_with("--") {
                    usage();
                }
                let key = a.trim_start_matches("--").to_string();
                if matches!(key.as_str(), "source-verify" | "report" | "pdf" | "yes") {
                    flags.push(key);
                    i += 1;
                } else {
                    let Some(v) = args.get(i + 1) else { usage() };
                    opts.entry(key).or_default().push(v.clone());
                    i += 2;
                }
            }
            let one = |k: &str| opts.get(k).and_then(|v| v.first().cloned());
            let kinds: HashMap<String, DeviceKind> = opts
                .get("kind")
                .map(|v| {
                    v.iter()
                        .filter_map(|kv| kv.split_once('='))
                        .filter_map(|(p, k)| {
                            let kind = match k {
                                "ssd" => DeviceKind::Ssd,
                                "hdd" => DeviceKind::Hdd,
                                "network" | "nas" => DeviceKind::Network,
                                _ => return None,
                            };
                            Some((p.to_string(), kind))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut destinations = Vec::new();
            for (key, tier) in [("primary", Tier::Primary), ("cascade", Tier::Cascade)] {
                for root in opts.get(key).cloned().unwrap_or_default() {
                    let kind_override = kinds.get(&root).copied();
                    destinations.push(DestRequest { root, tier, kind_override });
                }
            }
            let req = JobRequest {
                source: one("source").unwrap_or_else(|| usage()),
                destinations,
                meta: RollMeta {
                    shoot_date: one("date").unwrap_or_else(|| chrono::Local::now().format("%Y%m%d").to_string()),
                    project_name: one("project").unwrap_or_else(|| usage()),
                    media_category: one("category").unwrap_or_else(|| "01_FOOTAGE".into()),
                    shoot_day: one("day").unwrap_or_else(|| "Day01".into()),
                    camera_id: one("camera").unwrap_or_else(|| usage()),
                    roll_id: one("roll").unwrap_or_else(|| usage()),
                    dit_name: one("dit").unwrap_or_default(),
                },
                roll_template: one("template").unwrap_or_else(|| preset::DEFAULT_ROLL_TEMPLATE.into()),
                report_template: preset::DEFAULT_REPORT_TEMPLATE.into(),
                algo: one("algo").and_then(|a| HashAlgo::from_name(&a)).unwrap_or(HashAlgo::Xxh64),
                min_copies: one("min-copies").and_then(|v| v.parse().ok()).unwrap_or(2),
                source_verify: flags.iter().any(|f| f == "source-verify"),
                generate_report: flags.iter().any(|f| f == "report"),
                report_pdf: flags.iter().any(|f| f == "pdf"),
                branding: Default::default(),
                location: String::new(),
                confirm_repeat_card: flags.iter().any(|f| f == "yes"),
            };
            let e = engine();
            let pf = e.preflight(&req);
            println!(
                "Source: {} | {} files | {} | {} clips | roll: {}",
                pf.source_name,
                pf.file_count,
                human_bytes(pf.total_bytes),
                pf.clip_count,
                pf.roll_rel_path
            );
            for w in &pf.warnings {
                println!("WARN:  {w}");
            }
            match e.start(req) {
                Ok(id) => std::process::exit(follow(&e, &id)),
                Err(pf) => {
                    for x in &pf.errors {
                        eprintln!("ERROR: {x}");
                    }
                    if pf.needs_confirmation {
                        eprintln!("Add --yes to confirm offloading this card again.");
                    }
                    std::process::exit(1);
                }
            }
        }
        "verify" => {
            let Some(dir) = args.get(1) else { usage() };
            let progress = rollvault_core::reverify::VerifyProgress::default();
            let t = Instant::now();
            match rollvault_core::reverify::verify_folder(
                dir.as_ref(),
                env!("CARGO_PKG_VERSION"),
                "",
                &AtomicBool::new(false),
                &progress,
            ) {
                Ok(out) => {
                    println!(
                        "{} files match, {} mismatched, {} missing, {} not in history ({:.1}s)",
                        out.verified,
                        out.failed.len(),
                        out.missing.len(),
                        out.unrecorded.len(),
                        t.elapsed().as_secs_f64()
                    );
                    for f in &out.failed {
                        println!("MISMATCH: {f}");
                    }
                    for f in &out.missing {
                        println!("MISSING:  {f}");
                    }
                    for f in &out.unrecorded {
                        println!("NEW:      {f}");
                    }
                    if let Some(m) = &out.manifest {
                        println!("ASC MHL: {m}");
                    }
                    std::process::exit(if out.ok() { 0 } else { 1 });
                }
                Err(e) => {
                    eprintln!("ERROR: {e}");
                    std::process::exit(1);
                }
            }
        }
        "resume" => {
            let Some(id) = args.get(1) else { usage() };
            let e = engine();
            match e.resume(id) {
                Ok(id) => std::process::exit(follow(&e, &id)),
                Err(err) => {
                    eprintln!("ERROR: {err}");
                    std::process::exit(1);
                }
            }
        }
        _ => usage(),
    }
}
