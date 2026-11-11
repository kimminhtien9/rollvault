//! End-to-end engine tests on real disk images (each image is its own
//! physical device, so the copy-count policy is exercised for real).
//!
//! Optional external validators (tests skip those checks when absent):
//!   ASCMHL_BIN      path to the ASC reference `ascmhl` CLI
//!   ASCMHL_XSD_DIR  folder with ASCMHL.xsd and ASCMHLDirectory.local.xsd

mod common;

use common::*;
use rollvault_core::hashing::HashAlgo;
use rollvault_core::job::*;
use rollvault_core::preset::RollMeta;
use rollvault_core::transfer::FaultInjector;
use rollvault_core::volumes::DeviceKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn engine(data: &Path) -> Arc<Engine> {
    Engine::new(EngineConfig {
        data_dir: data.to_path_buf(),
        tool_version: "test".into(),
        sounds: false,
        notifications: false,
    })
}

fn meta(roll: &str) -> RollMeta {
    RollMeta {
        shoot_date: "20261025".into(),
        project_name: "TVC Test".into(),
        media_category: "01_FOOTAGE".into(),
        shoot_day: "Day01".into(),
        camera_id: "CAM_A".into(),
        roll_id: roll.into(),
        dit_name: "Test DIT".into(),
    }
}

fn request(card: &Path, dests: &[(&Path, Tier, DeviceKind)], roll: &str) -> JobRequest {
    JobRequest {
        source: card.display().to_string(),
        destinations: dests
            .iter()
            .map(|(p, t, k)| DestRequest { root: p.display().to_string(), tier: *t, kind_override: Some(*k) })
            .collect(),
        meta: meta(roll),
        roll_template: rollvault_core::preset::DEFAULT_ROLL_TEMPLATE.into(),
        report_template: rollvault_core::preset::DEFAULT_REPORT_TEMPLATE.into(),
        algo: HashAlgo::Xxh64,
        min_copies: 2,
        source_verify: false,
        generate_report: false,
        report_pdf: false,
        branding: Default::default(),
        location: String::new(),
        confirm_repeat_card: false,
    }
}

fn wait(engine: &Engine, id: &str, limit: Duration) -> JobSnapshot {
    let start = Instant::now();
    loop {
        let s = engine.snapshot(id).expect("job exists");
        if !s.phase.is_active() {
            return s;
        }
        assert!(start.elapsed() < limit, "job did not finish in {limit:?}: {:?} {}", s.phase, s.status_text);
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn images(dir: &Path, names: &[&str], mb: u64) -> Option<Vec<DiskImage>> {
    names.iter().map(|n| DiskImage::create(dir, n, mb)).collect()
}

fn assert_identical(card: &Path, roll: &Path) {
    let scan = rollvault_core::scan::scan_source(card).unwrap();
    for f in &scan.files {
        assert!(files_equal(&card.join(&f.rel), &roll.join(&f.rel)), "{} differs in {}", f.rel, roll.display());
    }
    assert!(find_partials(roll).is_empty());
}

/// Validate a destination with the ASC reference tool on a scratch copy:
/// `ascmhl create` re-hashes everything; every file must come back
/// "verified" (nothing new, nothing failed, nothing missing).
fn ascmhl_validate(roll: &Path, scratch: &Path) {
    let Ok(bin) = std::env::var("ASCMHL_BIN") else {
        eprintln!("ASCMHL_BIN not set: skipping reference validation");
        return;
    };
    let copy = scratch.join(format!("val-{}", uuid::Uuid::new_v4()));
    let st = Command::new("/bin/cp").arg("-Rp").arg(roll).arg(&copy).status().unwrap();
    assert!(st.success());
    let out = Command::new(&bin).args(["create", "-h", "xxh64", "-v"]).arg(&copy).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "ascmhl create failed:\n{text}");
    let newest = std::fs::read_dir(copy.join("ascmhl"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "mhl").unwrap_or(false))
        .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with("._"))
        .max()
        .unwrap();
    let xml = std::fs::read_to_string(&newest).unwrap();
    assert!(!xml.contains("action=\"original\""), "reference tool found files we did not record:\n{xml}");
    assert!(!xml.contains("action=\"failed\""), "reference tool found mismatches:\n{xml}");
    assert!(xml.contains("action=\"verified\""));
    let _ = std::fs::remove_dir_all(&copy);
}

