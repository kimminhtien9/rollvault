mod common;

use common::*;
use rollvault_core::hashing::{HashAlgo, hash_bytes};
use rollvault_core::scan::scan_source;
use rollvault_core::transfer::*;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn targets(roots: &[&Path], defer: bool) -> Vec<DestTarget> {
    roots
        .iter()
        .map(|r| DestTarget { roll_dir: r.to_path_buf(), defer_verify: defer, progress: Arc::new(DestProgress::default()) })
        .collect()
}

fn run_with(
    src: &Path,
    dests: &[DestTarget],
    faults: Option<Arc<FaultInjector>>,
    expected: Option<&[Option<String>]>,
    cancel: Arc<AtomicBool>,
    stall: Duration,
) -> (rollvault_core::scan::SourceScan, TransferOutcome) {
    let scan = scan_source(src).unwrap();
    let work = vec![(0..dests.len()).collect::<Vec<_>>(); scan.files.len()];
    let req = TransferRequest {
        source_root: src.to_path_buf(),
        files: Arc::new(scan.files.clone()),
        work,
        dests: dests.to_vec(),
        algo: HashAlgo::Xxh64,
        expected: expected.map(|e| Arc::new(e.to_vec())),
        cancel,
        source_progress: Arc::new(SourceProgress::default()),
        listener: Arc::new(NoopListener),
        faults,
        chunk_size: DEFAULT_CHUNK,
        retry_failed: true,
        stall_timeout: stall,
    };
    let out = run_transfer(&req);
    (scan, out)
}

fn run(
    src: &Path,
    dests: &[DestTarget],
    faults: Option<&Arc<FaultInjector>>,
    expected: Option<&[Option<String>]>,
    cancel: &Arc<AtomicBool>,
) -> (rollvault_core::scan::SourceScan, TransferOutcome) {
    run_with(src, dests, faults.cloned(), expected, cancel.clone(), DEFAULT_STALL)
}

#[test]
fn copies_every_file_byte_identical_and_preserves_mtime() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    let files = make_sony_card(&card);
    let d0 = t.path().join("ssd1/A001");
    let d1 = t.path().join("ssd2/A001");
    let dests = targets(&[&d0, &d1], false);
    let cancel = Arc::new(AtomicBool::new(false));
    let (scan, out) = run(&card, &dests, None, None, &cancel);

    assert_eq!(scan.files.len(), files.len());
    assert!(out.source_errors.is_empty(), "{:?}", out.source_errors);
    for (d, root) in [&d0, &d1].iter().enumerate() {
        let o = &out.dests[d];
        assert!(o.fatal.is_none(), "{:?}", o.fatal);
        assert!(o.failures.is_empty(), "{:?}", o.failures);
        for (fi, f) in scan.files.iter().enumerate() {
            let a = card.join(&f.rel);
            let b = root.join(&f.rel);
            assert!(files_equal(&a, &b), "content differs: {}", f.rel);
            let ma = std::fs::metadata(&a).unwrap().modified().unwrap();
            let mb = std::fs::metadata(&b).unwrap().modified().unwrap();
            assert_eq!(ma, mb, "mtime differs: {}", f.rel);
            let expect = hash_bytes(HashAlgo::Xxh64, &std::fs::read(&a).unwrap());
            assert_eq!(o.verified[fi].as_deref(), Some(expect.as_str()));
            assert_eq!(out.source_hashes[fi].as_deref(), Some(expect.as_str()));
        }
        assert!(find_partials(root).is_empty());
        assert!(!root.join(".DS_Store").exists(), "junk must not be copied");
        assert!(!root.join("PRIVATE/M4ROOT/CLIP/._C0001.MP4").exists());
    }
    let p = dests[0].progress.files_verified.load(Ordering::Relaxed);
    assert_eq!(p as usize, scan.files.len());
}

