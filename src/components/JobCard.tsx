import {
  CircleCheckBig,
  CircleX,
  Clock,
  Eject,
  ExternalLink,
  FileDown,
  FileText,
  FolderOpen,
  Loader,
  RotateCcw,
  ShieldAlert,
  ShieldCheck,
  Square,
  TriangleAlert,
  X,
} from "lucide-react";
import { useState } from "react";
import type { DestSnapshot, JobPhase, JobSnapshot } from "../types";
import { isActive } from "../types";
import { bytes, clock, duration, speed } from "../lib/format";
import { KIND_LABEL } from "../lib/settings";
import { useI18n } from "../i18n";
import { Button, DualProgress, IconButton, Pill } from "./ui";

function phaseTone(p: JobPhase): "good" | "warn" | "bad" | "info" | "cascade" {
  if (p === "completed") return "good";
  if (p === "completed_with_errors" || p === "cancelled" || p === "interrupted") return "warn";
  if (p === "failed") return "bad";
  if (p === "cascading") return "cascade";
  return "info";
}

function PhaseIcon({ p }: { p: JobPhase }) {
  if (isActive(p)) return <Loader size={11} className="animate-spin" aria-hidden="true" />;
  if (p === "completed") return <CircleCheckBig size={11} aria-hidden="true" />;
  if (p === "failed") return <CircleX size={11} aria-hidden="true" />;
  return <TriangleAlert size={11} aria-hidden="true" />;
}

function DestLine({ d, active }: { d: DestSnapshot; active: boolean }) {
  const { t } = useI18n();
  const need = Math.max(1, d.need_bytes);
  const total = Math.max(1, d.total_bytes);
  const writePct = d.complete ? 100 : ((d.prior_bytes + Math.min(d.bytes_written, need)) / total) * 100;
  const verifyPct = d.complete ? 100 : ((d.prior_bytes + Math.min(d.bytes_verified, need)) / total) * 100;
  const tone = d.phase === "failed" ? "bad" : d.complete ? "good" : d.tier === "cascade" ? "cascade" : "info";
  const running = active && (d.phase === "copying" || d.phase === "verifying");
  return (
    <div className="grid grid-cols-[84px_minmax(150px,1fr)_minmax(200px,2.2fr)_118px] items-center gap-4 py-2">
      <div>
        <Pill tone={d.tier === "primary" ? "good" : "cascade"}>{d.tier === "primary" ? t.tier1 : t.cascade}</Pill>
      </div>
      <div className="min-w-0">
        <div className="truncate font-semibold" title={d.roll_dir}>
          {d.volume_name}
        </div>
        <div className="text-[11.5px] text-faint">
          {KIND_LABEL[d.kind]} · <span className="font-mono">{d.device_id}</span>
        </div>
      </div>
      <div className="min-w-0">
        <DualProgress write={writePct} verify={verifyPct} tone={tone} live={running} label={`${d.volume_name}: ${t.destPhase[d.phase]}`} />
        <div className="mt-1 flex justify-between gap-3 text-[11.5px] text-muted">
          <span className="min-w-0 truncate">
            {d.error ? (
              <span className="text-bad-ink" title={d.error}>
                {d.error}
              </span>
            ) : running ? (
              <span className="font-mono">{d.current_file.split("/").pop()}</span>
            ) : (
              t.destPhase[d.phase]
            )}
          </span>
          <span className="num shrink-0">{t.filesOf(d.files_verified, d.total_files)}</span>
        </div>
      </div>
      <div className="text-right text-[12px]">
        {running ? (
          <div className="num leading-tight text-muted">
            <div>
              <span className="text-faint">{t.write}</span> <span className="text-ink">{speed(d.write_speed)}</span>
            </div>
            <div>
              <span className="text-faint">{t.verify}</span> <span className="text-ink">{speed(d.verify_speed)}</span>
            </div>
          </div>
        ) : d.complete ? (
          <span className="inline-flex items-center gap-1 font-semibold text-good-ink">
            <CircleCheckBig size={14} aria-hidden="true" /> {t.destPhase.done}
          </span>
        ) : d.phase === "failed" ? (
          <span className="inline-flex items-center gap-1 font-semibold text-bad-ink">
            <CircleX size={14} aria-hidden="true" /> {t.destPhase.failed}
          </span>
        ) : (
          <span className="inline-flex items-center gap-1 text-muted">
            <Clock size={13} aria-hidden="true" /> {t.destPhase[d.phase]}
          </span>
        )}
      </div>
    </div>
  );
}

