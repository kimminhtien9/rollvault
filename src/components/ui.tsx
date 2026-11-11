import type { ReactNode } from "react";
import { X } from "lucide-react";

type Variant = "primary" | "secondary" | "ghost" | "danger" | "good";

export function Button(props: {
  children: ReactNode;
  onClick?: () => void;
  variant?: Variant;
  size?: "sm" | "md" | "lg";
  disabled?: boolean;
  title?: string;
  ariaLabel?: string;
  className?: string;
}) {
  const { variant = "secondary", size = "md" } = props;
  const sizes = { sm: "h-7 px-2.5 text-[12px] gap-1.5", md: "h-8 px-3 text-[13px] gap-1.5", lg: "h-11 px-5 text-[14px] gap-2" };
  const variants: Record<Variant, string> = {
    primary: "bg-primary text-white hover:bg-primary-hover",
    secondary: "bg-raised text-ink border border-line-strong hover:border-line-hover",
    ghost: "text-muted hover:text-ink hover:bg-raised",
    danger: "bg-bad-soft text-bad-ink border border-bad/50 hover:border-bad",
    good: "bg-good text-on-good hover:brightness-110",
  };
  return (
    <button
      type="button"
      title={props.title}
      aria-label={props.ariaLabel}
      disabled={props.disabled}
      onClick={props.onClick}
      className={`inline-flex cursor-pointer items-center justify-center rounded-[var(--radius-control)] font-medium whitespace-nowrap transition-[background-color,border-color,color,filter] duration-150 select-none active:brightness-90 disabled:cursor-not-allowed disabled:opacity-40 ${sizes[size]} ${variants[variant]} ${props.className ?? ""}`}
    >
      {props.children}
    </button>
  );
}

export function IconButton(props: { label: string; onClick: () => void; children: ReactNode; tone?: "default" | "danger" }) {
  return (
    <button
      type="button"
      aria-label={props.label}
      title={props.label}
      onClick={props.onClick}
      className={`inline-flex h-7 w-7 cursor-pointer items-center justify-center rounded-md text-faint transition-colors duration-150 hover:bg-raised ${
        props.tone === "danger" ? "hover:text-bad-ink" : "hover:text-ink"
      }`}
    >
      {props.children}
    </button>
  );
}

type Tone = "neutral" | "good" | "warn" | "bad" | "info" | "cascade";

export function Pill(props: { children: ReactNode; tone?: Tone; title?: string }) {
  const tones: Record<Tone, string> = {
    neutral: "bg-raised text-muted border-line-strong",
    good: "bg-good-soft text-good-ink border-good/40",
    warn: "bg-warn-soft text-warn-ink border-warn/40",
    bad: "bg-bad-soft text-bad-ink border-bad/40",
    info: "bg-info-soft text-link border-link/30",
    cascade: "bg-cascade-soft text-cascade border-cascade/40",
  };
  return (
    <span
      title={props.title}
      className={`inline-flex h-5 items-center gap-1 rounded-full border px-2 text-[11px] font-semibold whitespace-nowrap ${tones[props.tone ?? "neutral"]}`}
    >
      {props.children}
    </span>
  );
}

export function Switch(props: { checked: boolean; onChange: (v: boolean) => void; label: ReactNode; hint?: string; disabled?: boolean }) {
  return (
    <label className={`flex cursor-pointer items-start gap-3 ${props.disabled ? "cursor-not-allowed opacity-45" : ""}`}>
      <button
        type="button"
        role="switch"
        aria-checked={props.checked}
        disabled={props.disabled}
        onClick={() => props.onChange(!props.checked)}
        className={`relative mt-0.5 h-5 w-9 shrink-0 cursor-pointer rounded-full transition-colors duration-150 ${props.checked ? "bg-primary" : "bg-track"}`}
      >
        <span
          className={`absolute top-0.5 h-4 w-4 rounded-full bg-white shadow transition-[left] duration-150 ${props.checked ? "left-[18px]" : "left-0.5"}`}
        />
      </button>
      <span className="leading-snug">
        <span className="text-ink">{props.label}</span>
        {props.hint && <span className="mt-0.5 block text-[12px] text-faint">{props.hint}</span>}
      </span>
    </label>
  );
}

