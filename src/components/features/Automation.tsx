import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  CalendarClock,
  Check,
  ChevronDown,
  CircleAlert,
  Copy,
  Loader2,
  Play,
  RefreshCw,
  Save,
  ScrollText,
  Trash2,
  Zap,
} from "lucide-react";
import { Button, Card, CardContent } from "@/components/ui";
import { Badge, Checkbox, ConfirmDialog, EmptyState } from "@/components/shared";
import { useTranslation } from "@/hooks/useTranslation";
import { cn, formatBytes } from "@/lib/utils";
import {
  clearLogs,
  generateToken,
  getLogs,
  getStatus,
  onAutomationLog,
  runNow,
  setConfig,
} from "@/services/automation";
import type { Lang, TranslationKey } from "@/i18n/translations";
import type {
  AppError,
  AutomationAction,
  AutomationConfig,
  AutomationLogEntry,
  AutomationStatus,
  ScheduleConfig,
  ThresholdTrigger,
  TriggerSource,
  WebhookConfig,
} from "@/types";

/** Rows the log table keeps on screen. */
const MAX_LOG_ROWS = 100;

/** Weekday chips in display order: Monday first, Sunday last. */
const DAY_ORDER = [1, 2, 3, 4, 5, 6, 0];

const DAY_KEYS: Record<number, TranslationKey> = {
  0: "automation.day.sun",
  1: "automation.day.mon",
  2: "automation.day.tue",
  3: "automation.day.wed",
  4: "automation.day.thu",
  5: "automation.day.fri",
  6: "automation.day.sat",
};

const INTERVAL_HOURS = [1, 2, 3, 4, 6, 8, 12];

const SOURCE_KEYS: Record<TriggerSource, TranslationKey> = {
  schedule: "automation.source.schedule",
  disk: "automation.source.disk",
  memory: "automation.source.memory",
  webhook: "automation.source.webhook",
  manual: "automation.source.manual",
};

const SOURCE_TONES: Record<TriggerSource, "primary" | "warning" | "neutral"> = {
  schedule: "primary",
  disk: "warning",
  memory: "warning",
  webhook: "neutral",
  manual: "neutral",
};

const ACTION_KEYS: Record<AutomationAction, TranslationKey> = {
  "deep-clean": "automation.action.deep-clean",
  "memory-clean": "automation.action.memory-clean",
};