#[test]
fn transient_corruption_is_retried_and_ends_verified() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    make_sony_card(&card);
    let d0 = t.path().join("a/A001");
    let d1 = t.path().join("b/A001");
    let dests = targets(&[&d0, &d1], false);
    let faults = Arc::new(FaultInjector::default());
    faults.corrupt_dest_once.lock().unwrap().insert((1, "PRIVATE/M4ROOT/CLIP/C0003.MP4".into()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (scan, out) = run(&card, &dests, Some(&faults), None, &cancel);
    assert!(out.dests[1].failures.is_empty(), "{:?}", out.dests[1].failures);
    assert!(out.dests[1].verified.iter().all(|v| v.is_some()));
    assert!(out.source_unstable.is_empty());
    for f in &scan.files {
        assert!(files_equal(&card.join(&f.rel), &d1.join(&f.rel)));
    }
}

#[test]
fn persistent_corruption_fails_only_that_file_on_that_destination() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    make_sony_card(&card);
    let d0 = t.path().join("a/A001");
    let d1 = t.path().join("b/A001");
    let dests = targets(&[&d0, &d1], false);
    let bad = "PRIVATE/M4ROOT/CLIP/C0002.MP4";
    let faults = Arc::new(FaultInjector::default());
    faults.corrupt_dest_always.lock().unwrap().insert((1, bad.into()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (scan, out) = run(&card, &dests, Some(&faults), None, &cancel);
    let bad_idx = scan.files.iter().position(|f| f.rel == bad).unwrap();
    assert!(out.dests[0].failures.is_empty());
    assert!(out.dests[0].verified.iter().all(|v| v.is_some()));
    assert!(out.dests[1].failures.contains_key(&bad_idx));
    assert!(out.dests[1].failures[&bad_idx].contains("HASH MISMATCH"));
    assert!(out.dests[1].verified[bad_idx].is_none());
    assert!(!d1.join(bad).exists(), "a corrupt file must never get its real name");
    assert!(find_partials(&d1).is_empty());
    assert_eq!(out.dests[1].verified.iter().filter(|v| v.is_some()).count(), scan.files.len() - 1);
}

#[test]
fn failing_destination_is_dropped_and_others_finish() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    make_sony_card(&card);
    let d0 = t.path().join("a/A001");
    let d1 = t.path().join("b/A001");
    let dests = targets(&[&d0, &d1], false);
    let faults = Arc::new(FaultInjector::default());
    faults.fail_dest_open.lock().unwrap().insert((0, "PRIVATE/M4ROOT/CLIP/C0002.MP4".into()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (scan, out) = run(&card, &dests, Some(&faults), None, &cancel);
    assert!(out.dests[0].fatal.is_some());
    assert!(out.dests[1].fatal.is_none());
    assert_eq!(out.dests[1].verified.iter().filter(|v| v.is_some()).count(), scan.files.len());
    assert!(find_partials(&d0).is_empty());
}

#[test]
fn cascade_with_wrong_expected_hash_never_verifies() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    make_sony_card(&card);
    let scan = scan_source(&card).unwrap();
    // Pretend the original card hash of one file was different: the copy we
    // are cascading from is corrupt and must be rejected.
    let mut expected: Vec<Option<String>> = scan
        .files
        .iter()
        .map(|f| Some(hash_bytes(HashAlgo::Xxh64, &std::fs::read(card.join(&f.rel)).unwrap())))
        .collect();
    expected[2] = Some("0000000000000000".into());
    let hdd = t.path().join("hdd/A001");
    let dests = targets(&[&hdd], true);
    let cancel = Arc::new(AtomicBool::new(false));
    let (_, out) = run(&card, &dests, None, Some(&expected), &cancel);
    assert!(out.dests[0].failures.contains_key(&2));
    assert!(out.dests[0].failures[&2].contains("original"));
    assert!(!hdd.join(&scan.files[2].rel).exists());
    assert_eq!(out.dests[0].verified.iter().filter(|v| v.is_some()).count(), scan.files.len() - 1);
}

#[test]
fn deferred_verification_matches_immediate() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    make_sony_card(&card);
    let hdd = t.path().join("hdd/A001");
    let dests = targets(&[&hdd], true);
    let cancel = Arc::new(AtomicBool::new(false));
    let (scan, out) = run(&card, &dests, None, None, &cancel);
    assert!(out.dests[0].failures.is_empty());
    assert_eq!(out.dests[0].verified.iter().filter(|v| v.is_some()).count(), scan.files.len());
}

#[test]
fn cancel_leaves_only_verified_files() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    make_sony_card(&card);
    let d0 = t.path().join("a/A001");
    let dests = targets(&[&d0], false);
    let faults = Arc::new(FaultInjector::default());
    *faults.chunk_delay.lock().unwrap() = Some(Duration::from_millis(40));
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        c2.store(true, Ordering::Relaxed);
    });
    let (scan, out) = run(&card, &dests, Some(&faults), None, &cancel);
    stopper.join().unwrap();
    assert!(out.cancelled);
    assert!(find_partials(&d0).is_empty(), "partials left: {:?}", find_partials(&d0));
    for (fi, f) in scan.files.iter().enumerate() {
        let exists = d0.join(&f.rel).exists();
        assert_eq!(exists, out.dests[0].verified[fi].is_some(), "{}", f.rel);
        if exists {
            assert!(files_equal(&card.join(&f.rel), &d0.join(&f.rel)));
        }
    }
    assert!(out.dests[0].verified.iter().any(|v| v.is_none()), "cancel came too late to test anything");
}

