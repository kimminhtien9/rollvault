import { useState } from "react";
import { FileText, FolderOpen, RotateCcw, ShieldCheck, Trash } from "lucide-react";
import type { HistoryEntry } from "../types";
import { isActive } from "../types";
import { bytes, dateTime } from "../lib/format";
import { useI18n } from "../i18n";
import { Button, Drawer, IconButton, Modal, Pill } from "./ui";

export function HistoryDrawer(props: {
  entries: HistoryEntry[];
  activeIds: Set<string>;
  onClose: () => void;
  onResume: (id: string) => void;
  onReveal: (path: string) => void;
  onOpen: (path: string) => void;
  onVerify: (rollDir: string) => void;
  onDelete: (ids: string[]) => void;
}) {
  const { t } = useI18n();
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [confirm, setConfirm] = useState<string[] | null>(null);
  const sorted = [...props.entries].sort((a, b) => b.created_at.localeCompare(a.created_at));
  // Running jobs cannot be deleted.
  const deletable = sorted.filter((h) => !props.activeIds.has(h.job_id) && !isActive(h.status)).map((h) => h.job_id);
  const chosen = deletable.filter((id) => picked.has(id));
  const toggle = (id: string) =>
    setPicked((p) => {
      const n = new Set(p);
      if (!n.delete(id)) n.add(id);
      return n;
    });
  return (
    <Drawer title={t.historyTitle(sorted.length)} onClose={props.onClose} width={660} closeLabel={t.close}>
      {sorted.length === 0 ? (
        <p className="text-muted">{t.historyEmpty}</p>
      ) : (
        <p className="mb-3 text-[12px] text-faint">{t.historyHint}</p>
      )}
      {deletable.length > 0 && (
        <div className="mb-2 flex h-7 items-center gap-2">
          <label className="flex cursor-pointer items-center gap-2 text-[12px] text-muted">
            <input
              type="checkbox"
              className="accent-primary"
              checked={chosen.length === deletable.length}
              onChange={(e) => setPicked(new Set(e.target.checked ? deletable : []))}
            />
            {t.historySelectAll}
          </label>
          <span className="flex-1" />
          {chosen.length > 0 && (
            <Button size="sm" variant="danger" onClick={() => setConfirm(chosen)}>
              <Trash size={13} aria-hidden="true" /> {t.historyDeleteSelected(chosen.length)}
            </Button>
          )}
        </div>
      )}
      <ul className="space-y-2">
        {sorted.map((h) => {
          const running = isActive(h.status);
          const status = running ? t.historyStatus.running : t.historyStatus[h.status as keyof typeof t.historyStatus] ?? h.status;
          const tone = running ? "info" : h.status === "completed" ? "good" : h.status === "failed" ? "bad" : "warn";
          const resumable = !props.activeIds.has(h.job_id) && h.status !== "completed" && h.dests.some((d) => !d.complete);
          const done = h.dests.filter((d) => d.complete);
          return (
            <li key={h.job_id} className="rounded-[var(--radius-control)] border border-line bg-raised px-3 py-2.5">
              <div className="flex items-center gap-2">
                {deletable.includes(h.job_id) && (
                  <input
                    type="checkbox"
                    className="accent-primary"
                    aria-label={t.historySelect(h.meta.roll_id)}
                    checked={picked.has(h.job_id)}
                    onChange={() => toggle(h.job_id)}
                  />
                )}
                <span className="num text-[15px] font-bold">{h.meta.roll_id}</span>
                <span className="num text-muted">{h.meta.camera_id}</span>
                <Pill tone={tone}>{status}</Pill>
                {h.safe_to_eject && <Pill tone="good">{t.wasSafe}</Pill>}
                <span className="flex-1" />
                <span className="num text-[12px] text-faint">{dateTime(h.created_at)}</span>
              </div>
              <div className="mt-1 text-[12px] text-muted">
                {h.meta.project_name} · {h.meta.shoot_day} · {h.source_label} · <span className="num">{h.file_count}</span> · <span className="num">{bytes(h.total_bytes)}</span>
              </div>
              <div className="mt-1.5 flex flex-wrap gap-1">
                {h.dests.map((d) =>
                  d.complete ? (
                    <button
                      type="button"
                      key={d.roll_dir}
                      title={t.reverifyCopy(d.volume_name, d.roll_dir)}
                      aria-label={t.reverifyCopy(d.volume_name, d.roll_dir)}
                      onClick={() => props.onVerify(d.roll_dir)}
                      className="inline-flex h-6 cursor-pointer items-center gap-1 rounded-full border border-good/40 bg-good-soft px-2 text-[11.5px] font-semibold text-good-ink transition-colors hover:border-good"
                    >
                      <ShieldCheck size={12} aria-hidden="true" /> {d.tier === "primary" ? "T1" : "C"} · {d.volume_name}
                    </button>
                  ) : (
                    <Pill key={d.roll_dir} tone="bad" title={d.roll_dir}>
                      {d.tier === "primary" ? "T1" : "C"} · {d.volume_name}
                    </Pill>
                  ),
                )}
              </div>
              {h.errors.length > 0 && (
                <div className="mt-1 truncate text-[12px] text-bad-ink" title={h.errors.join("\n")}>
                  {h.errors[0]}
                </div>
              )}
              <div className="mt-2 flex gap-1.5">
                {done[0] && (
                  <Button size="sm" variant="ghost" onClick={() => props.onReveal(done[0].roll_dir)}>
                    <FolderOpen size={13} aria-hidden="true" /> {t.openFolder}
                  </Button>
                )}
                {h.report_path && (
                  <Button size="sm" variant="ghost" onClick={() => props.onOpen(h.report_path as string)}>
                    <FileText size={13} aria-hidden="true" /> {t.openReport}
                  </Button>
                )}
                {resumable && (
                  <Button size="sm" variant="primary" onClick={() => props.onResume(h.job_id)}>
                    <RotateCcw size={13} aria-hidden="true" /> {t.resume}
                  </Button>
                )}
                <span className="flex-1" />
                {deletable.includes(h.job_id) && (
                  <IconButton label={t.historyDelete} tone="danger" onClick={() => setConfirm([h.job_id])}>
                    <Trash size={14} aria-hidden="true" />
                  </IconButton>
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {confirm && (
        <Modal
          tone="bad"
          title={t.historyDeleteTitle(confirm.length)}
          onClose={() => setConfirm(null)}
          actions={
            <>
              <Button onClick={() => setConfirm(null)}>{t.cancel}</Button>
              <Button
                variant="danger"
                onClick={() => {
                  props.onDelete(confirm);
                  setPicked(new Set());
                  setConfirm(null);
                }}
              >
                {t.historyDeleteConfirm}
              </Button>
            </>
          }
        >
          {t.historyDeleteBody}
        </Modal>
      )}
    </Drawer>
  );
}
