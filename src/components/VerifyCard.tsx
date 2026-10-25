import { CircleCheckBig, CircleX, Loader, ShieldCheck, Square, X } from "lucide-react";
import { useState } from "react";
import type { VerifySnapshot } from "../types";
import { basename, bytes } from "../lib/format";
import { useI18n } from "../i18n";
import { Button, DualProgress, IconButton, Pill } from "./ui";

export function VerifyCard(props: { task: VerifySnapshot; onCancel: () => void; onDismiss: () => void; onReveal: () => void }) {
  const { t } = useI18n();
  const v = props.task;
  const o = v.outcome;
  const [open, setOpen] = useState(false);
  const pct = v.bytes_total > 0 ? (v.bytes_done / v.bytes_total) * 100 : 0;
  const running = v.status === "running";
  const problems = o ? [...o.failed.map((f) => `${t.mismatch} · ${f}`), ...o.missing.map((f) => `${t.missing} · ${f}`)] : [];
  const border = v.status === "failed" ? "border-bad/50" : v.status === "passed" ? "border-good/50" : "border-line";
  return (
    <article className={`rounded-[var(--radius-card)] border bg-surface px-4 py-3 ${border}`}>
      <div className="flex items-center gap-3">
        <ShieldCheck size={16} className="text-link" aria-hidden="true" />
        <span className="font-semibold">{t.verifyTitle}</span>
        <span className="num min-w-0 flex-1 truncate text-[12px] text-muted" title={v.root}>
          {basename(v.root)} · {v.root}
        </span>
        {running && (
          <Pill tone="info">
            <Loader size={11} className="animate-spin" aria-hidden="true" /> {Math.floor(pct)}%
          </Pill>
        )}
        {v.status === "passed" && (
          <Pill tone="good">
            <CircleCheckBig size={11} aria-hidden="true" /> {t.verifyMatch(o?.verified ?? 0)}
          </Pill>
        )}
        {v.status === "failed" && (
          <Pill tone="bad">
            <CircleX size={11} aria-hidden="true" /> {v.error ? t.verifyError : t.verifyProblems(problems.length)}
          </Pill>
        )}
        {v.status === "cancelled" && <Pill tone="warn">{t.phase.cancelled}</Pill>}
        {running ? (
          <Button size="sm" variant="danger" onClick={props.onCancel}>
            <Square size={11} className="fill-current" aria-hidden="true" /> {t.stop}
          </Button>
        ) : (
          <>
            <Button size="sm" variant="ghost" onClick={props.onReveal}>
              {t.open}
            </Button>
            <IconButton label={t.dismiss} onClick={props.onDismiss}>
              <X size={15} aria-hidden="true" />
            </IconButton>
          </>
        )}
      </div>
      {running && (
        <div className="mt-2">
          <DualProgress write={pct} verify={pct} tone="info" live label={t.verifyTitle} />
          <div className="mt-1 flex justify-between text-[11.5px] text-muted">
            <span className="truncate font-mono">{v.current}</span>
            <span className="num">
              {bytes(v.bytes_done)} / {bytes(v.bytes_total)}
            </span>
          </div>
        </div>
      )}
      {v.error && <div className="mt-1 text-[12px] text-bad-ink">{v.error}</div>}
      {o && (problems.length > 0 || o.unrecorded.length > 0) && (
        <div className="mt-1.5">
          <button type="button" aria-expanded={open} className="cursor-pointer text-[12px] text-muted hover:text-ink" onClick={() => setOpen((x) => !x)}>
            {t.verifySummary(problems.length, o.unrecorded.length)} <span aria-hidden="true">{open ? "▾" : "▸"}</span>
          </button>
          {open && (
            <ul className="selectable mt-1 max-h-36 overflow-y-auto font-mono text-[11.5px]">
              {problems.map((p) => (
                <li key={p} className="text-bad-ink">
                  {p}
                </li>
              ))}
              {o.unrecorded.map((p) => (
                <li key={p} className="text-warn-ink">
                  {t.newFile} · {p}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </article>
  );
}