#[test]
fn disk_full_drops_that_destination_only() {
    let t = tempfile::tempdir().unwrap();
    let Some(img) = DiskImage::create(t.path(), "rvfull", 12) else {
        eprintln!("hdiutil unavailable, skipping");
        return;
    };
    let card = t.path().join("card");
    make_sony_card(&card);
    let full = img.mount.join("A001");
    let ok = t.path().join("ok/A001");
    let dests = targets(&[&full, &ok], false);
    let cancel = Arc::new(AtomicBool::new(false));
    let (scan, out) = run(&card, &dests, None, None, &cancel);
    let fatal = out.dests[0].fatal.clone().unwrap_or_default();
    assert!(fatal.contains("disk full"), "expected disk-full error, got: {fatal}");
    assert!(out.dests[1].fatal.is_none());
    assert_eq!(out.dests[1].verified.iter().filter(|v| v.is_some()).count(), scan.files.len());
    assert!(find_partials(&full).is_empty());
}

#[test]
fn unplugged_destination_mid_copy_is_reported_not_panicking() {
    let t = tempfile::tempdir().unwrap();
    let Some(mut img) = DiskImage::create(t.path(), "rvunplug", 200) else {
        eprintln!("hdiutil unavailable, skipping");
        return;
    };
    let card = t.path().join("card");
    make_sony_card(&card);
    let gone = img.mount.join("A001");
    let ok = t.path().join("ok/A001");
    let dests = targets(&[&gone, &ok], false);
    let faults = Arc::new(FaultInjector::default());
    *faults.chunk_delay.lock().unwrap() = Some(Duration::from_millis(60));
    let cancel = Arc::new(AtomicBool::new(false));
    let mount = img.mount.clone();
    let unplug = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        let _ = std::process::Command::new("hdiutil").args(["detach", "-quiet", "-force"]).arg(&mount).status();
    });
    let (scan, out) = run(&card, &dests, Some(&faults), None, &cancel);
    unplug.join().unwrap();
    img.detach_force();
    assert!(out.dests[0].fatal.is_some(), "unplugged destination must be fatal");
    assert!(out.dests[1].fatal.is_none());
    assert_eq!(out.dests[1].verified.iter().filter(|v| v.is_some()).count(), scan.files.len());
}

#[test]
fn hung_destination_is_abandoned_and_others_finish() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    make_sony_card(&card);
    let d0 = t.path().join("a/A001");
    let d1 = t.path().join("b/A001");
    let dests = targets(&[&d0, &d1], false);
    let faults = Arc::new(FaultInjector::default());
    faults.hang_dest_write.lock().unwrap().insert((1, "PRIVATE/M4ROOT/CLIP/C0002.MP4".into()));
    let started = std::time::Instant::now();
    let (scan, out) = run_with(&card, &dests, Some(faults), None, Arc::new(AtomicBool::new(false)), Duration::from_secs(1));
    assert!(started.elapsed() < Duration::from_secs(30), "transfer waited on a hung drive: {:?}", started.elapsed());
    assert!(out.dests[1].fatal.as_deref().unwrap_or("").contains("stopped responding"), "{:?}", out.dests[1].fatal);
    assert!(out.dests[0].fatal.is_none());
    assert_eq!(out.dests[0].verified.iter().filter(|v| v.is_some()).count(), scan.files.len());
}

#[test]
fn hung_source_is_abandoned() {
    let t = tempfile::tempdir().unwrap();
    let card = t.path().join("card");
    make_sony_card(&card);
    let d0 = t.path().join("a/A001");
    let dests = targets(&[&d0], false);
    let faults = Arc::new(FaultInjector::default());
    faults.hang_source_read.lock().unwrap().insert("PRIVATE/M4ROOT/CLIP/C0003.MP4".into());
    let started = std::time::Instant::now();
    let (_, out) = run_with(&card, &dests, Some(faults), None, Arc::new(AtomicBool::new(false)), Duration::from_secs(1));
    assert!(started.elapsed() < Duration::from_secs(30), "transfer waited on a hung card: {:?}", started.elapsed());
    assert!(out.source_lost);
    assert!(out.source_errors.values().any(|e| e.contains("Source stopped responding")), "{:?}", out.source_errors);
    assert!(find_partials(&d0).is_empty());
}
