//! Report generation on real (ffmpeg-generated) clips. Skipped when ffmpeg
//! is not installed.

use chrono::Local;
use rollvault_core::hashing::HashAlgo;
use rollvault_core::preset::RollMeta;
use rollvault_core::report::{self, ReportBranding, ReportInput};
use rollvault_core::scan::scan_source;
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[test]
fn report_has_thumbnails_timecode_and_sony_sidecar_data() {
    let Some(ffmpeg) = rollvault_core::tools::ffmpeg() else {
        eprintln!("ffmpeg not installed, skipping");
        return;
    };
    let t = tempfile::tempdir().unwrap();
    let clip_dir = t.path().join("PRIVATE/M4ROOT/CLIP");
    std::fs::create_dir_all(&clip_dir).unwrap();
    let ok = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25", "-t", "3"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-timecode", "10:00:00:00"])
        .arg(clip_dir.join("C0001.MP4"))
        .status()
        .unwrap()
        .success();
    assert!(ok);
    // Second clip without a timecode track: TC must come from the XML sidecar.
    let ok = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25", "-t", "2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(clip_dir.join("C0002.MP4"))
        .status()
        .unwrap()
        .success();
    assert!(ok);
    std::fs::write(
        clip_dir.join("C0002M01.XML"),
        r#"<NonRealTimeMeta><LtcChangeTable tcFps="25"><LtcChange frameCount="0" value="12345601" status="increment"/></LtcChangeTable>
<AcquisitionRecord><Group><Item name="CaptureGammaEquation" value="s-log3-cine"/></Group></AcquisitionRecord>
<Device manufacturer="Sony" modelName="ILME-FX3"/></NonRealTimeMeta>"#,
    )
    .unwrap();

    let scan = scan_source(t.path()).unwrap();
    let hashes: Vec<Option<String>> = scan.files.iter().map(|_| Some("0123456789abcdef".into())).collect();
    let meta = RollMeta {
        shoot_date: "20261025".into(),
        project_name: "Test & <Co>".into(),
        media_category: "01_FOOTAGE".into(),
        shoot_day: "Day01".into(),
        camera_id: "CAM_A".into(),
        roll_id: "A001".into(),
        dit_name: "DIT".into(),
    };
    let cancel = AtomicBool::new(false);
    let input = ReportInput {
        meta: &meta,
        algo: HashAlgo::Xxh64,
        copy_root: t.path(),
        files: &scan.files,
        hashes: &hashes,
        source_label: "Card",
        card_vendor: Some("Sony"),
        copies: vec![],
        started: Local::now(),
        finished: Local::now(),
        safe_to_eject_at: None,
        issues: vec![],
        branding: &ReportBranding::default(),
        tool: "RollVault test",
        cancel: &cancel,
        progress: None,
    };
    let clips = report::collect_clips(&input);
    assert_eq!(clips.len(), 2);
    let c1 = clips.iter().find(|c| c.name == "C0001.MP4").unwrap();
    assert_eq!(c1.timecode, "10:00:00:00");
    assert_eq!(c1.thumbs.len(), 3, "three thumbnails expected");
    assert_eq!(c1.resolution, "640×360");
    assert_eq!(c1.fps, "25");
    let c2 = clips.iter().find(|c| c.name == "C0002.MP4").unwrap();
    assert_eq!(c2.timecode, "01:56:34:12", "TC from the Sony XML sidecar");
    assert_eq!(c2.camera, "ILME-FX3");
    assert_eq!(c2.gamma, "s-log3-cine");

    let html = report::render_html(&input, &clips);
    assert!(html.contains("Test &amp; &lt;Co&gt;"), "text must be HTML-escaped");
    assert!(html.contains("data:image/jpeg;base64,"));
    assert!(!html.contains("<Co>"));
}