export function JobCard(props: {
  job: JobSnapshot;
  onCancel: () => void;
  onResume: () => void;
  onDismiss: () => void;
  onEject: () => void;
  onReveal: (path: string) => void;
  onOpen: (path: string) => void;
  onExportPdf: (html: string) => void;
}) {
  const { t, lang } = useI18n();
  const j = props.job;
  const active = isActive(j.phase);
  const [showErrors, setShowErrors] = useState(false);
  const readingCard = ["scanning", "copying", "source_verify", "waiting_device", "queued"].includes(j.phase);
  const firstComplete = j.dests.find((d) => d.complete) ?? j.dests[0];
  const border = j.safe_to_eject && !j.card_ejected ? "border-good/60" : j.phase === "failed" ? "border-bad/50" : "border-line";

  return (
    <article aria-label={`${j.meta.camera_id} ${j.meta.roll_id}`} className={`overflow-hidden rounded-[var(--radius-card)] border bg-surface ${border}`}>
      <header className="flex items-center gap-4 px-4 pt-3.5 pb-3">
        <div className="min-w-0">
          <div className="flex items-baseline gap-2">
            <span className="num text-[22px] leading-none font-bold tracking-tight">{j.meta.roll_id}</span>
            <span className="num text-[13px] text-muted">{j.meta.camera_id}</span>
          </div>
          <div className="mt-1 truncate text-[12px] text-faint">
            {j.meta.project_name} · {j.meta.shoot_day} · {j.source_label}
            {j.card_vendor ? ` · ${j.card_vendor}` : ""}
          </div>
        </div>
        <Pill tone={phaseTone(j.phase)}>
          <PhaseIcon p={j.phase} /> {t.phase[j.phase]}
        </Pill>
        <div className="min-w-0 flex-1 truncate text-[12.5px] text-muted" title={j.status_text}>
          {j.status_text}
        </div>
        <div className="num shrink-0 text-right text-[12px] text-muted">
          <div>
            {bytes(j.total_bytes)} · {j.total_files} {t.files.toLowerCase()}
          </div>
          {active && j.eta_seconds != null && <div className="text-faint">{t.left(duration(j.eta_seconds, lang))}</div>}
        </div>
        <div className="num w-14 shrink-0 text-right text-[20px] font-bold">{Math.floor(j.percent)}%</div>
      </header>

      <div aria-live="polite">
        {j.safe_to_eject ? (
          <div className="flex items-center gap-3 border-y border-good/40 bg-good-soft px-4 py-2.5">
            <ShieldCheck size={22} className="shrink-0 text-good-ink" aria-hidden="true" />
            <div className="min-w-0 flex-1">
              <div className="text-[14px] font-bold tracking-wide text-good-ink">{j.card_ejected ? t.ejected : t.safeToEject}</div>
              <div className="text-[12px] text-good-ink/85">
                {t.safeDetail(j.verified_copies, clock(j.safe_to_eject_at))}
                {active ? t.safeStillRunning : ""}
              </div>
            </div>
            {!j.card_ejected && !readingCard && (
              <Button variant="good" size="sm" onClick={props.onEject}>
                <Eject size={14} aria-hidden="true" /> {t.ejectCard}
              </Button>
            )}
          </div>
        ) : (
          !active &&
          j.phase !== "cancelled" && (
            <div className="flex items-center gap-3 border-y border-warn/40 bg-warn-soft px-4 py-2.5 text-[12.5px] font-medium text-warn-ink">
              <ShieldAlert size={18} className="shrink-0" aria-hidden="true" />
              {t.notSafe(j.verified_copies, j.min_copies)}
            </div>
          )
        )}
      </div>

      <div className="px-4 pt-1 pb-1">
        <div className="flex items-center gap-4 border-b border-line py-2 text-[12px]">
          <span className="eyebrow w-[84px]">{t.source}</span>
          <span className="min-w-0 flex-1 truncate text-ink">{j.source}</span>
          {readingCard && active && <span className="num text-muted">{t.read(speed(j.source_speed))}</span>}
        </div>
        <div className="divide-y divide-line">
          {j.dests.map((d) => (
            <DestLine key={d.index} d={d} active={active} />
          ))}
        </div>
      </div>

      <footer className="flex flex-wrap items-center gap-1.5 border-t border-line bg-sunken/60 px-3 py-2">
        {j.errors.length > 0 && (
          <button
            type="button"
            aria-expanded={showErrors}
            onClick={() => setShowErrors((v) => !v)}
            className="flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-1 text-[12px] font-semibold text-bad-ink hover:bg-raised"
          >
            <CircleX size={13} aria-hidden="true" /> {t.errorsCount(j.errors.length)} <span aria-hidden="true">{showErrors ? "▾" : "▸"}</span>
          </button>
        )}
        <span className="flex-1" />
        {firstComplete && (
          <Button size="sm" variant="ghost" onClick={() => props.onReveal(firstComplete.roll_dir)}>
            <FolderOpen size={14} aria-hidden="true" /> {t.openFolder}
          </Button>
        )}
        {j.report_path && (
          <Button size="sm" variant="ghost" onClick={() => props.onOpen(j.report_path as string)}>
            <FileText size={14} aria-hidden="true" /> {t.openReport}
          </Button>
        )}
        {j.report_pdf_path ? (
          <Button size="sm" variant="ghost" onClick={() => props.onOpen(j.report_pdf_path as string)}>
            <ExternalLink size={14} aria-hidden="true" /> {t.openPdf}
          </Button>
        ) : (
          j.report_path && (
            <Button size="sm" variant="ghost" onClick={() => props.onExportPdf(j.report_path as string)}>
              <FileDown size={14} aria-hidden="true" /> {t.exportPdf}
            </Button>
          )
        )}
        {active ? (
          <Button size="sm" variant="danger" onClick={props.onCancel}>
            <Square size={11} className="fill-current" aria-hidden="true" /> {t.stop}
          </Button>
        ) : (
          <>
            {j.resumable && (
              <Button size="sm" variant="primary" onClick={props.onResume}>
                <RotateCcw size={14} aria-hidden="true" /> {t.resume}
              </Button>
            )}
            <IconButton label={t.dismiss} onClick={props.onDismiss}>
              <X size={15} aria-hidden="true" />
            </IconButton>
          </>
        )}
      </footer>
      {showErrors && j.errors.length > 0 && (
        <ul className="selectable max-h-44 space-y-1 overflow-y-auto border-t border-line px-4 py-2 font-mono text-[11.5px] text-bad-ink">
          {j.errors.map((e) => (
            <li key={e}>{e}</li>
          ))}
        </ul>
      )}
    </article>
  );
}