export function Segmented<T extends string>(props: {
  value: T;
  options: { value: T; label: ReactNode; active?: string; title?: string }[];
  onChange: (v: T) => void;
  ariaLabel: string;
  size?: "sm" | "md";
}) {
  return (
    <div role="radiogroup" aria-label={props.ariaLabel} className="inline-flex rounded-[var(--radius-control)] border border-line-strong bg-sunken p-0.5">
      {props.options.map((o) => {
        const on = o.value === props.value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={on}
            title={o.title}
            onClick={() => props.onChange(o.value)}
            className={`cursor-pointer rounded-md px-2.5 font-semibold transition-colors duration-150 ${props.size === "sm" ? "h-6 text-[11.5px]" : "h-7 text-[12px]"} ${
              on ? (o.active ?? "bg-raised text-ink") : "text-faint hover:text-ink"
            }`}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/** Two-track bar: faint = written, solid = verified. */
export function DualProgress(props: { write: number; verify: number; tone: "info" | "good" | "bad" | "cascade"; live?: boolean; label: string }) {
  const clamp = (v: number) => Math.max(0, Math.min(100, v));
  const color = { info: "bg-[#3b82f6]", good: "bg-good", bad: "bg-bad", cascade: "bg-cascade" }[props.tone];
  return (
    <div
      role="progressbar"
      aria-label={props.label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(clamp(props.verify))}
      className="relative h-2 w-full overflow-hidden rounded-full bg-sunken ring-1 ring-line"
    >
      <div className={`absolute inset-y-0 left-0 ${color} opacity-35 transition-[width] duration-300`} style={{ width: `${clamp(props.write)}%` }} />
      <div className={`absolute inset-y-0 left-0 ${color} transition-[width] duration-300 ${props.live ? "stripes" : ""}`} style={{ width: `${clamp(props.verify)}%` }} />
    </div>
  );
}

export function Field(props: { label: string; hint?: string; children: ReactNode; className?: string; htmlFor?: string }) {
  return (
    <div className={props.className}>
      <label htmlFor={props.htmlFor} className="mb-1 block text-[12px] font-medium text-muted" title={props.hint}>
        {props.label}
      </label>
      {props.children}
    </div>
  );
}

export function Step(props: { n: number; title: string; icon: ReactNode; actions?: ReactNode; children: ReactNode; done?: boolean }) {
  return (
    <section aria-labelledby={`step-${props.n}`} className="border-b border-line px-4 py-4 last:border-b-0">
      <header className="mb-3 flex items-center gap-2.5">
        <span
          className={`flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-[12px] font-bold ${
            props.done ? "bg-good text-on-good" : "bg-raised text-muted ring-1 ring-line-strong"
          }`}
          aria-hidden="true"
        >
          {props.n}
        </span>
        <h2 id={`step-${props.n}`} className="flex items-center gap-2 text-[14px] font-semibold text-ink">
          {props.icon}
          {props.title}
        </h2>
        <div className="ml-auto flex items-center gap-1">{props.actions}</div>
      </header>
      {props.children}
    </section>
  );
}

export function Modal(props: { title: string; children: ReactNode; onClose: () => void; actions?: ReactNode; tone?: "warn" | "bad" }) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/65 p-6" onClick={props.onClose}>
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="rv-modal-title"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.key === "Escape" && props.onClose()}
        className={`w-full max-w-lg rounded-[var(--radius-card)] border bg-surface p-5 shadow-2xl ${
          props.tone === "bad" ? "border-bad/50" : props.tone === "warn" ? "border-warn/50" : "border-line-strong"
        }`}
      >
        <h3 id="rv-modal-title" className="mb-2 text-[15px] font-semibold">
          {props.title}
        </h3>
        <div className="text-[13px] leading-relaxed text-muted">{props.children}</div>
        {props.actions && <div className="mt-5 flex justify-end gap-2">{props.actions}</div>}
      </div>
    </div>
  );
}

export function Drawer(props: { title: string; children: ReactNode; onClose: () => void; width?: number; closeLabel: string }) {
  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/50" onClick={props.onClose} onKeyDown={(e) => e.key === "Escape" && props.onClose()}>
      <aside
        role="dialog"
        aria-modal="true"
        aria-label={props.title}
        onClick={(e) => e.stopPropagation()}
        style={{ width: props.width ?? 520 }}
        className="flex h-full max-w-full flex-col border-l border-line-strong bg-surface shadow-2xl"
      >
        <header className="flex h-[52px] items-center justify-between border-b border-line px-4">
          <h3 className="text-[15px] font-semibold">{props.title}</h3>
          <IconButton label={props.closeLabel} onClick={props.onClose}>
            <X size={16} aria-hidden="true" />
          </IconButton>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto p-4">{props.children}</div>
      </aside>
    </div>
  );
}

export function UsageBar(props: { free: number; total: number }) {
  const used = props.total > 0 ? 100 - (props.free / props.total) * 100 : 0;
  const tone = used > 92 ? "bg-bad" : used > 80 ? "bg-warn" : "bg-meter";
  return (
    <div className="h-1 w-full overflow-hidden rounded-full bg-sunken" aria-hidden="true">
      <div className={`h-full ${tone}`} style={{ width: `${Math.max(2, used)}%` }} />
    </div>
  );
}
