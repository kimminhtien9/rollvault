import {
  CircleCheckBig,
  CircleX,
  Clapperboard,
  Eject,
  FolderOpen,
  FolderPlus,
  HardDrive,
  Loader,
  MemoryStick,
  Minus,
  Network,
  Play,
  Plus,
  Server,
  SlidersHorizontal,
  Trash,
  TriangleAlert,
  X,
} from "lucide-react";
import { useState } from "react";
import type { Algo, DestCheck, Kind, Preflight, Tier, VolumeInfo } from "../types";
import type { Project, Settings } from "../lib/settings";
import { ALGOS, KIND_LABEL, newProject } from "../lib/settings";
import { bytes, inputToYmd, ymdToInput } from "../lib/format";
import { useI18n } from "../i18n";
import { Button, Field, IconButton, Pill, Segmented, Step, Switch, UsageBar } from "./ui";

// ---------------------------------------------------------------------------
// Step 1: source
// ---------------------------------------------------------------------------

export function SourceStep(props: {
  volumes: VolumeInfo[];
  source: string;
  busy: Set<string>;
  loading: boolean;
  onSelect: (path: string) => void;
  onPickFolder: () => void;
  onEject: (mount: string) => void;
}) {
  const { t } = useI18n();
  const candidates = props.volumes.filter((v) => !v.internal && !v.network);
  const custom = props.source && !candidates.some((v) => v.mount_point === props.source) ? props.source : null;
  return (
    <Step
      n={1}
      done={!!props.source}
      title={t.stepSource}
      icon={<MemoryStick size={15} className="text-link" aria-hidden="true" />}
      actions={
        <Button size="sm" variant="ghost" onClick={props.onPickFolder} title={t.pickFolderSource}>
          <FolderOpen size={14} aria-hidden="true" /> {t.pickFolder}
        </Button>
      }
    >
      <div role="radiogroup" aria-label={t.stepSource} className="space-y-1.5">
        {props.loading && candidates.length === 0 && (
          <div className="flex items-center gap-2 px-1 py-2 text-muted">
            <Loader size={14} className="animate-spin" aria-hidden="true" /> {t.scanningDrives}
          </div>
        )}
        {!props.loading && candidates.length === 0 && !custom && (
          <div className="rounded-[var(--radius-control)] border border-dashed border-line-strong px-3 py-4 text-center">
            <div className="text-ink">{t.noCards}</div>
            <div className="mt-0.5 text-[12px] text-faint">{t.noCardsHint}</div>
          </div>
        )}
        {candidates.map((v) => {
          const selected = props.source === v.mount_point;
          const busy = props.busy.has(v.mount_point);
          return (
            <div
              key={v.mount_point}
              role="radio"
              aria-checked={selected}
              aria-disabled={v.unresponsive}
              tabIndex={0}
              onClick={() => !v.unresponsive && props.onSelect(v.mount_point)}
              onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && !v.unresponsive && props.onSelect(v.mount_point)}
              className={`group flex cursor-pointer items-center gap-3 rounded-[var(--radius-control)] border px-3 py-2 transition-colors duration-150 ${
                selected
                  ? "border-link bg-info-soft"
                  : v.unresponsive
                    ? "cursor-not-allowed border-warn/40 bg-warn-soft/50"
                    : "border-line bg-raised hover:border-line-strong"
              }`}
            >
              <span
                aria-hidden="true"
                className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-full border-2 ${selected ? "border-link" : "border-line-strong"}`}
              >
                {selected && <span className="h-2 w-2 rounded-full bg-link" />}
              </span>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="truncate font-semibold">{v.name}</span>
                  {v.is_camera_card && <Pill tone="info">{t.card(v.card_vendor ?? "")}</Pill>}
                  {busy && (
                    <Pill tone="warn">
                      <span className="pulse h-1.5 w-1.5 rounded-full bg-warn" aria-hidden="true" /> {t.reading}
                    </Pill>
                  )}
                </div>
                {v.unresponsive ? (
                  <div className="mt-0.5 flex items-start gap-1.5 text-[12px] text-warn-ink">
                    <TriangleAlert size={12} className="mt-0.5 shrink-0" aria-hidden="true" />
                    {t.notResponding}. {t.notRespondingHint}
                  </div>
                ) : (
                  <div className="mt-0.5 flex items-center gap-2 text-[12px] whitespace-nowrap text-faint">
                    <span className="num shrink-0">{t.used(bytes(v.total_bytes - v.free_bytes))}</span>
                    <span aria-hidden="true">·</span>
                    <span className="uppercase">{v.fs_type}</span>
                    <span aria-hidden="true">·</span>
                    <span className="truncate font-mono">{v.mount_point}</span>
                  </div>
                )}
              </div>
              {v.ejectable && !busy && (
                <span onClick={(e) => e.stopPropagation()} className="opacity-60 group-hover:opacity-100">
                  <IconButton label={t.ejectNamed(v.name)} onClick={() => props.onEject(v.mount_point)}>
                    <Eject size={14} aria-hidden="true" />
                  </IconButton>
                </span>
              )}
            </div>
          );
        })}
        {custom && (
          <div className="flex items-center gap-3 rounded-[var(--radius-control)] border border-link bg-info-soft px-3 py-2">
            <FolderOpen size={15} className="text-link" aria-hidden="true" />
            <div className="min-w-0">
              <div className="font-semibold">{t.folder}</div>
              <div className="selectable truncate font-mono text-[12px] text-muted">{custom}</div>
            </div>
          </div>
        )}
      </div>
    </Step>
  );
}

// ---------------------------------------------------------------------------
// Step 2: project & roll
// ---------------------------------------------------------------------------

function bumpDay(day: string, delta: number): string {
  const m = day.match(/^(.*?)(\d+)$/);
  if (!m) return day;
  const n = Math.max(1, parseInt(m[2], 10) + delta);
  return `${m[1]}${String(n).padStart(m[2].length, "0")}`;
}

export function RollStep(props: {
  settings: Settings;
  project: Project;
  camera: string;
  roll: string;
  pathPreview: string;
  onSettings: (patch: Partial<Settings>) => void;
  onProject: (patch: Partial<Project>) => void;
  onSwitchProject: (id: string) => void;
  onCamera: (camera: string) => void;
  onRoll: (roll: string) => void;
}) {
  const { t } = useI18n();
  const { settings: s, project: p } = props;
  const [showOptions, setShowOptions] = useState(false);
  const cameras = Object.keys(p.cameras).sort();
  const algo = ALGOS.find((a) => a.id === s.algo);

  const addProject = () => {
    const np = newProject(t.newProjectName);
    props.onSettings({ projects: [...s.projects, np], activeProjectId: np.id });
  };
  const removeProject = () => {
    if (s.projects.length <= 1 || !window.confirm(t.confirmDeleteProject(p.projectName))) return;
    const rest = s.projects.filter((x) => x.id !== p.id);
    props.onSettings({ projects: rest, activeProjectId: rest[0].id });
  };

  return (
    <Step n={2} done={!!p.projectName && !!props.camera && !!props.roll} title={t.stepRoll} icon={<Clapperboard size={15} className="text-cascade" aria-hidden="true" />}>
      <div className="space-y-3">
        <div className="flex items-end gap-1">
          <Field label={t.project} className="min-w-0 flex-1" htmlFor="rv-project">
            <select id="rv-project" className="field" value={p.id} onChange={(e) => props.onSwitchProject(e.target.value)}>
              {s.projects.map((x) => (
                <option key={x.id} value={x.id}>
                  {x.projectName || "—"}
                </option>
              ))}
            </select>
          </Field>
          <IconButton label={t.newProject} onClick={addProject}>
            <Plus size={15} aria-hidden="true" />
          </IconButton>
          {s.projects.length > 1 && (
            <IconButton label={t.deleteProject} onClick={removeProject} tone="danger">
              <Trash size={14} aria-hidden="true" />
            </IconButton>
          )}
        </div>

        <div className="grid grid-cols-2 gap-x-2.5 gap-y-3">
          <Field label={t.projectName} className="col-span-2" htmlFor="rv-pname">
            <input id="rv-pname" className="field font-semibold" value={p.projectName} onChange={(e) => props.onProject({ projectName: e.target.value })} />
          </Field>
          <Field label={t.shootDate} htmlFor="rv-date">
            <input id="rv-date" type="date" className="field num" value={ymdToInput(p.shootDate)} onChange={(e) => props.onProject({ shootDate: inputToYmd(e.target.value) })} />
          </Field>
          <Field label={t.shootDay} htmlFor="rv-day">
            <div className="flex items-center gap-1">
              <input id="rv-day" className="field num" value={p.shootDay} onChange={(e) => props.onProject({ shootDay: e.target.value })} />
              <IconButton label={t.prevDay} onClick={() => props.onProject({ shootDay: bumpDay(p.shootDay, -1) })}>
                <Minus size={14} aria-hidden="true" />
              </IconButton>
              <IconButton label={t.nextDay} onClick={() => props.onProject({ shootDay: bumpDay(p.shootDay, 1) })}>
                <Plus size={14} aria-hidden="true" />
              </IconButton>
            </div>
          </Field>
          <Field label={t.category} htmlFor="rv-cat">
            <select id="rv-cat" className="field" value={p.mediaCategory} onChange={(e) => props.onProject({ mediaCategory: e.target.value })}>
              {["01_FOOTAGE", "02_AUDIO", "04_STILLS", "05_DRONE"].map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </Field>
          <Field label={t.dit} htmlFor="rv-dit">
            <input id="rv-dit" className="field" value={s.ditName} placeholder={t.ditPlaceholder} onChange={(e) => props.onSettings({ ditName: e.target.value })} />
          </Field>
          <Field label={t.camera} hint={t.cameraHint} htmlFor="rv-cam">
            <input id="rv-cam" className="field num" list="rv-cameras" value={props.camera} onChange={(e) => props.onCamera(e.target.value)} />
            <datalist id="rv-cameras">
              {cameras.map((c) => (
                <option key={c} value={c} />
              ))}
            </datalist>
          </Field>
          <Field label={t.roll} hint={t.rollHint} htmlFor="rv-roll">
            <input
              id="rv-roll"
              className="field num border-link/60 bg-info-soft text-[15px] font-bold text-link"
              value={props.roll}
              onChange={(e) => props.onRoll(e.target.value)}
            />
          </Field>
        </div>

        <div className="rounded-[var(--radius-control)] bg-sunken px-3 py-2 ring-1 ring-line">
          <div className="eyebrow !text-[10.5px]">{t.rollPath}</div>
          <div className="selectable num mt-0.5 text-[12px] break-all text-good-ink">{props.pathPreview || "—"}</div>
        </div>

        <button
          type="button"
          aria-expanded={showOptions}
          onClick={() => setShowOptions((v) => !v)}
          className="flex w-full cursor-pointer items-center gap-2 rounded-md py-1 text-[12px] font-semibold text-muted hover:text-ink"
        >
          <SlidersHorizontal size={14} aria-hidden="true" />
          {t.options}
          <span className="ml-auto font-normal text-faint">
            {algo?.label} · {s.minCopies}× · {s.generateReport ? (s.reportPdf ? "HTML+PDF" : "HTML") : "—"}
          </span>
          <span aria-hidden="true">{showOptions ? "▾" : "▸"}</span>
        </button>
        {showOptions && (
          <div className="space-y-3 rounded-[var(--radius-control)] border border-line p-3">
            <div className="grid grid-cols-2 gap-2.5">
              <Field label={t.checksum} hint={algo?.hint} htmlFor="rv-algo">
                <select id="rv-algo" className="field" value={s.algo} onChange={(e) => props.onSettings({ algo: e.target.value as Algo })}>
                  {ALGOS.map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.label}
                    </option>
                  ))}
                </select>
              </Field>
              <div>
                <span className="mb-1 block text-[12px] font-medium text-muted" title={t.minCopiesHint}>
                  {t.minCopies}
                </span>
                <Segmented
                  ariaLabel={t.minCopies}
                  value={String(s.minCopies)}
                  onChange={(v) => props.onSettings({ minCopies: Number(v) })}
                  options={[
                    { value: "1", label: "1", active: "bg-warn-soft text-warn-ink" },
                    { value: "2", label: "2", active: "bg-good-soft text-good-ink", title: t.recommended },
                    { value: "3", label: "3", active: "bg-good-soft text-good-ink" },
                  ]}
                />
              </div>
            </div>
            <Switch checked={s.sourceVerify} onChange={(v) => props.onSettings({ sourceVerify: v })} label={t.sourceVerify} hint={t.sourceVerifyHint} />
            <Switch checked={s.generateReport} onChange={(v) => props.onSettings({ generateReport: v })} label={t.report} />
            <Switch checked={s.reportPdf} disabled={!s.generateReport} onChange={(v) => props.onSettings({ reportPdf: v })} label={t.pdf} hint={t.pdfHint} />
          </div>
        )}
      </div>
    </Step>
  );
}

// ---------------------------------------------------------------------------
// Step 3: destinations
// ---------------------------------------------------------------------------

export interface DestRow {
  path: string;
  name: string;
  volume: VolumeInfo | null;
  kind: Kind;
  kindKey: string | null;
  custom: boolean;
}

type Choice = "off" | Tier;

export function DestStep(props: {
  rows: DestRow[];
  tiers: Record<string, Tier>;
  checks: DestCheck[];
  distinctDevices: number;
  minCopies: number;
  onTier: (path: string, tier: Choice) => void;
  onKind: (key: string, kind: Kind) => void;
  onAddFolder: () => void;
  onRemoveCustom: (path: string) => void;
}) {
  const { t } = useI18n();
  const selected = props.rows.filter((r) => props.tiers[r.path]);
  const enough = props.distinctDevices >= props.minCopies;
  const hasTier1 = selected.some((r) => props.tiers[r.path] === "primary");
  return (
    <Step
      n={3}
      done={hasTier1}
      title={t.stepDest}
      icon={<HardDrive size={15} className="text-good-ink" aria-hidden="true" />}
      actions={
        <Button size="sm" variant="ghost" onClick={props.onAddFolder} title={t.addFolderHint}>
          <FolderPlus size={14} aria-hidden="true" /> {t.addFolder}
        </Button>
      }
    >
      <div className="space-y-1.5">
        {props.rows.length === 0 && (
          <div className="rounded-[var(--radius-control)] border border-dashed border-line-strong px-3 py-4 text-center text-ink">{t.noDests}</div>
        )}
        {props.rows.map((r) => {
          const tier: Choice = (props.tiers[r.path] as Tier | undefined) ?? "off";
          const check = props.checks.find((c) => c.root === r.path);
          const v = r.volume;
          const Icon = r.kind === "network" ? Network : r.kind === "hdd" ? Server : HardDrive;
          return (
            <div
              key={r.path}
              className={`rounded-[var(--radius-control)] border px-3 py-2 transition-colors duration-150 ${
                tier === "primary" ? "border-good/50 bg-good-soft/50" : tier === "cascade" ? "border-cascade/50 bg-cascade-soft/60" : "border-line bg-raised"
              }`}
            >
              <div className="flex items-center gap-2">
                <Icon size={15} className="shrink-0 text-muted" aria-hidden="true" />
                <span className="min-w-0 truncate font-semibold">{r.name}</span>
                {v?.unresponsive && <Pill tone="warn">{t.notResponding}</Pill>}
                <span className="flex-1" />
                {r.kindKey ? (
                  <select
                    aria-label={`${r.name}: ${KIND_LABEL[r.kind]}`}
                    title={t.driveKindHint}
                    className="h-6 cursor-pointer rounded-md border border-line-strong bg-sunken px-1 text-[11.5px] font-semibold text-muted"
                    value={r.kind}
                    onChange={(e) => props.onKind(r.kindKey as string, e.target.value as Kind)}
                  >
                    {(["ssd", "hdd", "network"] as Kind[]).map((k) => (
                      <option key={k} value={k}>
                        {KIND_LABEL[k]}
                      </option>
                    ))}
                  </select>
                ) : (
                  <Pill>{KIND_LABEL[r.kind]}</Pill>
                )}
                {r.custom && (
                  <IconButton label={t.removeFolder} onClick={() => props.onRemoveCustom(r.path)}>
                    <X size={13} aria-hidden="true" />
                  </IconButton>
                )}
              </div>
              <div className="mt-1.5 flex items-center gap-3">
                <div className="min-w-0 flex-1">
                  {v && <UsageBar free={v.free_bytes} total={v.total_bytes} />}
                  <div className="mt-1 flex gap-2 text-[12px] text-faint">
                    {v ? (
                      <>
                        <span className="num">{t.free(bytes(v.free_bytes), bytes(v.total_bytes))}</span>
                        <span className="uppercase">{v.fs_type}</span>
                      </>
                    ) : (
                      <span>{t.driveMissing}</span>
                    )}
                    {check && check.needed_bytes > 0 && tier !== "off" && <span className="num">· {t.needs(bytes(check.needed_bytes))}</span>}
                  </div>
                  {r.custom && <div className="selectable truncate font-mono text-[11.5px] text-faint">{r.path}</div>}
                </div>
                <Segmented<Choice>
                  size="sm"
                  ariaLabel={r.name}
                  value={tier}
                  onChange={(c) => props.onTier(r.path, c)}
                  options={[
                    { value: "off", label: t.off },
                    { value: "primary", label: t.tier1, active: "bg-good text-on-good", title: t.tier1Hint },
                    { value: "cascade", label: t.cascade, active: "bg-cascade text-on-cascade", title: t.cascadeHint },
                  ]}
                />
              </div>
              {check && check.problems.length > 0 && tier !== "off" && (
                <ul className="mt-1.5 space-y-0.5">
                  {check.problems.map((p) => (
                    <li key={p} className="flex items-start gap-1.5 text-[12px] text-bad-ink">
                      <CircleX size={12} className="mt-0.5 shrink-0" aria-hidden="true" />
                      {p}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          );
        })}
        {selected.length > 0 && (
          <div
            className={`mt-2 flex items-start gap-2 rounded-[var(--radius-control)] px-3 py-2 text-[12px] ${
              enough ? "bg-good-soft text-good-ink" : "bg-warn-soft text-warn-ink"
            }`}
          >
            {enough ? <CircleCheckBig size={14} className="mt-px shrink-0" aria-hidden="true" /> : <TriangleAlert size={14} className="mt-px shrink-0" aria-hidden="true" />}
            {enough ? t.devicesEnough(props.distinctDevices, props.minCopies) : t.devicesShort(props.distinctDevices, props.minCopies)}
          </div>
        )}
      </div>
    </Step>
  );
}

// ---------------------------------------------------------------------------
// Start bar (sticky at the bottom of the setup column)
// ---------------------------------------------------------------------------

export function StartBar(props: {
  preflight: Preflight | null;
  loading: boolean;
  ready: boolean;
  hint: string;
  starting: boolean;
  roll: string;
  onStart: () => void;
}) {
  const { t } = useI18n();
  const pf = props.preflight;
  const errors = pf?.errors ?? [];
  const warnings = pf?.warnings ?? [];
  const canStart = props.ready && !!pf && errors.length === 0 && !props.loading && !props.starting;
  const confirm = !!pf?.needs_confirmation;
  return (
    <div className="border-t border-line bg-surface px-4 py-3">
      <div aria-live="polite" className="mb-2 min-h-[20px]">
        {props.loading ? (
          <span className="flex items-center gap-2 text-muted">
            <Loader size={14} className="animate-spin" aria-hidden="true" /> {t.checking}
          </span>
        ) : !props.ready || !pf ? (
          <span className="text-muted">{props.hint}</span>
        ) : (
          <div className="grid grid-cols-4 gap-2">
            {[
              [t.files, pf.file_count.toLocaleString()],
              [t.clips, pf.clip_count.toLocaleString()],
              [t.size, bytes(pf.total_bytes)],
              [t.drives, String(pf.distinct_devices)],
            ].map(([k, v]) => (
              <div key={k}>
                <div className="text-[11px] text-faint">{k}</div>
                <div className="num text-[14px] font-semibold">{v}</div>
              </div>
            ))}
          </div>
        )}
      </div>
      {(errors.length > 0 || warnings.length > 0) && !props.loading && (
        <ul className="mb-2 max-h-28 space-y-1 overflow-y-auto pr-1">
          {errors.map((e) => (
            <li key={e} className="flex items-start gap-1.5 text-[12px] text-bad-ink">
              <CircleX size={13} className="mt-px shrink-0" aria-hidden="true" /> {e}
            </li>
          ))}
          {warnings.map((w) => (
            <li key={w} className="flex items-start gap-1.5 text-[12px] text-warn-ink">
              <TriangleAlert size={13} className="mt-px shrink-0" aria-hidden="true" /> {w}
            </li>
          ))}
        </ul>
      )}
      <Button
        size="lg"
        variant={confirm ? "danger" : "primary"}
        disabled={!canStart}
        onClick={props.onStart}
        className="w-full"
        title={confirm ? t.startAgainHint : undefined}
      >
        {props.starting ? <Loader size={16} className="animate-spin" aria-hidden="true" /> : <Play size={16} className="fill-current" aria-hidden="true" />}
        {confirm ? t.startAgain(props.roll) : t.start(props.roll)}
      </Button>
    </div>
  );
}