/// Negative control: the reference tool must flag a one-byte corruption.
fn ascmhl_detects_corruption(roll: &Path, scratch: &Path) {
    let Ok(bin) = std::env::var("ASCMHL_BIN") else { return };
    let copy = scratch.join(format!("neg-{}", uuid::Uuid::new_v4()));
    assert!(Command::new("/bin/cp").arg("-Rp").arg(roll).arg(&copy).status().unwrap().success());
    let victim = copy.join("PRIVATE/M4ROOT/CLIP/C0003.MP4");
    let mut data = std::fs::read(&victim).unwrap();
    data[1234] ^= 0x01;
    std::fs::write(&victim, data).unwrap();
    let out = Command::new(&bin).args(["create", "-h", "xxh64"]).arg(&copy).output().unwrap();
    let newest = std::fs::read_dir(copy.join("ascmhl"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "mhl").unwrap_or(false))
        .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with("._"))
        .max()
        .unwrap();
    let xml = std::fs::read_to_string(newest).unwrap_or_default();
    assert!(
        !out.status.success() || xml.contains("action=\"failed\""),
        "reference tool did not detect a flipped bit"
    );
    let _ = std::fs::remove_dir_all(&copy);
}

fn xsd_validate(roll: &Path) {
    let Ok(dir) = std::env::var("ASCMHL_XSD_DIR") else { return };
    let dir = PathBuf::from(dir);
    for e in std::fs::read_dir(roll.join("ascmhl")).unwrap().filter_map(|e| e.ok()) {
        let p = e.path();
        if p.file_name().unwrap().to_string_lossy().starts_with("._") {
            continue;
        }
        let schema = if p.file_name().unwrap() == "ascmhl_chain.xml" {
            dir.join("ASCMHLDirectory.local.xsd")
        } else {
            dir.join("ASCMHL.xsd")
        };
        let out = Command::new("/usr/bin/xmllint").arg("--noout").arg("--schema").arg(&schema).arg(&p).output().unwrap();
        assert!(out.status.success(), "XSD validation failed for {}: {}", p.display(), String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn full_offload_two_ssds_and_hdd_cascade() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvA_ssd1", "rvA_ssd2", "rvA_hdd"], 160) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let mut req = request(
        &card,
        &[
            (&imgs[0].mount, Tier::Primary, DeviceKind::Ssd),
            (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd),
            (&imgs[2].mount, Tier::Cascade, DeviceKind::Hdd),
        ],
        "A001",
    );
    req.generate_report = true;
    let pf = e.preflight(&req);
    assert!(pf.ok, "preflight: {:?}", pf.errors);
    assert_eq!(pf.distinct_devices, 3);
    let id = e.start(req.clone()).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::Completed, "{} {:?}", s.status_text, s.errors);
    assert!(s.safe_to_eject);
    assert_eq!(s.verified_copies, 3);
    for d in &s.dests {
        assert!(d.complete, "{d:?}");
        let roll = PathBuf::from(&d.roll_dir);
        assert_identical(&card, &roll);
        assert!(roll.join("ascmhl/ascmhl_chain.xml").exists());
        assert!(d.legacy_mhl_path.as_ref().map(|p| Path::new(p).exists()).unwrap_or(false));
        assert!(roll.join("PRIVATE/M4ROOT/SUB").is_dir(), "empty folders must be preserved");
        xsd_validate(&roll);
        ascmhl_validate(&roll, t.path());
        ascmhl_detects_corruption(&roll, t.path());
        let report_dir = PathBuf::from(&d.root).join("20261025_TVC_Test/03_DIT_REPORTS");
        assert!(report_dir.join("20261025_TVC_Test_Day01_CAM_A_A001_CameraReport.html").exists());
    }
    let hdd = PathBuf::from(&s.dests[2].roll_dir);
    let gens = std::fs::read_dir(hdd.join("ascmhl")).unwrap().filter(|e| {
        e.as_ref().unwrap().path().extension().map(|x| x == "mhl").unwrap_or(false)
    }).count();
    assert_eq!(gens, 2, "cascade copy must carry the SSD history plus its own generation");
    let chain = std::fs::read_to_string(hdd.join("ascmhl/ascmhl_chain.xml")).unwrap();
    assert!(chain.contains("sequencenr=\"2\""));

    // Same card again: confirmation required, and the roll folder is taken.
    let again = e.preflight(&req);
    assert!(again.needs_confirmation);
    assert!(again.errors.iter().any(|x| x.contains("already exists")), "{:?}", again.errors);
    assert_eq!(e.history().len(), 1);
}

