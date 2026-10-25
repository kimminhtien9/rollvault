// Mirrors the serde types of rollvault-core.

export type Kind = "ssd" | "hdd" | "network" | "unknown";
export type Tier = "primary" | "cascade";
export type Algo = "xxh64" | "xxh3" | "xxh128" | "md5";

export interface VolumeInfo {
  mount_point: string;
  name: string;
  fs_type: string;
  device_node: string;
  device_id: string;
  kind: Kind;
  total_bytes: number;
  free_bytes: number;
  block_size: number;
  internal: boolean;
  ejectable: boolean;
  removable: boolean;
  network: boolean;
  read_only: boolean;
  protocol: string | null;
  media_name: string | null;
  volume_uuid: string | null;
  card_vendor: string | null;
  is_camera_card: boolean;
  unresponsive: boolean;
}

export interface RollMeta {
  shoot_date: string;
  project_name: string;
  media_category: string;
  shoot_day: string;
  camera_id: string;
  roll_id: string;
  dit_name: string;
}

export interface DestRequest {
  root: string;
  tier: Tier;
  kind_override: Kind | null;
}

export interface JobRequest {
  source: string;
  destinations: DestRequest[];
  meta: RollMeta;
  roll_template: string;
  report_template: string;
  algo: Algo;
  min_copies: number;
  source_verify: boolean;
  generate_report: boolean;
  report_pdf: boolean;
  branding: { company: string; logo_data_uri: string | null };
  location: string;
  confirm_repeat_card: boolean;
}

export interface DestCheck {
  root: string;
  tier: Tier;
  roll_dir: string;
  volume_name: string;
  device_id: string;
  kind: Kind;
  fs_type: string;
  free_bytes: number;
  reserved_bytes: number;
  needed_bytes: number;
  problems: string[];
}

export interface Preflight {
  ok: boolean;
  errors: string[];
  warnings: string[];
  source_name: string;
  source_device_id: string;
  card_vendor: string | null;
  file_count: number;
  dir_count: number;
  clip_count: number;
  total_bytes: number;
  largest_file: number;
  fingerprint: string;
  roll_rel_path: string;
  dests: DestCheck[];
  distinct_devices: number;
  previously_offloaded: {
    job_id: string;
    camera_id: string;
    roll_id: string;
    finished_at: string | null;
  } | null;
  needs_confirmation: boolean;
}

export type DestPhase = "queued" | "waiting_device" | "copying" | "verifying" | "done" | "failed" | "cancelled";

export type JobPhase =
  | "queued"
  | "scanning"
  | "waiting_device"
  | "copying"
  | "source_verify"
  | "cascading"
  | "reporting"
  | "completed"
  | "completed_with_errors"
  | "failed"
  | "cancelled"
  | "interrupted";

export interface DestSnapshot {
  index: number;
  tier: Tier;
  root: string;
  roll_dir: string;
  volume_name: string;
  kind: Kind;
  device_id: string;
  phase: DestPhase;
  bytes_written: number;
  bytes_verified: number;
  need_bytes: number;
  prior_bytes: number;
  total_bytes: number;
  files_verified: number;
  files_failed: number;
  total_files: number;
  write_speed: number;
  verify_speed: number;
  current_file: string;
  error: string | null;
  mhl_path: string | null;
  legacy_mhl_path: string | null;
  complete: boolean;
}

export interface JobSnapshot {
  id: string;
  meta: RollMeta;
  source: string;
  source_label: string;
  card_vendor: string | null;
  phase: JobPhase;
  status_text: string;
  created_at: string;
  finished_at: string | null;
  total_files: number;
  total_bytes: number;
  source_read: number;
  source_speed: number;
  safe_to_eject: boolean;
  safe_to_eject_at: string | null;
  verified_copies: number;
  min_copies: number;
  dests: DestSnapshot[];
  errors: string[];
  warnings: string[];
  report_path: string | null;
  report_pdf_path: string | null;
  percent: number;
  eta_seconds: number | null;
  resumable: boolean;
  card_ejected: boolean;
  algo: Algo;
}

export interface HistoryEntry {
  job_id: string;
  created_at: string;
  finished_at: string | null;
  status: JobPhase;
  meta: RollMeta;
  source: string;
  source_label: string;
  fingerprint: string;
  file_count: number;
  total_bytes: number;
  dests: { root: string; roll_dir: string; tier: Tier; volume_name: string; complete: boolean }[];
  report_path: string | null;
  safe_to_eject: boolean;
  errors: string[];
}

export interface AppInfo {
  version: string;
  data_dir: string;
  ffmpeg: string | null;
  ffprobe: string | null;
  chrome: string | null;
  hostname: string;
  username: string;
}

export interface VerifyOutcome {
  root: string;
  files_checked: number;
  verified: number;
  failed: string[];
  missing: string[];
  unrecorded: string[];
  manifest: string | null;
  cancelled: boolean;
}

export interface VerifySnapshot {
  id: string;
  root: string;
  status: "running" | "passed" | "failed" | "cancelled";
  bytes_done: number;
  bytes_total: number;
  files_done: number;
  current: string;
  outcome: VerifyOutcome | null;
  error: string | null;
  started_at: string;
  finished_at: string | null;
}

export const ACTIVE_PHASES: JobPhase[] = [
  "queued",
  "scanning",
  "waiting_device",
  "copying",
  "source_verify",
  "cascading",
  "reporting",
];

export function isActive(p: JobPhase): boolean {
  return ACTIVE_PHASES.includes(p);
}
