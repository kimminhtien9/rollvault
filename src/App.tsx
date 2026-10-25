import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { Coffee, ScrollText, Settings as SettingsIcon, ShieldCheck, ShieldEllipsis } from "lucide-react";
import { api, errorText, inTauri } from "./api";
import type { AppInfo, HistoryEntry, JobRequest, JobSnapshot, Kind, Preflight, RollMeta, Tier, VerifySnapshot, VolumeInfo } from "./types";
import { isActive } from "./types";
import type { Project, Settings } from "./lib/settings";
import { guessFirstRoll, normalizeSettings, volumeKey } from "./lib/settings";
import { basename } from "./lib/format";
import { DICTS, I18nProvider } from "./i18n";
import { DestStep, type DestRow, RollStep, SourceStep, StartBar } from "./components/Setup";
import { JobCard } from "./components/JobCard";
import { VerifyCard } from "./components/VerifyCard";
import { HistoryDrawer } from "./components/HistoryDrawer";
import { SettingsDrawer } from "./components/SettingsDrawer";
import { Button, Modal, Segmented } from "./components/ui";

type Toast = { text: string; tone: "good" | "bad" | "info" };

function volumeOf(path: string, volumes: VolumeInfo[]): VolumeInfo | null {
  let best: VolumeInfo | null = null;
  for (const v of volumes) {
    const mp = v.mount_point.endsWith("/") ? v.mount_point : `${v.mount_point}/`;
    if ((path === v.mount_point || path.startsWith(mp)) && (!best || v.mount_point.length > best.mount_point.length)) best = v;
  }
  if (!best && path.startsWith("/") && !path.startsWith("/Volumes/")) {
    best = volumes.find((v) => v.mount_point === "/System/Volumes/Data") ?? null;
  }
  return best;
}

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [volumes, setVolumes] = useState<VolumeInfo[]>([]);
  const [volumesLoading, setVolumesLoading] = useState(true);
  const [jobs, setJobs] = useState<JobSnapshot[]>([]);
  const [verifies, setVerifies] = useState<VerifySnapshot[]>([]);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [source, setSource] = useState("");
  const [camera, setCamera] = useState("CAM_A");
  const [roll, setRoll] = useState("A001");
  const [pathPreview, setPathPreview] = useState("");
  const [preflight, setPreflight] = useState<Preflight | null>(null);
  const [pfLoading, setPfLoading] = useState(false);
  const [starting, setStarting] = useState(false);
  const [drawer, setDrawer] = useState<"history" | "settings" | null>(null);
  const [askClose, setAskClose] = useState(false);
  const [askRepeat, setAskRepeat] = useState(false);
  const [toast, setToast] = useState<Toast | null>(null);
  const knownMounts = useRef<Set<string> | null>(null);
  const pfSeq = useRef(0);
  const sourceRef = useRef(source);
  sourceRef.current = source;

  const lang = settings?.language ?? "en";
  const t = DICTS[lang];
  const tRef = useRef(t);
  tRef.current = t;

  const notify = useCallback((text: string, tone: Toast["tone"] = "info") => {
    setToast({ text, tone });
    window.setTimeout(() => setToast((x) => (x?.text === text ? null : x)), tone === "bad" ? 9000 : 4500);
  }, []);

  // ---- loading & events --------------------------------------------------
  const refreshVolumes = useCallback(async () => {
    setVolumesLoading(true);
    try {
      const list = await api.listVolumes();
      setVolumes(list);
      const mounts = new Set(list.map((v) => v.mount_point));
      if (!inTauri && !sourceRef.current) {
        const demo = list.find((v) => v.is_camera_card);
        if (demo) setSource(demo.mount_point);
      }
      if (knownMounts.current) {
        const fresh = list.find((v) => v.is_camera_card && !knownMounts.current!.has(v.mount_point));
        if (fresh && !sourceRef.current) setSource(fresh.mount_point);
      }
      knownMounts.current = mounts;
    } catch (e) {
      notify(tRef.current.toastVolumesError(errorText(e)), "bad");
    } finally {
      setVolumesLoading(false);
    }
  }, [notify]);

  useEffect(() => {
    api.appInfo().then(setInfo).catch(() => {});
    api
      .loadSettings()
      .then((raw) => {
        const s = normalizeSettings(raw);
        setSettings(s);
        const p = s.projects.find((x) => x.id === s.activeProjectId) ?? s.projects[0];
        const cam = p.lastCamera || "CAM_A";
        setCamera(cam);
        setRoll(p.cameras[cam] ?? guessFirstRoll(cam));
      })
      .catch(() => setSettings(normalizeSettings(null)));
    refreshVolumes();
    api.jobs().then(setJobs).catch(() => {});
    api.verifyTasks().then((v) => setVerifies(v ?? [])).catch(() => {});
    api.history().then(setHistory).catch(() => {});
    if (!inTauri) return;
    const un = [
      listen<JobSnapshot[]>("jobs", (e) => setJobs(e.payload)),
      listen<VerifySnapshot[]>("verifies", (e) => setVerifies(e.payload)),
      listen("volumes-changed", () => refreshVolumes()),
      listen("close-requested", () => setAskClose(true)),
    ];
    return () => {
      un.forEach((p) => p.then((f) => f()));
    };
  }, [refreshVolumes]);

  // Persist settings (debounced; the first load is not written back).
  const firstSave = useRef(true);
  useEffect(() => {
    if (!settings) return;
    if (firstSave.current) {
      firstSave.current = false;
      return;
    }
    const h = window.setTimeout(() => {
      api.saveSettings(settings).catch((e) => notify(tRef.current.toastSettingsError(errorText(e)), "bad"));
    }, 300);
    return () => window.clearTimeout(h);
  }, [settings, notify]);

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  const finishedKey = jobs.filter((j) => !isActive(j.phase)).map((j) => j.id + j.phase).join(",");
  useEffect(() => {
    api.history().then(setHistory).catch(() => {});
  }, [finishedKey]);

  // ---- derived -----------------------------------------------------------
  const project: Project | null = settings ? (settings.projects.find((p) => p.id === settings.activeProjectId) ?? settings.projects[0]) : null;

  const patchSettings = useCallback((patch: Partial<Settings>) => {
    setSettings((s) => (s ? { ...s, ...patch } : s));
  }, []);

  const patchProject = useCallback((patch: Partial<Project>) => {
    setSettings((s) => (s ? { ...s, projects: s.projects.map((p) => (p.id === s.activeProjectId ? { ...p, ...patch } : p)) } : s));
  }, []);

  const meta: RollMeta | null = useMemo(
    () =>
      project && settings
        ? {
            shoot_date: project.shootDate,
            project_name: project.projectName,
            media_category: project.mediaCategory,
            shoot_day: project.shootDay,
            camera_id: camera,
            roll_id: roll,
            dit_name: settings.ditName,
          }
        : null,
    [project, settings, camera, roll],
  );

  const sourceVolume = source ? volumeOf(source, volumes) : null;

  const destRows: DestRow[] = useMemo(() => {
    if (!settings) return [];
    const rows: DestRow[] = [];
    for (const v of volumes) {
      if ((sourceVolume && v.mount_point === sourceVolume.mount_point) || v.is_camera_card) continue;
      const key = volumeKey(v);
      rows.push({ path: v.mount_point, name: v.name, volume: v, kind: settings.kindOverrides[key] ?? v.kind, kindKey: key, custom: false });
    }
    for (const path of settings.customDestinations) {
      const v = volumeOf(path, volumes);
      const key = v ? volumeKey(v) : null;
      rows.push({
        path,
        name: `${basename(path)}${v ? ` · ${v.name}` : ""}`,
        volume: v,
        kind: (key && settings.kindOverrides[key]) || v?.kind || "unknown",
        kindKey: key,
        custom: true,
      });
    }
    return rows;
  }, [volumes, settings, sourceVolume]);

  const activeTiers: Record<string, Tier> = useMemo(() => {
    const out: Record<string, Tier> = {};
    if (!settings) return out;
    for (const r of destRows) {
      const tier = settings.destTiers[r.path];
      if (tier && !r.volume?.unresponsive) out[r.path] = tier;
    }
    return out;
  }, [destRows, settings]);

  const request: JobRequest | null = useMemo(() => {
    if (!settings || !meta) return null;
    return {
      source,
      destinations: destRows
        .filter((r) => activeTiers[r.path])
        .map((r) => ({
          root: r.path,
          tier: activeTiers[r.path],
          kind_override: (r.kindKey && settings.kindOverrides[r.kindKey]) || (r.kind !== "unknown" ? r.kind : null),
        })),
      meta,
      roll_template: settings.rollTemplate,
      report_template: settings.reportTemplate,
      algo: settings.algo,
      min_copies: settings.minCopies,
      source_verify: settings.sourceVerify,
      generate_report: settings.generateReport,
      report_pdf: settings.reportPdf,
      branding: { company: settings.company, logo_data_uri: settings.logoDataUri },
      location: settings.location,
      confirm_repeat_card: false,
    };
  }, [settings, meta, destRows, activeTiers, source]);

  const ready = !!request && !!source && request.destinations.some((d) => d.tier === "primary");
  const hint = !source ? t.hintNoSource : !request?.destinations.some((d) => d.tier === "primary") ? t.hintNoTier1 : "";

  useEffect(() => {
    if (!settings || !meta) return;
    let alive = true;
    api
      .renderRollPath(settings.rollTemplate, meta)
      .then((p) => alive && setPathPreview(p))
      .catch((e) => alive && setPathPreview(`⚠ ${errorText(e)}`));
    return () => {
      alive = false;
    };
  }, [settings?.rollTemplate, meta]); // eslint-disable-line react-hooks/exhaustive-deps

  // Preflight: debounced, stale results ignored, re-run when language changes.
  const requestKey = JSON.stringify(request);
  const activeKey = jobs.map((j) => j.id + j.phase).join(",");
  useEffect(() => {
    if (!ready || !request) {
      setPreflight(null);
      setPfLoading(false);
      return;
    }
    const seq = ++pfSeq.current;
    setPfLoading(true);
    const h = window.setTimeout(() => {
      api
        .preflight(request)
        .then((pf) => seq === pfSeq.current && setPreflight(pf))
        .catch((e) => seq === pfSeq.current && notify(errorText(e), "bad"))
        .finally(() => seq === pfSeq.current && setPfLoading(false));
    }, 450);
    return () => window.clearTimeout(h);
  }, [requestKey, ready, activeKey, lang]); // eslint-disable-line react-hooks/exhaustive-deps

  const busySources = useMemo(() => {
    const s = new Set<string>();
    for (const j of jobs) {
      if (["queued", "scanning", "waiting_device", "copying", "source_verify"].includes(j.phase)) {
        const v = volumeOf(j.source, volumes);
        s.add(v ? v.mount_point : j.source);
      }
    }
    return s;
  }, [jobs, volumes]);

  // ---- actions -----------------------------------------------------------
  const guard = async (fn: () => Promise<unknown>, ok?: string) => {
    try {
      await fn();
      if (ok) notify(ok, "good");
    } catch (e) {
      notify(errorText(e), "bad");
    }
  };

  const changeCamera = (cam: string) => {
    setCamera(cam);
    if (project) {
      setRoll(project.cameras[cam] ?? guessFirstRoll(cam));
      patchProject({ lastCamera: cam });
    }
  };

  const switchProject = (id: string) => {
    if (!settings) return;
    const p = settings.projects.find((x) => x.id === id);
    patchSettings({ activeProjectId: id });
    if (p) {
      const cam = p.lastCamera || "CAM_A";
      setCamera(cam);
      setRoll(p.cameras[cam] ?? guessFirstRoll(cam));
    }
  };

  const setTier = (path: string, tier: "off" | Tier) => {
    if (!settings) return;
    const next = { ...settings.destTiers };
    if (tier === "off") delete next[path];
    else next[path] = tier;
    patchSettings({ destTiers: next });
  };

  const setKind = (key: string, kind: Kind) => settings && patchSettings({ kindOverrides: { ...settings.kindOverrides, [key]: kind } });

  const pickSourceFolder = async () => {
    const p = await open({ directory: true, multiple: false, title: t.pickSourceTitle });
    if (typeof p === "string") setSource(p);
  };

  const addDestFolder = async () => {
    if (!settings) return;
    const p = await open({ directory: true, multiple: false, title: t.pickDestTitle });
    if (typeof p !== "string" || settings.customDestinations.includes(p)) return;
    const v = volumeOf(p, volumes);
    patchSettings({ customDestinations: [...settings.customDestinations, p], destTiers: { ...settings.destTiers, [p]: v?.network ? "cascade" : "primary" } });
  };

  const removeCustom = (path: string) => {
    if (!settings) return;
    const next = { ...settings.destTiers };
    delete next[path];
    patchSettings({ customDestinations: settings.customDestinations.filter((x) => x !== path), destTiers: next });
  };

  const doStart = async (confirmRepeat: boolean) => {
    if (!request || !project) return;
    setAskRepeat(false);
    setStarting(true);
    const startedRoll = roll;
    const startedCamera = camera;
    try {
      const r = await api.startJob({ ...request, confirm_repeat_card: confirmRepeat });
      patchProject({ cameras: { ...project.cameras, [startedCamera]: r.next_roll }, lastCamera: startedCamera });
      setRoll(r.next_roll);
      setSource("");
      setPreflight(null);
      notify(t.toastStarted(startedCamera, startedRoll, r.next_roll), "good");
    } catch (e) {
      const pf = e as Preflight;
      if (pf && Array.isArray(pf.errors)) setPreflight(pf);
      notify(errorText(e), "bad");
    } finally {
      setStarting(false);
    }
  };

  const start = () => (preflight?.needs_confirmation ? setAskRepeat(true) : doStart(false));

  const startVerify = (path: string) =>
    guard(async () => {
      await api.startVerify(path, settings?.ditName ?? "");
      setVerifies(await api.verifyTasks());
      setDrawer(null);
    }, t.toastVerifying);

  const pickVerifyFolder = async () => {
    const p = await open({ directory: true, multiple: false, title: t.pickVerifyTitle });
    if (typeof p === "string") startVerify(p);
  };

  const pickLogo = async () => {
    const p = await open({ multiple: false, title: t.pickLogoTitle, filters: [{ name: t.images, extensions: ["png", "jpg", "jpeg", "svg", "webp"] }] });
    if (typeof p === "string") guard(async () => patchSettings({ logoDataUri: await api.readLogo(p) }));
  };

  const activeJobs = jobs.filter((j) => isActive(j.phase)).length;
  const activeCount = activeJobs + verifies.filter((v) => v.status === "running").length;
  const activeIds = new Set(jobs.filter((j) => isActive(j.phase)).map((j) => j.id));
  const sortedJobs = [...jobs].sort((a, b) => (isActive(a.phase) ? 0 : 1) - (isActive(b.phase) ? 0 : 1) || b.created_at.localeCompare(a.created_at));

  if (!settings || !project) {
    return (
      <div role="status" className="flex h-full items-center justify-center text-muted">
        {t.loading}
      </div>
    );
  }

  return (
    <I18nProvider lang={lang}>
      <div className="flex h-full flex-col">
        <header data-tauri-drag-region className="flex h-[52px] shrink-0 items-center gap-3 border-b border-line bg-surface pr-3 pl-[86px]">
          <div data-tauri-drag-region className="flex items-center gap-2.5">
            <div className="flex h-7 w-7 items-center justify-center rounded-lg bg-primary" aria-hidden="true">
              <ShieldCheck size={16} className="text-white" />
            </div>
            <div data-tauri-drag-region className="leading-tight">
              <h1 className="text-[14px] font-bold tracking-tight">RollVault</h1>
              <div className="text-[11px] text-faint">{t.appTagline}</div>
            </div>
          </div>
          <div data-tauri-drag-region className="flex-1 self-stretch" />
          {activeCount > 0 && (
            <span role="status" className="flex items-center gap-1.5 rounded-full border border-link/30 bg-info-soft px-2.5 py-1 text-[12px] font-semibold text-link">
              <Coffee size={13} aria-hidden="true" /> {t.runningBadge(activeCount)}
            </span>
          )}
          <Segmented
            size="sm"
            ariaLabel={t.language}
            value={lang}
            onChange={(v) => patchSettings({ language: v })}
            options={[
              { value: "en", label: "EN", title: "English" },
              { value: "vi", label: "VI", title: "Tiếng Việt" },
            ]}
          />
          <Button size="sm" variant="ghost" onClick={pickVerifyFolder} title={t.reverifyHint}>
            <ShieldEllipsis size={14} aria-hidden="true" /> {t.reverify}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setDrawer("history")}>
            <ScrollText size={14} aria-hidden="true" /> {t.history}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setDrawer("settings")}>
            <SettingsIcon size={14} aria-hidden="true" /> {t.settings}
          </Button>
        </header>

        <div className="grid min-h-0 flex-1 grid-cols-[minmax(400px,440px)_1fr]">
          <aside aria-label="Setup" className="flex min-h-0 flex-col border-r border-line bg-surface">
            <div className="min-h-0 flex-1 overflow-y-auto">
              <SourceStep
                volumes={volumes}
                source={source}
                busy={busySources}
                loading={volumesLoading}
                onSelect={setSource}
                onPickFolder={pickSourceFolder}
                onEject={(m) => guard(() => api.ejectVolume(m).then(refreshVolumes), t.toastEjected)}
              />
              <RollStep
                settings={settings}
                project={project}
                camera={camera}
                roll={roll}
                pathPreview={pathPreview}
                onSettings={patchSettings}
                onProject={patchProject}
                onSwitchProject={switchProject}
                onCamera={changeCamera}
                onRoll={setRoll}
              />
              <DestStep
                rows={destRows}
                tiers={activeTiers}
                checks={preflight?.dests ?? []}
                distinctDevices={preflight?.distinct_devices ?? new Set(destRows.filter((r) => activeTiers[r.path]).map((r) => r.volume?.device_id ?? r.path)).size}
                minCopies={settings.minCopies}
                onTier={setTier}
                onKind={setKind}
                onAddFolder={addDestFolder}
                onRemoveCustom={removeCustom}
              />
            </div>
            <StartBar preflight={preflight} loading={pfLoading} ready={ready} hint={hint} starting={starting} roll={roll} onStart={start} />
          </aside>

          <main className="min-h-0 overflow-y-auto px-5 py-4">
            <div className="mb-3 flex items-baseline gap-3">
              <h2 className="text-[16px] font-semibold">{t.jobsTitle}</h2>
              {jobs.length > 0 && (
                <span className="text-[12px] text-faint">
                  {t.activeCount(activeJobs)} · {t.finishedCount(jobs.length - activeJobs)}
                </span>
              )}
            </div>
            {verifies.length > 0 && (
              <div className="mb-3 space-y-2">
                {verifies.map((v) => (
                  <VerifyCard
                    key={v.id}
                    task={v}
                    onCancel={() => guard(() => api.cancelVerify(v.id))}
                    onDismiss={() =>
                      guard(async () => {
                        await api.dismissVerify(v.id);
                        setVerifies(await api.verifyTasks());
                      })
                    }
                    onReveal={() => guard(() => api.reveal(v.root))}
                  />
                ))}
              </div>
            )}
            {sortedJobs.length === 0 ? (
              <div className="flex h-[calc(100%-40px)] min-h-40 flex-col items-center justify-center rounded-[var(--radius-card)] border border-dashed border-line-strong text-center">
                <ShieldCheck size={28} className="mb-2 text-faint" aria-hidden="true" />
                <div className="text-[14px] font-semibold">{t.jobsEmpty}</div>
                <div className="mt-1 max-w-sm text-[12.5px] text-muted">{t.jobsEmptyHint}</div>
              </div>
            ) : (
              <div className="space-y-3">
                {sortedJobs.map((j) => (
                  <JobCard
                    key={j.id}
                    job={j}
                    onCancel={() => guard(() => api.cancelJob(j.id))}
                    onResume={() => guard(() => api.resumeJob(j.id), t.toastResumed(j.meta.roll_id))}
                    onDismiss={() => guard(() => api.dismissJob(j.id).then(() => api.jobs().then(setJobs)))}
                    onEject={() => guard(() => api.ejectSource(j.id).then(refreshVolumes), t.toastCardEjected(j.meta.roll_id))}
                    onReveal={(p) => guard(() => api.reveal(p))}
                    onOpen={(p) => guard(() => api.openPath(p))}
                    onExportPdf={(h) => guard(async () => api.openPath(await api.exportPdf(h)), t.toastPdf)}
                  />
                ))}
              </div>
            )}
          </main>
        </div>

        {drawer === "history" && (
          <HistoryDrawer
            entries={history}
            activeIds={activeIds}
            onClose={() => setDrawer(null)}
            onResume={(id) => guard(() => api.resumeJob(id).then(() => setDrawer(null)), t.resume)}
            onReveal={(p) => guard(() => api.reveal(p))}
            onOpen={(p) => guard(() => api.openPath(p))}
            onVerify={startVerify}
          />
        )}
        {drawer === "settings" && (
          <SettingsDrawer
            settings={settings}
            info={info}
            onChange={patchSettings}
            onPickLogo={pickLogo}
            onClose={() => setDrawer(null)}
            onOpenData={() => info && guard(() => api.openPath(info.data_dir))}
          />
        )}

        {askRepeat && preflight?.previously_offloaded && (
          <Modal
            tone="warn"
            title={t.repeatTitle}
            onClose={() => setAskRepeat(false)}
            actions={
              <>
                <Button onClick={() => setAskRepeat(false)}>{t.cancel}</Button>
                <Button variant="danger" onClick={() => doStart(true)}>
                  {t.repeatConfirm(roll)}
                </Button>
              </>
            }
          >
            {t.repeatBody(preflight.previously_offloaded.camera_id, preflight.previously_offloaded.roll_id)}
          </Modal>
        )}

        {askClose && (
          <Modal
            tone="bad"
            title={t.closeTitle}
            onClose={() => setAskClose(false)}
            actions={
              <>
                <Button onClick={() => setAskClose(false)}>{t.stay}</Button>
                <Button variant="danger" onClick={() => api.quitApp()}>
                  {t.quitAnyway}
                </Button>
              </>
            }
          >
            {t.closeBody(activeCount)}
          </Modal>
        )}

        <div aria-live="polite" role="status" className="pointer-events-none fixed bottom-4 left-1/2 z-50 -translate-x-1/2">
          {toast && (
            <div
              className={`pointer-events-auto max-w-[680px] rounded-[var(--radius-control)] border px-4 py-2.5 text-[13px] whitespace-pre-line shadow-2xl ${
                toast.tone === "good"
                  ? "border-good/50 bg-good-soft text-good-ink"
                  : toast.tone === "bad"
                    ? "border-bad/50 bg-bad-soft text-bad-ink"
                    : "border-line-strong bg-raised text-ink"
              }`}
            >
              {toast.text}
            </div>
          )}
        </div>
      </div>
    </I18nProvider>
  );
}