#[test]
fn single_ssd_is_safe_only_after_cascade_copy() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvB_ssd", "rvB_hdd"], 120) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let req = request(
        &card,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Cascade, DeviceKind::Hdd)],
        "B001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::Completed, "{:?}", s.errors);
    assert!(s.safe_to_eject);
    assert_eq!(s.verified_copies, 2);
}

#[test]
fn one_destination_alone_is_never_safe() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvC_ssd"], 100) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let req = request(&card, &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd)], "C001");
    let pf = e.preflight(&req);
    assert!(pf.ok, "{:?}", pf.errors);
    assert!(pf.warnings.iter().any(|w| w.contains("safe to eject")), "{:?}", pf.warnings);
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(60));
    assert_eq!(s.phase, JobPhase::Completed);
    assert!(!s.safe_to_eject, "one copy must never be reported as safe");
}

#[test]
fn source_verify_catches_a_flaky_card_reader() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvD_ssd1", "rvD_ssd2"], 120) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let faults = Arc::new(FaultInjector::default());
    faults.corrupt_source_once.lock().unwrap().insert("PRIVATE/M4ROOT/CLIP/C0003.MP4".into());
    e.set_faults(EngineFaults { primary: Some(faults), cascade: None, ..Default::default() });
    let mut req = request(
        &card,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
        "D001",
    );
    req.source_verify = true;
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(120));
    assert!(!s.safe_to_eject, "a card that reads differently twice must never be safe");
    assert_eq!(s.phase, JobPhase::Failed, "{:?}", s.errors);
    assert!(s.errors.iter().any(|x| x.contains("SOURCE CHECK FAILED")), "{:?}", s.errors);
}

