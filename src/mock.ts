// Demo data used only when the UI runs outside Tauri (plain browser), so the
// layout can be previewed and screenshotted without a backend.
import type { AppInfo, HistoryEntry, JobSnapshot, Preflight, VolumeInfo } from "./types";

const GB = 1e9;
const vol = (p: Partial<VolumeInfo>): VolumeInfo => ({
  mount_point: "/Volumes/X",
  name: "X",
  fs_type: "exfat",
  device_node: "/dev/disk9s1",
  device_id: "disk9",
  kind: "ssd",
  total_bytes: 1000 * GB,
  free_bytes: 500 * GB,
  block_size: 4096,
  internal: false,
  ejectable: true,
  removable: false,
  network: false,
  read_only: false,
  protocol: "USB",
  media_name: null,
  volume_uuid: null,
  card_vendor: null,
  is_camera_card: false,
  unresponsive: false,
  ...p,
});

const volumes: VolumeInfo[] = [
  vol({ mount_point: "/Volumes/Untitled", name: "Untitled", device_id: "disk8", total_bytes: 256 * GB, free_bytes: 61 * GB, removable: true, card_vendor: "Sony", is_camera_card: true, volume_uuid: "card" }),
  vol({ mount_point: "/Volumes/SOF SSD 2TB", name: "SOF SSD 2TB", device_id: "disk6", total_bytes: 2000 * GB, free_bytes: 1170 * GB, volume_uuid: "nvme" }),
  vol({ mount_point: "/Volumes/T5", name: "T5", fs_type: "apfs", device_id: "disk4", total_bytes: 1000 * GB, free_bytes: 612 * GB, volume_uuid: "t5" }),
  vol({ mount_point: "/Volumes/ARCHIVE 5TB", name: "ARCHIVE 5TB", fs_type: "hfs", device_id: "disk5", kind: "hdd", total_bytes: 5000 * GB, free_bytes: 3100 * GB, volume_uuid: "hdd" }),
  vol({ mount_point: "/Volumes/Footage", name: "Footage (NAS)", fs_type: "smbfs", device_id: "net:nas.local", kind: "network", network: true, ejectable: false, total_bytes: 16000 * GB, free_bytes: 9000 * GB, volume_uuid: null }),
  vol({ mount_point: "/System/Volumes/Data", name: "Macintosh HD (internal)", fs_type: "apfs", device_id: "disk0", internal: true, ejectable: false, total_bytes: 494 * GB, free_bytes: 142 * GB }),
];

const meta = (roll: string, cam: string) => ({
  shoot_date: "20261025",
  project_name: "TVC_BrandX",
  media_category: "01_FOOTAGE",
  shoot_day: "Day01",
  camera_id: cam,
  roll_id: roll,
  dit_name: "Alex Morgan",
});

const now = new Date().toISOString();
const dest = (i: number, tier: "primary" | "cascade", name: string, kind: "ssd" | "hdd", phase: JobSnapshot["dests"][0]["phase"], w: number, v: number, extra: Partial<JobSnapshot["dests"][0]> = {}) => ({
  index: i,
  tier,
  root: `/Volumes/${name}`,
  roll_dir: `/Volumes/${name}/20261025_TVC_BrandX/01_FOOTAGE/Day01/CAM_A_FX6/A003`,
  volume_name: name,
  kind,
  device_id: `disk${i + 4}`,
  phase,
  bytes_written: w,
  bytes_verified: v,
  need_bytes: 194 * GB,
  prior_bytes: 0,
  total_bytes: 194 * GB,
  files_verified: Math.round((v / (194 * GB)) * 412),
  files_failed: 0,
  total_files: 412,
  write_speed: phase === "copying" || phase === "verifying" ? 268e6 : 0,
  verify_speed: phase === "copying" || phase === "verifying" ? 251e6 : 0,
  current_file: "PRIVATE/M4ROOT/CLIP/C0287.MP4",
  error: null,
  mhl_path: null,
  legacy_mhl_path: null,
  complete: phase === "done",
  ...extra,
});