/** "Wed 14:00" style, absolute so a stale page still reads correctly. */
function formatWhen(ms: number, lang: Lang): string {
  return new Date(ms).toLocaleString(lang, {
    weekday: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatLogTime(at: number, lang: Lang): string {
  return new Date(at).toLocaleString(lang, {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

export function Automation() {
  const { t, lang } = useTranslation();

  const [status, setStatus] = useState<AutomationStatus | null>(null);
  const [draft, setDraft] = useState<AutomationConfig | null>(null);
  const [logs, setLogs] = useState<AutomationLogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedFlash, setSavedFlash] = useState(false);
  const [copied, setCopied] = useState(false);
  const [confirmClearOpen, setConfirmClearOpen] = useState(false);

  const savedTimer = useRef<number | null>(null);
  const copiedTimer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (savedTimer.current) window.clearTimeout(savedTimer.current);
      if (copiedTimer.current) window.clearTimeout(copiedTimer.current);
    },
    [],
  );

  const load = useCallback(async () => {
    setLoadError(false);
    try {
      const [next, entries] = await Promise.all([
        getStatus(),
        getLogs(MAX_LOG_ROWS),
      ]);
      setStatus(next);
      setDraft((current) => current ?? next.config);
      setLogs(entries);
    } catch (err) {
      console.error("[Automation] load failed:", err);
      setLoadError(true);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const syncStatus = useCallback(async () => {
    try {
      setStatus(await getStatus());
    } catch {
      // Keep the last known state; the next event will try again.
    }
  }, []);

  // A finished run shows up as an event, so the log fills in live while the
  // page is open — the same entry the backend just wrote to disk.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    onAutomationLog((entry) => {
      setLogs((current) => [entry, ...current].slice(0, MAX_LOG_ROWS));
      void syncStatus();
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [syncStatus]);

  const dirty = useMemo(
    () =>
      draft != null &&
      status != null &&
      JSON.stringify(draft) !== JSON.stringify(status.config),
    [draft, status],
  );

  const busy = status?.running ?? false;

  function patch(changes: Partial<AutomationConfig>) {
    setDraft((current) => (current ? { ...current, ...changes } : current));
  }

  function patchSchedule(changes: Partial<ScheduleConfig>) {
    setDraft((current) =>
      current
        ? { ...current, schedule: { ...current.schedule, ...changes } }
        : current,
    );
  }

  function patchDiskTrigger(changes: Partial<ThresholdTrigger>) {
    setDraft((current) =>
      current
        ? { ...current, diskTrigger: { ...current.diskTrigger, ...changes } }
        : current,
    );
  }

  function patchMemoryTrigger(changes: Partial<ThresholdTrigger>) {
    setDraft((current) =>
      current
        ? {
            ...current,
            memoryTrigger: { ...current.memoryTrigger, ...changes },
          }
        : current,
    );
  }

  function patchWebhook(changes: Partial<WebhookConfig>) {
    setDraft((current) =>
      current ? { ...current, webhook: { ...current.webhook, ...changes } } : current,
    );
  }

  function toggleDay(day: number) {
    setDraft((current) => {
      if (!current) return current;
      const days = current.schedule.days.includes(day)
        ? current.schedule.days.filter((value) => value !== day)
        : [...current.schedule.days, day].sort((a, b) => a - b);
      return { ...current, schedule: { ...current.schedule, days } };
    });
  }

  async function save() {
    if (!draft) return;
    // The backend refuses these too; checking here is only so the message is
    // in the user's language and lands next to the fields.
    if (draft.schedule.enabled && draft.schedule.days.length === 0) {
      setErrorMessage(t("automation.pickAtLeastOneDay"));
      return;
    }
    if (draft.webhook.enabled && draft.webhook.token.trim().length < 8) {
      setErrorMessage(t("automation.tokenTooShort"));
      return;
    }

    setSaving(true);
    setErrorMessage(null);
    try {
      const applied = await setConfig(draft);
      setStatus(applied);
      setDraft(applied.config);
      setSavedFlash(true);
      if (savedTimer.current) window.clearTimeout(savedTimer.current);
      savedTimer.current = window.setTimeout(() => setSavedFlash(false), 1800);
    } catch (err) {
      setErrorMessage(
        t("automation.saveFailed", { message: (err as AppError).message }),
      );
    } finally {
      setSaving(false);
    }
  }

  async function startRun(action: AutomationAction) {
    setErrorMessage(null);
    try {
      await runNow(action);
    } catch (err) {
      setErrorMessage((err as AppError).message);
    }
    void syncStatus();
  }

  async function copyToken() {
    if (!draft) return;
    try {
      await navigator.clipboard.writeText(draft.webhook.token);
      setCopied(true);
      if (copiedTimer.current) window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard unavailable — the token is selectable next to the button.
    }
  }

  async function regenerateToken() {
    try {
      patchWebhook({ token: await generateToken() });
    } catch (err) {
      setErrorMessage((err as AppError).message);
    }
  }

  async function confirmClearLogs() {
    setConfirmClearOpen(false);
    try {
      await clearLogs();
      setLogs([]);
    } catch (err) {
      setErrorMessage((err as AppError).message);
    }
  }

  if (loading) {
    return (
      <Card>
        <CardContent className="flex items-center justify-center gap-2.5 py-14 text-[13px] text-muted-foreground">
          <Loader2 className="size-4 animate-spin" />
          {t("common.loading")}
        </CardContent>
      </Card>
    );
  }

  if (loadError || !status || !draft) {
    return (
      <Card>
        <EmptyState
          icon={CircleAlert}
          title={t("common.loadFailed")}
          message={t("common.loadFailedMsg")}
          actionLabel={t("common.retry")}
          onAction={() => void load()}
        />
      </Card>
    );
  }

  const schedule = draft.schedule;

  // Hours the user asked for plus their current values, so a config saved
  // with a wider range never silently loses its setting.
  const hours = Array.from(
    new Set([...Array.from({ length: 13 }, (_, i) => i + 8), schedule.startHour, schedule.endHour]),
  ).sort((a, b) => a - b);

  return (
    <div className="flex flex-col gap-4">
      {/* ------------------------------- Toolbar ---------------------------- */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2.5 text-[13px]">
          {status.config.schedule.enabled && status.nextRunAt ? (
            <>
              <CalendarClock className="size-4 text-primary" />
              <span className="text-muted-foreground">
                {t("automation.nextRun", {
                  time: formatWhen(status.nextRunAt, lang),
                })}
              </span>
            </>
          ) : (
            <span className="text-muted-foreground">
              {t("automation.scheduleOff")}
            </span>
          )}
          {busy && (
            <Badge tone="primary" className="gap-1">
              <Loader2 className="size-2.5 animate-spin" />
              {t("automation.runningNow")}
            </Badge>
          )}
        </div>

        <div className="flex items-center gap-2.5">
          {dirty && (
            <span className="text-[12px] text-warning">
              {t("automation.unsaved")}
            </span>
          )}
          <Button onClick={() => void save()} disabled={!dirty || saving}>
            {saving ? (
              <Loader2 className="size-4 animate-spin" />
            ) : savedFlash ? (
              <Check className="size-4 text-success" />
            ) : (
              <Save className="size-4" />
            )}
            {t("automation.save")}
          </Button>
        </div>
      </div>

      {errorMessage && (
        <div className="flex items-center gap-2.5 rounded-lg border border-destructive/30 bg-destructive/8 px-3.5 py-2.5 text-[12.5px] text-destructive">
          <CircleAlert className="size-4 shrink-0" />
          {errorMessage}
        </div>
      )}

      {/* ------------------------------ Schedule ---------------------------- */}
      <Card>
        <CardContent className="flex flex-col gap-4 pt-5">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h3 className="text-[14px] font-semibold tracking-tight">
                {t("automation.scheduleTitle")}
              </h3>
              <p className="mt-0.5 text-[12.5px] text-muted-foreground">
                {t("automation.scheduleDesc")}
              </p>
            </div>
            <div
              className="flex cursor-pointer items-center gap-2.5"
              onClick={() => patchSchedule({ enabled: !schedule.enabled })}
            >
              <Checkbox
                checked={schedule.enabled}
                onChange={(checked) => patchSchedule({ enabled: checked })}
                label={t("automation.enableSchedule")}
              />
              <span className="text-[13px] font-medium">
                {t("automation.enableSchedule")}
              </span>
            </div>
          </div>

          <div
            className={cn(
              "flex flex-col gap-4",
              !schedule.enabled && "pointer-events-none opacity-45",
            )}
          >
            <Field label={t("automation.days")}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Chip
                  active={schedule.days.length === 7}
                  onClick={() => patchSchedule({ days: [0, 1, 2, 3, 4, 5, 6] })}
                >
                  {t("automation.everyDay")}
                </Chip>
                <span className="mx-1 h-4 w-px bg-border" />
                {DAY_ORDER.map((day) => (
                  <Chip
                    key={day}
                    active={schedule.days.includes(day)}
                    onClick={() => toggleDay(day)}
                  >
                    {t(DAY_KEYS[day])}
                  </Chip>
                ))}
              </div>
            </Field>

            <div className="flex flex-wrap gap-5">
              <Field label={t("automation.timeRange")}>
                <div className="flex items-center gap-1.5">
                  <Select
                    value={schedule.startHour}
                    options={hours.map((hour) => ({
                      value: hour,
                      label: `${String(hour).padStart(2, "0")}:00`,
                    }))}
                    onChange={(hour) => patchSchedule({ startHour: hour })}
                  />
                  <span className="text-muted-foreground">–</span>
                  <Select
                    value={schedule.endHour}
                    options={hours.map((hour) => ({
                      value: hour,
                      label: `${String(hour).padStart(2, "0")}:00`,
                    }))}
                    onChange={(hour) => patchSchedule({ endHour: hour })}
                  />
                </div>
              </Field>

              <Field label={t("automation.interval")}>
                <Select
                  value={schedule.intervalHours}
                  options={INTERVAL_HOURS.map((count) => ({
                    value: count,
                    label: t("automation.hoursUnit", { count }),
                  }))}
                  onChange={(count) => patchSchedule({ intervalHours: count })}
                />
              </Field>
            </div>

            <div
              className="flex cursor-pointer items-center gap-2.5 border-t border-border/60 pt-3.5"
              onClick={() => patch({ emptyTrash: !draft.emptyTrash })}
            >
              <Checkbox
                checked={draft.emptyTrash}
                onChange={(checked) => patch({ emptyTrash: checked })}
                label={t("automation.emptyTrash")}
              />
              <span className="text-[13px]">{t("automation.emptyTrash")}</span>
            </div>
          </div>
        </CardContent>
      </Card>

      {/* ------------------------------ Triggers ---------------------------- */}
      <Card>
        <CardContent className="flex flex-col gap-4 pt-5">
          <div>
            <h3 className="text-[14px] font-semibold tracking-tight">
              {t("automation.triggersTitle")}
            </h3>
            <p className="mt-0.5 text-[12.5px] text-muted-foreground">
              {t("automation.triggersDesc")}
            </p>
          </div>

          <TriggerRow
            label={t("automation.diskTrigger")}
            hint={t("automation.diskTriggerHint")}
            trigger={draft.diskTrigger}
            onPatch={patchDiskTrigger}
          />
          <div className="h-px bg-border/60" />
          <TriggerRow
            label={t("automation.memoryTrigger")}
            hint={t("automation.memoryTriggerHint")}
            trigger={draft.memoryTrigger}
            onPatch={patchMemoryTrigger}
          />
        </CardContent>
      </Card>

      {/* ------------------------------- Webhook ---------------------------- */}
      <Card>
        <CardContent className="flex flex-col gap-4 pt-5">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h3 className="text-[14px] font-semibold tracking-tight">
                {t("automation.webhookTitle")}
              </h3>
              <p className="mt-0.5 text-[12.5px] text-muted-foreground">
                {t("automation.webhookDesc")}
              </p>
            </div>
            <div
              className="flex cursor-pointer items-center gap-2.5"
              onClick={() => patchWebhook({ enabled: !draft.webhook.enabled })}
            >
              <Checkbox
                checked={draft.webhook.enabled}
                onChange={(checked) => patchWebhook({ enabled: checked })}
                label={t("automation.enableWebhook")}
              />
              <span className="text-[13px] font-medium">
                {t("automation.enableWebhook")}
              </span>
            </div>
          </div>

          <div
            className={cn(
              "flex flex-col gap-3.5",
              !draft.webhook.enabled && "pointer-events-none opacity-45",
            )}
          >
            <div className="flex flex-wrap items-end gap-5">
              <Field label={t("automation.webhookPort")} className="w-24">
                <input
                  type="number"
                  min={1024}
                  max={65535}
                  value={draft.webhook.port}
                  onChange={(event) => {
                    const port = Number(event.target.value);
                    if (Number.isFinite(port) && port > 0) {
                      patchWebhook({ port: Math.round(port) });
                    }
                  }}
                  className="h-9 w-full rounded-lg border border-border bg-card/70 px-3 text-[13px] tabular-nums backdrop-blur-xl outline-none transition-all duration-200 focus:border-primary/50 focus:ring-2 focus:ring-ring/25"
                />
              </Field>

              <Field
                label={t("automation.webhookToken")}
                className="min-w-[300px] flex-1"
              >
                <div className="flex items-center gap-2">
                  <code
                    data-selectable
                    className="flex h-9 min-w-0 flex-1 items-center truncate rounded-lg border border-border bg-muted/60 px-3 font-mono text-[12px]"
                  >
                    {draft.webhook.token}
                  </code>
                  <Button variant="outline" onClick={() => void copyToken()}>
                    {copied ? (
                      <Check className="size-4 text-success" />
                    ) : (
                      <Copy className="size-4" />
                    )}
                    {copied ? t("automation.copied") : t("automation.copyToken")}
                  </Button>
                  <Button variant="outline" onClick={() => void regenerateToken()}>
                    <RefreshCw className="size-4" />
                    {t("automation.regenerateToken")}
                  </Button>
                </div>
              </Field>
            </div>

            <div className="flex items-center gap-2 text-[12px]">
              <span
                className={cn(
                  "size-1.5 shrink-0 rounded-full",
                  status.webhook.running
                    ? "bg-success"
                    : status.webhook.error
                      ? "bg-destructive"
                      : "bg-muted-foreground/50",
                )}
              />
              <span className="text-muted-foreground">
                {status.webhook.running && status.webhook.address
                  ? t("automation.webhookRunning", {
                      address: status.webhook.address,
                    })
                  : status.webhook.error
                    ? t("automation.webhookError", {
                        error: status.webhook.error,
                      })
                    : t("automation.webhookStopped")}
              </span>
            </div>

            <p className="text-[11.5px] text-muted-foreground">
              {t("automation.webhookLocalOnly")}
            </p>

            <div>
              <p className="mb-1.5 text-[11.5px] font-medium text-muted-foreground">
                {t("automation.webhookExamples")}
              </p>
              <pre
                data-selectable
                className="overflow-x-auto rounded-lg bg-muted/60 p-3 font-mono text-[11px] leading-relaxed text-foreground/80"
              >
{`curl -X POST http://127.0.0.1:${draft.webhook.port}/api/deep-clean -H "Authorization: Bearer ${draft.webhook.token}"
curl -X POST http://127.0.0.1:${draft.webhook.port}/api/memory-clean -H "Authorization: Bearer ${draft.webhook.token}"
curl http://127.0.0.1:${draft.webhook.port}/api/status -H "Authorization: Bearer ${draft.webhook.token}"`}
              </pre>
            </div>
          </div>
        </CardContent>
      </Card>

      {/* --------------------------------- Log ------------------------------ */}
      <Card className="overflow-hidden">
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-2.5">
          <div>
            <h3 className="text-[14px] font-semibold tracking-tight">
              {t("automation.logTitle")}
            </h3>
            <p className="mt-0.5 text-[11.5px] text-muted-foreground">
              {t("automation.logDesc")}
            </p>
          </div>

          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => void startRun("deep-clean")}
            >
              <Play className="size-3.5" />
              {t("automation.runDeepClean")}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => void startRun("memory-clean")}
            >
              <Zap className="size-3.5" />
              {t("automation.runMemoryClean")}
            </Button>
            <Button
              variant="ghost"
              size="icon"
              aria-label={t("automation.refresh")}
              onClick={() => void load()}
            >
              <RefreshCw className="size-4" />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              aria-label={t("automation.clearLog")}
              disabled={logs.length === 0}
              onClick={() => setConfirmClearOpen(true)}
            >
              <Trash2 className="size-4" />
            </Button>
          </div>
        </div>

        {logs.length === 0 ? (
          <EmptyState
            icon={ScrollText}
            title={t("automation.noLogs")}
            message={t("automation.noLogsMsg")}
          />
        ) : (
          <div className="flex flex-col">
            {logs.map((entry) => (
              <LogRow key={`${entry.at}-${entry.action}`} entry={entry} />
            ))}
          </div>
        )}
      </Card>

      <ConfirmDialog
        open={confirmClearOpen}
        destructive
        title={t("automation.clearLogTitle")}
        message={t("automation.clearLogMsg")}
        confirmLabel={t("automation.clear")}
        onConfirm={() => void confirmClearLogs()}
        onClose={() => setConfirmClearOpen(false)}
      />
    </div>
  );
}

/* ------------------------------ Small pieces ------------------------------ */

function Field({
  label,
  className,
  children,
}: {
  label: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      <span className="text-[11.5px] font-medium text-muted-foreground">
        {label}
      </span>
      {children}
    </div>
  );
}

function Chip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={cn(
        "rounded-[7px] border px-2.5 py-1 text-[12px] font-medium transition-all duration-150",
        active
          ? "border-primary/40 bg-primary/10 text-primary"
          : "border-border bg-card text-muted-foreground hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

function Select({
  value,
  options,
  onChange,
}: {
  value: number;
  options: { value: number; label: string }[];
  onChange: (value: number) => void;
}) {
  return (
    <div className="relative">
      <select
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
        className="h-9 appearance-none rounded-lg border border-border bg-card/70 pr-8 pl-3 text-[13px] font-medium tabular-nums backdrop-blur-xl outline-none transition-all duration-200 focus:border-primary/50 focus:ring-2 focus:ring-ring/25"
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
      <ChevronDown className="pointer-events-none absolute top-1/2 right-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
    </div>
  );
}

function TriggerRow({
  label,
  hint,
  trigger,
  onPatch,
}: {
  label: string;
  hint: string;
  trigger: ThresholdTrigger;
  onPatch: (changes: Partial<ThresholdTrigger>) => void;
}) {
  const { t } = useTranslation();

  return (
    <div className="flex flex-wrap items-center justify-between gap-3">
      <div
        className="flex cursor-pointer items-start gap-2.5"
        onClick={() => onPatch({ enabled: !trigger.enabled })}
      >
        <Checkbox
          checked={trigger.enabled}
          onChange={(checked) => onPatch({ enabled: checked })}
          label={label}
          className="mt-0.5"
        />
        <div>
          <p className="text-[13px] font-medium">{label}</p>
          <p className="mt-0.5 text-[11.5px] text-muted-foreground">{hint}</p>
        </div>
      </div>

      <div className="flex items-center gap-1.5">
        <span className="text-[12.5px] text-muted-foreground">
          {t("automation.threshold")}
        </span>
        <input
          type="number"
          min={1}
          max={100}
          value={trigger.thresholdPercent}
          disabled={!trigger.enabled}
          onChange={(event) => {
            const value = Number(event.target.value);
            if (Number.isFinite(value)) {
              onPatch({ thresholdPercent: clamp(Math.round(value), 1, 100) });
            }
          }}
          className="h-8 w-16 rounded-lg border border-border bg-card/70 px-2.5 text-[13px] tabular-nums backdrop-blur-xl outline-none transition-all duration-200 focus:border-primary/50 focus:ring-2 focus:ring-ring/25 disabled:opacity-45"
        />
        <span className="text-[12.5px]">%</span>
      </div>
    </div>
  );
}

function LogRow({ entry }: { entry: AutomationLogEntry }) {
  const { t, lang } = useTranslation();

  return (
    <div className="flex flex-col gap-1 border-b border-border/60 px-4 py-2.5 last:border-0">
      <div className="flex flex-wrap items-center gap-2.5">
        <span className="w-[104px] shrink-0 text-[11.5px] text-muted-foreground tabular-nums">
          {formatLogTime(entry.at, lang)}
        </span>
        <Badge tone={SOURCE_TONES[entry.source]}>
          {t(SOURCE_KEYS[entry.source])}
        </Badge>
        <span className="text-[12.5px] font-medium">
          {t(ACTION_KEYS[entry.action])}
        </span>
        {entry.freedBytes > 0 && (
          <span className="text-[12.5px] font-semibold text-success tabular-nums">
            +{formatBytes(entry.freedBytes)}
          </span>
        )}
        <span className="text-[11.5px] text-muted-foreground tabular-nums">
          {t("automation.itemsUnit", { count: entry.itemCount })}
        </span>
        <span className="text-[11.5px] text-muted-foreground tabular-nums">
          {(entry.durationMs / 1000).toFixed(1)}s
        </span>
        <span className="ml-auto">
          <Badge tone={entry.succeeded ? "success" : "danger"}>
            {entry.succeeded
              ? t("automation.result.ok")
              : t("automation.result.failed")}
          </Badge>
        </span>
      </div>
      {entry.message && (
        <p
          className="truncate text-[11.5px] text-muted-foreground"
          title={entry.message}
        >
          {entry.message}
        </p>
      )}
    </div>
  );
}