#[test]
fn without_source_verify_a_flaky_reader_is_invisible() {
    // Documents the limitation that motivates the source-verify option.
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvE_ssd1", "rvE_ssd2"], 120) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let faults = Arc::new(FaultInjector::default());
    faults.corrupt_source_once.lock().unwrap().insert("PRIVATE/M4ROOT/CLIP/C0003.MP4".into());
    e.set_faults(EngineFaults { primary: Some(faults), cascade: None, ..Default::default() });
    let req = request(
        &card,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
        "E001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::Completed);
    let roll = PathBuf::from(&s.dests[0].roll_dir);
    assert!(!files_equal(&card.join("PRIVATE/M4ROOT/CLIP/C0003.MP4"), &roll.join("PRIVATE/M4ROOT/CLIP/C0003.MP4")));
}

#[test]
fn cascade_rejects_corrupt_reads_of_the_ssd_copy_and_recovers() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvF_ssd1", "rvF_ssd2", "rvF_hdd"], 160) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let faults = Arc::new(FaultInjector::default());
    // The SSD returns bad data once while cascading: must be caught against
    // the ORIGINAL card hash and retried.
    faults.corrupt_source_once.lock().unwrap().insert("PRIVATE/M4ROOT/CLIP/C0002.MP4".into());
    e.set_faults(EngineFaults { primary: None, cascade: Some(faults), ..Default::default() });
    let req = request(
        &card,
        &[
            (&imgs[0].mount, Tier::Primary, DeviceKind::Ssd),
            (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd),
            (&imgs[2].mount, Tier::Cascade, DeviceKind::Hdd),
        ],
        "F001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::Completed, "{:?}", s.errors);
    assert_identical(&card, Path::new(&s.dests[2].roll_dir));
}

#[test]
fn broken_cascade_destination_does_not_break_the_job() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvG_ssd1", "rvG_ssd2", "rvG_hdd"], 160) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let faults = Arc::new(FaultInjector::default());
    faults.corrupt_dest_always.lock().unwrap().insert((0, "PRIVATE/M4ROOT/CLIP/C0001.MP4".into()));
    e.set_faults(EngineFaults { primary: None, cascade: Some(faults), ..Default::default() });
    let req = request(
        &card,
        &[
            (&imgs[0].mount, Tier::Primary, DeviceKind::Ssd),
            (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd),
            (&imgs[2].mount, Tier::Cascade, DeviceKind::Hdd),
        ],
        "G001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::CompletedWithErrors, "{:?}", s.errors);
    assert!(s.safe_to_eject, "two verified SSD copies are enough");
    assert!(!s.dests[2].complete);
    assert!(!Path::new(&s.dests[2].roll_dir).join("ascmhl").exists(), "incomplete copy must not get an MHL");
}

#[test]
fn cancel_then_resume_completes_with_valid_mhl() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvH_ssd1", "rvH_ssd2"], 120) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let data = t.path().join("data");
    let e = engine(&data);
    let faults = Arc::new(FaultInjector::default());
    *faults.chunk_delay.lock().unwrap() = Some(Duration::from_millis(60));
    e.set_faults(EngineFaults { primary: Some(faults), cascade: None, ..Default::default() });
    let req = request(
        &card,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
        "H001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    std::thread::sleep(Duration::from_millis(300));
    assert!(e.cancel(&id));
    let s = wait(&e, &id, Duration::from_secs(60));
    assert_eq!(s.phase, JobPhase::Cancelled);
    assert!(s.resumable);
    assert!(!s.safe_to_eject);

    // A brand-new engine (as after an app restart) resumes from the journal.
    drop(e);
    let e = engine(&data);
    e.set_faults(EngineFaults::default());
    let id2 = e.resume(&id).expect("resume");
    assert_eq!(id2, id);
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::Completed, "{:?}", s.errors);
    assert!(s.safe_to_eject);
    for d in &s.dests {
        let roll = PathBuf::from(&d.roll_dir);
        assert_identical(&card, &roll);
        xsd_validate(&roll);
        ascmhl_validate(&roll, t.path());
    }
}

#[test]
fn resume_refuses_a_different_card() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvI_ssd1", "rvI_ssd2"], 120) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let faults = Arc::new(FaultInjector::default());
    *faults.chunk_delay.lock().unwrap() = Some(Duration::from_millis(60));
    e.set_faults(EngineFaults { primary: Some(faults), cascade: None, ..Default::default() });
    let req = request(
        &card,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
        "I001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    std::thread::sleep(Duration::from_millis(250));
    e.cancel(&id);
    wait(&e, &id, Duration::from_secs(60));
    std::fs::write(card.join("DCIM/100MSDCF/DSC00002.JPG"), b"new shot").unwrap();
    let err = e.resume(&id).unwrap_err();
    assert!(err.contains("differs from the original"), "{err}");
}

