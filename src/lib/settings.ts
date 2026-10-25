import type { Algo, Kind, Tier, VolumeInfo } from "../types";
import { todayYmd } from "./format";

export const DEFAULT_ROLL_TEMPLATE = "{date}_{project}/{category}/{day}/{camera}/{roll}";
export const DEFAULT_REPORT_TEMPLATE = "{date}_{project}/03_DIT_REPORTS";

export interface Project {
  id: string;
  projectName: string;
  shootDate: string;
  shootDay: string;
  mediaCategory: string;
  /** camera id -> next roll id */
  cameras: Record<string, string>;
  lastCamera: string;
}

export interface Settings {
  version: 1;
  language: "en" | "vi";
  ditName: string;
  company: string;
  logoDataUri: string | null;
  location: string;
  sounds: boolean;
  notifications: boolean;
  rollTemplate: string;
  reportTemplate: string;
  algo: Algo;
  minCopies: number;
  sourceVerify: boolean;
  generateReport: boolean;
  reportPdf: boolean;
  projects: Project[];
  activeProjectId: string | null;
  /** volume uuid (or mount point) -> kind chosen by the user */
  kindOverrides: Record<string, Kind>;
  /** destination path -> tier the user last used */
  destTiers: Record<string, Tier>;
  customDestinations: string[];
}

function uid(): string {
  return Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
}

export function newProject(name = "New project"): Project {
  return {
    id: uid(),
    projectName: name,
    shootDate: todayYmd(),
    shootDay: "Day01",
    mediaCategory: "01_FOOTAGE",
    cameras: {},
    lastCamera: "CAM_A",
  };
}

export function defaultSettings(): Settings {
  const p = newProject("TVC_BrandX");
  return {
    version: 1,
    language: "en",
    ditName: "",
    company: "",
    logoDataUri: null,
    location: "",
    sounds: true,
    notifications: true,
    rollTemplate: DEFAULT_ROLL_TEMPLATE,
    reportTemplate: DEFAULT_REPORT_TEMPLATE,
    algo: "xxh64",
    minCopies: 2,
    sourceVerify: false,
    generateReport: true,
    reportPdf: true,
    projects: [p],
    activeProjectId: p.id,
    kindOverrides: {},
    destTiers: {},
    customDestinations: [],
  };
}

export function normalizeSettings(raw: unknown): Settings {
  const d = defaultSettings();
  if (!raw || typeof raw !== "object") return d;
  const r = raw as Partial<Settings>;
  const merged: Settings = { ...d, ...r, version: 1 };
  if (!Array.isArray(merged.projects) || merged.projects.length === 0) {
    merged.projects = d.projects;
    merged.activeProjectId = d.activeProjectId;
  }
  merged.projects = merged.projects.map((p) => ({ ...newProject(), ...p, cameras: { ...(p.cameras ?? {}) } }));
  if (!merged.projects.some((p) => p.id === merged.activeProjectId)) merged.activeProjectId = merged.projects[0].id;
  merged.kindOverrides = { ...(r.kindOverrides ?? {}) };
  merged.destTiers = { ...(r.destTiers ?? {}) };
  merged.customDestinations = Array.isArray(r.customDestinations) ? r.customDestinations : [];
  if (![1, 2, 3].includes(merged.minCopies)) merged.minCopies = 2;
  if (merged.language !== "vi") merged.language = "en";
  return merged;
}

/** "CAM_B_FX3" -> "B001", "DRONE" -> "D001". */
export function guessFirstRoll(camera: string): string {
  const m = camera.match(/CAM[_\- ]?([A-Za-z])/i);
  const letter = (m ? m[1] : camera.replace(/[^A-Za-z]/g, "")[0] ?? "A").toUpperCase();
  return `${letter}001`;
}

export function volumeKey(v: VolumeInfo): string {
  return v.volume_uuid ?? v.mount_point;
}

export const ALGOS: { id: Algo; label: string; hint: string }[] = [
  { id: "xxh64", label: "xxHash64", hint: "Most widely supported; ASC MHL + MHL v1.1" },
  { id: "xxh128", label: "XXH128", hint: "ASC MHL default; ASC MHL only" },
  { id: "xxh3", label: "XXH3-64", hint: "Fastest on Apple Silicon; ASC MHL only" },
  { id: "md5", label: "MD5", hint: "When a client requires MD5; slower" },
];

export const KIND_LABEL: Record<Kind, string> = {
  ssd: "SSD",
  hdd: "HDD",
  network: "NAS",
  unknown: "Unknown",
};