const jobs: JobSnapshot[] = [
  {
    id: "job-a003",
    meta: meta("A003", "CAM_A_FX6"),
    source: "/Volumes/Untitled",
    source_label: "Untitled",
    card_vendor: "Sony",
    phase: "copying",
    status_text: "Copying and verifying from the card",
    created_at: now,
    finished_at: null,
    total_files: 412,
    total_bytes: 194 * GB,
    source_read: 121 * GB,
    source_speed: 271e6,
    safe_to_eject: false,
    safe_to_eject_at: null,
    verified_copies: 0,
    min_copies: 2,
    dests: [
      dest(0, "primary", "SOF SSD 2TB", "ssd", "copying", 121 * GB, 117 * GB),
      dest(1, "primary", "T5", "ssd", "verifying", 121 * GB, 113 * GB),
      dest(2, "cascade", "ARCHIVE 5TB", "hdd", "queued", 0, 0),
    ],
    errors: [],
    warnings: [],
    report_path: null,
    report_pdf_path: null,
    percent: 30.1,
    eta_seconds: 1420,
    resumable: false,
    card_ejected: false,
    algo: "xxh64",
  },
  {
    id: "job-a002",
    meta: meta("A002", "CAM_A_FX6"),
    source: "/Volumes/Untitled 1",
    source_label: "Untitled 1",
    card_vendor: "Sony",
    phase: "cascading",
    status_text: "Cascading from SOF SSD 2TB to 1 drive(s)",
    created_at: new Date(Date.now() - 3600e3).toISOString(),
    finished_at: null,
    total_files: 388,
    total_bytes: 181 * GB,
    source_read: 181 * GB,
    source_speed: 0,
    safe_to_eject: true,
    safe_to_eject_at: new Date(Date.now() - 1200e3).toISOString(),
    verified_copies: 2,
    min_copies: 2,
    dests: [
      dest(0, "primary", "SOF SSD 2TB", "ssd", "done", 181 * GB, 181 * GB, { total_bytes: 181 * GB, need_bytes: 181 * GB, files_verified: 388, total_files: 388 }),
      dest(1, "primary", "T5", "ssd", "done", 181 * GB, 181 * GB, { total_bytes: 181 * GB, need_bytes: 181 * GB, files_verified: 388, total_files: 388 }),
      dest(2, "cascade", "ARCHIVE 5TB", "hdd", "copying", 97 * GB, 0, { total_bytes: 181 * GB, need_bytes: 181 * GB, files_verified: 0, total_files: 388, write_speed: 182e6, verify_speed: 0 }),
    ],
    errors: [],
    warnings: [],
    report_path: null,
    report_pdf_path: null,
    percent: 76.8,
    eta_seconds: 1290,
    resumable: false,
    card_ejected: false,
    algo: "xxh64",
  },
];

const preflight: Preflight = {
  ok: true,
  errors: [],
  warnings: ["Footage (NAS): a network drive may serve verification reads from this Mac's cache. Run Re-verify on the NAS copy afterwards (from History)."],
  source_name: "Untitled",
  source_device_id: "disk8",
  card_vendor: "Sony",
  file_count: 412,
  dir_count: 9,
  clip_count: 136,
  total_bytes: 194 * GB,
  largest_file: 4.1 * GB,
  fingerprint: "x",
  roll_rel_path: "20261025_TVC_BrandX/01_FOOTAGE/Day01/CAM_A_FX6/A004",
  dests: [],
  distinct_devices: 3,
  previously_offloaded: null,
  needs_confirmation: false,
};

const info: AppInfo = {
  version: "0.1.0",
  data_dir: "~/Library/Application Support/com.rollvault.desktop",
  ffmpeg: "/opt/homebrew/bin/ffmpeg",
  ffprobe: "/opt/homebrew/bin/ffprobe",
  chrome: "/Applications/Google Chrome.app",
  hostname: "Mac-mini.local",
  username: "dit",
};

const history: HistoryEntry[] = [];

let settings: unknown = {
  ditName: "Alex Morgan",
  language: new URLSearchParams(window.location.search).get("lang") === "vi" ? "vi" : "en",
  destTiers: { "/Volumes/SOF SSD 2TB": "primary", "/Volumes/T5": "primary", "/Volumes/ARCHIVE 5TB": "cascade" },
  projects: [{ id: "p1", projectName: "TVC_BrandX", shootDate: "20261025", shootDay: "Day01", mediaCategory: "01_FOOTAGE", cameras: { CAM_A_FX6: "A004" }, lastCamera: "CAM_A_FX6" }],
  activeProjectId: "p1",
};

export async function mockInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const r = (v: unknown) => Promise.resolve(v as T);
  switch (cmd) {
    case "app_info":
      return r(info);
    case "list_volumes":
      return r(volumes);
    case "jobs":
      return r(jobs);
    case "history":
      return r(history);
    case "delete_history": {
      const ids = (args?.ids ?? []) as string[];
      const before = history.length;
      for (let i = history.length - 1; i >= 0; i--) if (ids.includes(history[i].job_id)) history.splice(i, 1);
      return r(before - history.length);
    }
    case "verify_tasks":
      return r([]);
    case "preflight":
      return r(preflight);
    case "load_settings":
      return r(settings);
    case "save_settings":
      settings = args?.settings;
      return r(undefined);
    case "render_roll_path": {
      const m = (args?.meta ?? {}) as Record<string, string>;
      return r(`${m.shoot_date}_${m.project_name}/${m.media_category}/${m.shoot_day}/${m.camera_id}/${m.roll_id}`);
    }
    default:
      return r(undefined);
  }
}