#[test]
fn concurrent_cards_share_destinations() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvJ_ssd1", "rvJ_ssd2"], 200) else { return };
    let card_a = t.path().join("cardA");
    let card_b = t.path().join("cardB");
    make_sony_card(&card_a);
    make_sony_card(&card_b);
    std::fs::write(card_b.join("DCIM/100MSDCF/DSC00009.JPG"), bytes(99, 5000)).unwrap();
    let e = engine(&t.path().join("data"));
    let dests = [(&imgs[0].mount as &Path, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)];
    let mut ra = request(&card_a, &dests, "A001");
    ra.meta.camera_id = "CAM_A".into();
    let mut rb = request(&card_b, &dests, "B001");
    rb.meta.camera_id = "CAM_B".into();
    let a = e.start(ra).unwrap_or_else(|pf| panic!("A: {:?}", pf.errors));
    let b = e.start(rb).unwrap_or_else(|pf| panic!("B: {:?}", pf.errors));
    let sa = wait(&e, &a, Duration::from_secs(120));
    let sb = wait(&e, &b, Duration::from_secs(120));
    assert_eq!(sa.phase, JobPhase::Completed, "{:?}", sa.errors);
    assert_eq!(sb.phase, JobPhase::Completed, "{:?}", sb.errors);
    assert_identical(&card_a, Path::new(&sa.dests[0].roll_dir));
    assert_identical(&card_b, Path::new(&sb.dests[1].roll_dir));
}

#[test]
fn preflight_blocks_unsafe_setups() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvK_small"], 20) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    // Too small destination.
    let req = request(&card, &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd)], "K001");
    let pf = e.preflight(&req);
    assert!(!pf.ok);
    assert!(pf.errors.iter().any(|x| x.contains("Not enough space")), "{:?}", pf.errors);
    // Destination inside the source.
    let inside = card.join("PRIVATE");
    let req = request(&card, &[(&inside, Tier::Primary, DeviceKind::Ssd)], "K001");
    let pf = e.preflight(&req);
    assert!(pf.errors.iter().any(|x| x.contains("inside the source") || x.contains("same physical device")), "{:?}", pf.errors);
    // No Tier 1.
    let req = request(&card, &[(&imgs[0].mount, Tier::Cascade, DeviceKind::Hdd)], "K001");
    assert!(e.preflight(&req).errors.iter().any(|x| x.contains("Tier 1")));
    // Missing source.
    let req = request(&t.path().join("nope"), &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd)], "K001");
    assert!(!e.preflight(&req).ok);
}

#[test]
fn unplugged_ssd_mid_job_leaves_one_copy_and_no_false_safety() {
    let t = tempfile::tempdir().unwrap();
    let Some(mut imgs) = images(t.path(), &["rvL_ssd1", "rvL_ssd2"], 160) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let faults = Arc::new(FaultInjector::default());
    *faults.chunk_delay.lock().unwrap() = Some(Duration::from_millis(50));
    e.set_faults(EngineFaults { primary: Some(faults), cascade: None, ..Default::default() });
    let req = request(
        &card,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
        "L001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    std::thread::sleep(Duration::from_millis(250));
    imgs[1].detach_force();
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::CompletedWithErrors, "{:?}", s.errors);
    assert!(!s.safe_to_eject);
    assert!(s.dests[0].complete);
    assert!(!s.dests[1].complete);
    assert!(s.dests[1].error.is_some());
}

