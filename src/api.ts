import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { mockInvoke } from "./mock";
import type { AppInfo, HistoryEntry, JobRequest, JobSnapshot, Preflight, RollMeta, VerifySnapshot, VolumeInfo } from "./types";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return inTauri ? tauriInvoke<T>(cmd, args) : mockInvoke<T>(cmd, args);
}

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  listVolumes: () => invoke<VolumeInfo[]>("list_volumes"),
  preflight: (req: JobRequest) => invoke<Preflight>("preflight", { req }),
  startJob: (req: JobRequest) => invoke<{ job_id: string; next_roll: string }>("start_job", { req }),
  cancelJob: (id: string) => invoke<boolean>("cancel_job", { id }),
  dismissJob: (id: string) => invoke<boolean>("dismiss_job", { id }),
  resumeJob: (id: string) => invoke<string>("resume_job", { id }),
  jobs: () => invoke<JobSnapshot[]>("jobs"),
  history: () => invoke<HistoryEntry[]>("history"),
  ejectSource: (id: string) => invoke<void>("eject_source", { id }),
  ejectVolume: (mount: string) => invoke<void>("eject_volume", { mount }),
  reveal: (path: string) => invoke<void>("reveal", { path }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  loadSettings: () => invoke<unknown>("load_settings"),
  saveSettings: (settings: unknown) => invoke<void>("save_settings", { settings }),
  readLogo: (path: string) => invoke<string>("read_logo", { path }),
  renderRollPath: (template: string, meta: RollMeta) => invoke<string>("render_roll_path", { template, meta }),
  nextRoll: (roll: string) => invoke<string>("next_roll", { roll }),
  exportPdf: (html: string) => invoke<string>("export_pdf", { html }),
  quitApp: () => invoke<void>("quit_app"),
  startVerify: (path: string, author: string) => invoke<string>("start_verify", { path, author }),
  verifyTasks: () => invoke<VerifySnapshot[]>("verify_tasks"),
  cancelVerify: (id: string) => invoke<void>("cancel_verify", { id }),
  dismissVerify: (id: string) => invoke<void>("dismiss_verify", { id }),
};

/** Tauri rejects with whatever the command returned as its error value. */
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object") {
    const o = e as { errors?: string[]; message?: string };
    if (Array.isArray(o.errors) && o.errors.length) return o.errors.join("\n");
    if (o.message) return o.message;
  }
  return String(e);
}
