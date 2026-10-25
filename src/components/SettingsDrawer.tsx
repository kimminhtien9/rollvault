import { CircleCheckBig, CircleX, ImagePlus, Trash } from "lucide-react";
import type { ReactNode } from "react";
import type { AppInfo } from "../types";
import type { Settings } from "../lib/settings";
import { DEFAULT_REPORT_TEMPLATE, DEFAULT_ROLL_TEMPLATE } from "../lib/settings";
import { useI18n } from "../i18n";
import { Button, Drawer, Field, IconButton, Segmented, Switch } from "./ui";

function Tool(props: { name: string; path: string | null; missing: string }) {
  return (
    <div className="flex items-start gap-2 py-1">
      {props.path ? (
        <CircleCheckBig size={14} className="mt-0.5 text-good-ink" aria-label="OK" />
      ) : (
        <CircleX size={14} className="mt-0.5 text-bad-ink" aria-label="Missing" />
      )}
      <div className="min-w-0">
        <div className="font-medium">{props.name}</div>
        <div className="selectable truncate font-mono text-[11.5px] text-faint">{props.path ?? props.missing}</div>
      </div>
    </div>
  );
}

function Section(props: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-3">
      <h4 className="eyebrow">{props.title}</h4>
      {props.children}
    </section>
  );
}

export function SettingsDrawer(props: {
  settings: Settings;
  info: AppInfo | null;
  onChange: (patch: Partial<Settings>) => void;
  onPickLogo: () => void;
  onClose: () => void;
  onOpenData: () => void;
}) {
  const { t } = useI18n();
  const s = props.settings;
  return (
    <Drawer title={t.settings} onClose={props.onClose} closeLabel={t.close}>
      <div className="space-y-7">
        <Section title={t.language}>
          <Segmented
            ariaLabel={t.language}
            value={s.language}
            onChange={(v) => props.onChange({ language: v })}
            options={[
              { value: "en", label: "English" },
              { value: "vi", label: "Tiếng Việt" },
            ]}
          />
        </Section>

        <Section title={t.settingsReport}>
          <Field label={t.company} htmlFor="rv-company">
            <input id="rv-company" className="field" value={s.company} placeholder={t.companyPlaceholder} onChange={(e) => props.onChange({ company: e.target.value })} />
          </Field>
          <div>
            <span className="mb-1 block text-[12px] font-medium text-muted">{t.logo}</span>
            <div className="flex items-center gap-3">
              <div className="flex h-14 w-32 items-center justify-center overflow-hidden rounded-[var(--radius-control)] border border-line-strong bg-white">
                {s.logoDataUri ? (
                  <img src={s.logoDataUri} alt={t.logo} className="max-h-12 max-w-28 object-contain" />
                ) : (
                  <span className="text-[11px] text-[#6b7280]">{t.noLogo}</span>
                )}
              </div>
              <Button size="sm" onClick={props.onPickLogo}>
                <ImagePlus size={14} aria-hidden="true" /> {t.chooseImage}
              </Button>
              {s.logoDataUri && (
                <IconButton label={t.removeLogo} tone="danger" onClick={() => props.onChange({ logoDataUri: null })}>
                  <Trash size={14} aria-hidden="true" />
                </IconButton>
              )}
            </div>
          </div>
          <Field label={t.location} htmlFor="rv-location">
            <input id="rv-location" className="field" value={s.location} onChange={(e) => props.onChange({ location: e.target.value })} />
          </Field>
        </Section>

        <Section title={t.folders}>
          <p className="text-[12px] text-faint">{t.foldersHint}</p>
          <Field label={t.rollFolder} htmlFor="rv-rolltpl">
            <input id="rv-rolltpl" className="field num" value={s.rollTemplate} onChange={(e) => props.onChange({ rollTemplate: e.target.value })} />
          </Field>
          <Field label={t.reportFolder} htmlFor="rv-reptpl">
            <input id="rv-reptpl" className="field num" value={s.reportTemplate} onChange={(e) => props.onChange({ reportTemplate: e.target.value })} />
          </Field>
          <Button size="sm" variant="ghost" onClick={() => props.onChange({ rollTemplate: DEFAULT_ROLL_TEMPLATE, reportTemplate: DEFAULT_REPORT_TEMPLATE })}>
            {t.resetDefaults}
          </Button>
        </Section>

        <Section title={t.alerts}>
          <Switch checked={s.sounds} onChange={(v) => props.onChange({ sounds: v })} label={t.sounds} hint={t.soundsHint} />
          <Switch checked={s.notifications} onChange={(v) => props.onChange({ notifications: v })} label={t.notifications} />
        </Section>

        <Section title={t.tools}>
          <div>
            <Tool name={t.ffprobe} path={props.info?.ffprobe ?? null} missing={t.ffMissing} />
            <Tool name={t.ffmpeg} path={props.info?.ffmpeg ?? null} missing={t.ffMissing} />
            <Tool name={t.chrome} path={props.info?.chrome ?? null} missing={t.chromeMissing} />
          </div>
        </Section>

        <Section title={t.appData}>
          <div className="text-[12px] text-muted">{t.versionLine(props.info?.version ?? "", props.info?.hostname ?? "")}</div>
          <div className="selectable truncate font-mono text-[11.5px] text-faint">{props.info?.data_dir}</div>
          <Button size="sm" variant="ghost" onClick={props.onOpenData}>
            {t.openData}
          </Button>
        </Section>
      </div>
    </Drawer>
  );
}