#[test]
fn exfat_destination_like_a_typical_dit_nvme() {
    let t = tempfile::tempdir().unwrap();
    let Some(ex) = DiskImage::create_fs(t.path(), "rvM_exfat", 200, "ExFAT") else { return };
    let Some(hfs) = DiskImage::create(t.path(), "rvM_hfs", 160) else { return };
    let fs_type = rollvault_core::darwin_io::statfs(&ex.mount).unwrap().fs_type;
    eprintln!("exFAT image mounted with fs type {fs_type}");
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let req = request(
        &card,
        &[(&ex.mount, Tier::Primary, DeviceKind::Ssd), (&hfs.mount, Tier::Primary, DeviceKind::Ssd)],
        "M001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::Completed, "{:?}", s.errors);
    for d in &s.dests {
        let roll = PathBuf::from(&d.roll_dir);
        let dot_underscore: Vec<String> = walkdir::WalkDir::new(&roll)
            .into_iter()
            .filter_map(|e| e.ok())
            .map(|e| e.path().strip_prefix(&roll).unwrap().display().to_string())
            .filter(|n| n.contains("._") || n.contains(".rvpart") || n.contains(".tmp"))
            .collect();
        eprintln!("{} ({}): AppleDouble/partials: {:?}", d.volume_name, fs_type, dot_underscore);
        assert!(!dot_underscore.iter().any(|n| n.contains(".rvpart") || n.ends_with(".tmp")), "{dot_underscore:?}");
        assert_identical(&card, &roll);
        xsd_validate(&roll);
        ascmhl_validate(&roll, t.path());
        let scan = rollvault_core::scan::scan_source(&card).unwrap();
        for f in &scan.files {
            let a = std::fs::metadata(card.join(&f.rel)).unwrap().modified().unwrap();
            let b = std::fs::metadata(roll.join(&f.rel)).unwrap().modified().unwrap();
            let diff = a.duration_since(b).unwrap_or_else(|e| e.duration());
            assert!(diff <= Duration::from_secs(2), "mtime not preserved on {}: {:?}", d.volume_name, diff);
        }
    }
}

#[test]
fn reverify_detects_bit_rot_missing_and_unrecorded_files() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvN_ssd1", "rvN_ssd2"], 120) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let req = request(
        &card,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
        "N001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::Completed);
    let roll = PathBuf::from(&s.dests[0].roll_dir);

    // Clean copy passes and gets an in-place generation.
    let p = rollvault_core::reverify::VerifyProgress::default();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let ok = rollvault_core::reverify::verify_folder(&roll, "test", "DIT", &cancel, &p).unwrap();
    assert!(ok.ok(), "{ok:?}");
    assert_eq!(ok.verified, 9);
    assert!(ok.unrecorded.is_empty(), "{:?}", ok.unrecorded);
    xsd_validate(&roll);

    // Damage the copy three ways.
    let victim = roll.join("PRIVATE/M4ROOT/CLIP/C0003.MP4");
    let mut data = std::fs::read(&victim).unwrap();
    data[777] ^= 0x40;
    std::fs::write(&victim, data).unwrap();
    std::fs::remove_file(roll.join("PRIVATE/M4ROOT/MEDIAPRO.XML")).unwrap();
    std::fs::write(roll.join("PRIVATE/M4ROOT/CLIP/STRAY.MP4"), b"not from the card").unwrap();
    let bad = rollvault_core::reverify::verify_folder(&roll, "test", "DIT", &cancel, &p).unwrap();
    assert!(!bad.ok());
    assert_eq!(bad.failed.len(), 1, "{:?}", bad.failed);
    assert!(bad.failed[0].starts_with("PRIVATE/M4ROOT/CLIP/C0003.MP4"));
    assert_eq!(bad.missing, vec!["PRIVATE/M4ROOT/MEDIAPRO.XML".to_string()]);
    assert_eq!(bad.unrecorded, vec!["PRIVATE/M4ROOT/CLIP/STRAY.MP4".to_string()]);
    let latest = std::fs::read_to_string(bad.manifest.as_ref().unwrap()).unwrap();
    assert!(latest.contains("action=\"failed\""));
    assert!(latest.contains("<process>in-place</process>"));
    xsd_validate(&roll);

    // Background task API on the engine.
    let task = e.start_verify(PathBuf::from(&s.dests[1].roll_dir), "DIT".into()).unwrap();
    let start = Instant::now();
    loop {
        let snap = e.verify_snapshots().into_iter().find(|v| v.id == task).unwrap();
        if snap.status != VerifyStatus::Running {
            assert_eq!(snap.status, VerifyStatus::Passed, "{snap:?}");
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(e.start_verify(t.path().join("card"), "DIT".into()).is_err(), "folder without history must be refused");
}

#[test]
fn exfat_and_fat32_camera_cards_as_source() {
    for (fs_name, tag) in [("ExFAT", "rvO_exfat"), ("MS-DOS FAT32", "rvO_fat32")] {
        let t = tempfile::tempdir().unwrap();
        let Some(card_img) = DiskImage::create_fs(t.path(), &format!("{tag}_card"), 120, fs_name) else { return };
        let Some(imgs) = images(t.path(), &[&format!("{tag}_d1"), &format!("{tag}_d2")], 120) else { return };
        make_sony_card(&card_img.mount);
        let e = engine(&t.path().join("data"));
        let req = request(
            &card_img.mount,
            &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
            "O001",
        );
        let pf = e.preflight(&req);
        assert!(pf.ok, "{fs_name}: {:?}", pf.errors);
        assert_eq!(pf.card_vendor.as_deref(), Some("Sony"));
        let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
        let s = wait(&e, &id, Duration::from_secs(120));
        assert_eq!(s.phase, JobPhase::Completed, "{fs_name}: {:?}", s.errors);
        for d in &s.dests {
            assert_identical(&card_img.mount, Path::new(&d.roll_dir));
        }
    }
}

#[test]
fn card_pulled_mid_copy_then_reinserted_and_resumed() {
    let t = tempfile::tempdir().unwrap();
    let Some(mut card_img) = DiskImage::create_fs(t.path(), "rvP_card", 120, "ExFAT") else { return };
    let Some(imgs) = images(t.path(), &["rvP_d1", "rvP_d2"], 120) else { return };
    make_sony_card(&card_img.mount);
    let data = t.path().join("data");
    let e = engine(&data);
    let faults = Arc::new(FaultInjector::default());
    *faults.chunk_delay.lock().unwrap() = Some(Duration::from_millis(60));
    e.set_faults(EngineFaults { primary: Some(faults), cascade: None, ..Default::default() });
    let req = request(
        &card_img.mount,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
        "P001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    std::thread::sleep(Duration::from_millis(300));
    card_img.detach_force();
    let s = wait(&e, &id, Duration::from_secs(60));
    assert_eq!(s.phase, JobPhase::Failed, "{:?}", s.errors);
    assert!(!s.safe_to_eject);
    assert!(s.resumable);
    for d in &s.dests {
        assert!(find_partials(Path::new(&d.roll_dir)).is_empty(), "partials left after card pull");
    }

    assert!(card_img.reattach(), "could not re-insert the card image");
    e.set_faults(EngineFaults::default());
    e.resume(&id).expect("resume after re-inserting the card");
    let s = wait(&e, &id, Duration::from_secs(120));
    assert_eq!(s.phase, JobPhase::Completed, "{:?}", s.errors);
    assert!(s.safe_to_eject);
    for d in &s.dests {
        let roll = PathBuf::from(&d.roll_dir);
        assert_identical(&card_img.mount, &roll);
        xsd_validate(&roll);
        ascmhl_validate(&roll, t.path());
    }
}

#[test]
fn snapshots_never_deadlock_while_a_destination_fails() {
    // Regression: the job thread used to lock a destination and then the job
    // state, while the UI snapshot locked them in the opposite order.
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvQ_ssd1", "rvQ_ssd2"], 120) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    for f in 0..30 {
        std::fs::write(card.join(format!("DCIM/100MSDCF/X{f:03}.JPG")), bytes(f, 3000)).unwrap();
    }
    let e = engine(&t.path().join("data"));
    let faults = Arc::new(FaultInjector::default());
    {
        let mut set = faults.corrupt_dest_always.lock().unwrap();
        for f in 0..30 {
            set.insert((1, format!("DCIM/100MSDCF/X{f:03}.JPG")));
        }
    }
    e.set_faults(EngineFaults { primary: Some(faults), ..Default::default() });
    let req = request(
        &card,
        &[(&imgs[0].mount, Tier::Primary, DeviceKind::Ssd), (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd)],
        "Q001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pollers: Vec<_> = (0..3)
        .map(|_| {
            let e = e.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = e.snapshots();
                }
            })
        })
        .collect();
    let s = wait(&e, &id, Duration::from_secs(60));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for p in pollers {
        p.join().unwrap();
    }
    assert_eq!(s.phase, JobPhase::CompletedWithErrors, "{:?}", s.errors);
}

#[test]
fn hung_cascade_drive_does_not_hang_the_job() {
    let t = tempfile::tempdir().unwrap();
    let Some(imgs) = images(t.path(), &["rvR_ssd1", "rvR_ssd2", "rvR_hdd"], 160) else { return };
    let card = t.path().join("card");
    make_sony_card(&card);
    let e = engine(&t.path().join("data"));
    let faults = Arc::new(FaultInjector::default());
    faults.hang_dest_write.lock().unwrap().insert((0, "PRIVATE/M4ROOT/CLIP/C0002.MP4".into()));
    e.set_faults(EngineFaults { cascade: Some(faults), stall_timeout: Some(Duration::from_secs(1)), ..Default::default() });
    let req = request(
        &card,
        &[
            (&imgs[0].mount, Tier::Primary, DeviceKind::Ssd),
            (&imgs[1].mount, Tier::Primary, DeviceKind::Ssd),
            (&imgs[2].mount, Tier::Cascade, DeviceKind::Hdd),
        ],
        "R001",
    );
    let id = e.start(req).unwrap_or_else(|pf| panic!("{:?}", pf.errors));
    let s = wait(&e, &id, Duration::from_secs(60));
    assert_eq!(s.phase, JobPhase::CompletedWithErrors, "{:?}", s.errors);
    assert!(s.safe_to_eject);
    assert!(!s.dests[2].complete);
    assert!(s.dests[2].error.as_deref().unwrap_or("").contains("stopped responding"), "{:?}", s.dests[2].error);
}

fn history_entry(id: &str, status: &str, age_days: i64) -> serde_json::Value {
    let at = chrono::Utc::now() - chrono::Duration::days(age_days);
    serde_json::json!({
        "job_id": id,
        "created_at": at,
        "finished_at": at,
        "status": status,
        "meta": meta("A001"),
        "source": "/Volumes/CARD",
        "source_label": "CARD",
        "fingerprint": id,
        "file_count": 1,
        "total_bytes": 1,
        "dests": [],
        "report_path": null,
        "safe_to_eject": true,
        "errors": [],
    })
}

#[test]
fn history_delete_and_auto_clean() {
    let data = tempfile::tempdir().unwrap();
    let keep = data.path().join("keep.txt");
    std::fs::write(&keep, b"x").unwrap();
    let entries = vec![
        history_entry("old-done", "completed", 100),
        history_entry("new-done", "completed", 1),
        history_entry("old-failed", "failed", 100),
        history_entry("../keep.txt", "completed", 1),
    ];
    std::fs::write(data.path().join("history.json"), serde_json::to_vec(&entries).unwrap()).unwrap();
    for id in ["old-done", "new-done", "old-failed"] {
        std::fs::create_dir_all(data.path().join("jobs").join(id)).unwrap();
    }
    let e = engine(data.path());
    assert_eq!(e.history().len(), 4);

    // Auto-clean: only completed jobs past the limit; 0 days keeps everything.
    assert_eq!(e.prune_history(0), 0);
    assert_eq!(e.prune_history(90), 1);
    assert!(!data.path().join("jobs/old-done").exists());
    assert!(data.path().join("jobs/old-failed").exists());

    // Manual delete: unknown ids are ignored, journals go with their entry.
    assert_eq!(e.delete_history(&["old-failed".into(), "nope".into()]), 1);
    assert!(!data.path().join("jobs/old-failed").exists());

    // A tampered id never deletes outside <data>/jobs.
    assert_eq!(e.delete_history(&["../keep.txt".into()]), 1);
    assert!(keep.exists());

    // Survives a restart.
    drop(e);
    let ids: Vec<String> = engine(data.path()).history().into_iter().map(|h| h.job_id).collect();
    assert_eq!(ids, vec!["new-done".to_string()]);
}
