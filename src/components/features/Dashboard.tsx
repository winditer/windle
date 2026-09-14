import { useEffect, useState } from "react";
import {
  CircleAlert,
  Cpu,
  HardDrive,
  Loader2,
  MemoryStick,
  Trash2,
  Wand2,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { Button, Card, CardContent, CardHeader, CardTitle } from "@/components/ui";
import { Badge, EmptyState, StatRing } from "@/components/shared";
import { getDashboardSummary } from "@/services/dashboard";
import { useTranslation } from "@/hooks/useTranslation";
import {
  cn,
  formatBytes,
} from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";

function useGreeting(): string {
  const { t } = useTranslation();
  const hour = new Date().getHours();
  if (hour < 5) return t("dashboard.greetingLateNight");
  if (hour < 12) return t("dashboard.greetingMorning");
  if (hour < 18) return t("dashboard.greetingAfternoon");
  return t("dashboard.greetingEvening");
}

export function Dashboard() {
  const summary = useAppStore((state) => state.summary);
  const setSummary = useAppStore((state) => state.setSummary);
  const setActiveModule = useAppStore((state) => state.setActiveModule);
  const { t } = useTranslation();
  const greeting = useGreeting();

  const [error, setError] = useState(false);
  const [retryNonce, setRetryNonce] = useState(0);
  const loading = !summary && !error; // Derived from store state, not local useState

  // Initial fetch + 5-second polling. On error, stop polling so the user
  // sees the error state with a retry button instead of silent failures.
  useEffect(() => {
    let cancelled = false;
    let intervalId: ReturnType<typeof setInterval> | null = null;
    let requestId = 0;

    setError(false);

    const fetchData = () => {
      const id = ++requestId;
      getDashboardSummary()
        .then((data) => {
          if (cancelled || id !== requestId) return;
          setError(false);
          setSummary(data);
        })
        .catch(() => {
          if (cancelled || id !== requestId) return;
          setError(true);
          if (intervalId) clearInterval(intervalId);
        });
    };

    fetchData(); // initial fetch
    intervalId = setInterval(fetchData, 5000); // poll every 5s

    return () => {
      cancelled = true;
      if (intervalId) clearInterval(intervalId);
    };
  }, [setSummary, retryNonce]);

  if (loading) {
    return (
      <Card>
        <CardContent className="flex flex-col items-center gap-3 px-8 py-14 text-center">
          <Loader2 className="size-6 animate-spin text-primary" />
          <p className="text-[14px] font-medium">
            {t("dashboard.loading")}
          </p>
        </CardContent>
      </Card>
    );
  }

  if (error || !summary) {
    return (
      <Card>
        <EmptyState
          icon={CircleAlert}
          title={t("dashboard.loadFailed")}
          message={t("dashboard.loadFailedMsg")}
          actionLabel={t("common.retry")}
          onAction={() => { setRetryNonce((n) => n + 1); }}
        />
      </Card>
    );
  }

  const { disk, memory, cpu } = summary;

  const diskPercent = disk ? (disk.usedBytes / disk.totalBytes) * 100 : 0;
  const memoryPercent = memory ? (memory.usedBytes / memory.totalBytes) * 100 : 0;
  const cpuPercent = (cpu?.usage ?? 0) * 100;

  const quickStats = [
    { label: t("dashboard.freedAllTime"), value: formatBytes(summary.totalFreedBytes) },
    { label: t("dashboard.reclaimableNow"), value: formatBytes(summary.junkSize) },
    { label: t("dashboard.appsInstalled"), value: `${summary.appCount}` },
    {
      label: t("dashboard.freeSpace"),
      value: disk ? formatBytes(disk.availableBytes) : "—",
    },
  ];

  return (
    <div className="flex flex-col gap-4">
      {/* ------------------------------- Header ------------------------------ */}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h2 className="text-[22px] leading-tight font-semibold tracking-tight">
            {greeting}
          </h2>
          <p className="mt-1 text-[13px] text-muted-foreground">
            {summary.lastCleanAt
              ? `${t("dashboard.lastCleaned")} ${new Date(summary.lastCleanAt).toLocaleDateString()}`
              : t("dashboard.quickActions")}
          </p>
        </div>

        <div className="flex items-center gap-2">
          <Badge tone="success">{t("dashboard.healthy")}</Badge>
        </div>
      </div>

      {/* -------------------------------- Rings ----------------------------- */}
      <div className="grid gap-4 md:grid-cols-3">
        <GaugeCard
          icon={Cpu}
          title={t("dashboard.processor")}
          value={cpuPercent}
          caption={`Load ${cpu?.loadAverage[0].toFixed(2) ?? "—"}`}
          color="hsl(var(--primary))"
          rows={[
            [
              t("dashboard.temperature"),
              cpu?.temperatureC != null ? `${Math.round(cpu.temperatureC)} °C` : "—",
            ],
            [
              t("dashboard.fanSpeed"),
              cpu?.fanSpeedRpm != null ? `${Math.round(cpu.fanSpeedRpm)} RPM` : "—",
            ],
          ]}
        />

        <GaugeCard
          icon={MemoryStick}
          title={t("dashboard.memory")}
          value={memoryPercent}
          caption={
            memory
              ? `${formatBytes(memory.usedBytes)} / ${formatBytes(memory.totalBytes)}`
              : "—"
          }
          color="#5E5CE6"
          rows={[
            [
              t("dashboard.pressure"),
              memory ? `${Math.round(memory.pressure * 100)}%` : "—",
            ],
            [
              t("dashboard.swap"),
              memory
                ? `${formatBytes(memory.swapUsedBytes)} / ${formatBytes(memory.swapTotalBytes)}`
                : "—",
            ],
          ]}
        />

        <GaugeCard
          icon={HardDrive}
          title={t("dashboard.startupDisk")}
          value={diskPercent}
          caption={
            disk
              ? `${formatBytes(disk.usedBytes)} / ${formatBytes(disk.totalBytes)}`
              : "—"
          }
          color={diskPercent > 85 ? "hsl(var(--destructive))" : "hsl(var(--warning))"}
          rows={[
            [t("dashboard.volume"), disk ? `${disk.name} · ${disk.fileSystem}` : "—"],
            [t("dashboard.available"), disk ? formatBytes(disk.availableBytes) : "—"],
          ]}
        />
      </div>

      {/* ---------------------------- Quick actions ------------------------- */}
      <Card className="overflow-hidden bg-card/70 backdrop-blur-xl">
        <div className="flex flex-wrap items-center justify-between gap-5 p-5">
          <div className="flex items-center gap-4">
            <span className="flex size-11 items-center justify-center rounded-xl bg-primary/12 text-primary">
              <Wand2 className="size-5" strokeWidth={1.9} />
            </span>
            <div>
              <h3 className="text-[15px] font-semibold tracking-tight">
                {t("dashboard.quickActions")}
              </h3>
              <p className="text-[12.5px] text-muted-foreground">
                {formatBytes(summary.junkSize)} {t("dashboard.junkSize").toLowerCase()} · {summary.appCount} {t("dashboard.appsInstalled").toLowerCase()}
              </p>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button size="lg" onClick={() => setActiveModule("clean")}>
              <Trash2 className="size-4" />
              {t("dashboard.quickClean")}
            </Button>
            <Button
              size="lg"
              variant="outline"
              onClick={() => setActiveModule("optimize")}
            >
              <Zap className="size-4" />
              {t("dashboard.quickOptimize")}
            </Button>
          </div>
        </div>

        <div className="grid grid-cols-2 divide-x divide-border border-t border-border sm:grid-cols-4">
          {quickStats.map((item) => (
            <div key={item.label} className="px-5 py-3">
              <p className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
                {item.label}
              </p>
              <p className="text-[15px] font-semibold tabular-nums">
                {item.value}
              </p>
            </div>
          ))}
        </div>
      </Card>
    </div>
  );
}

interface GaugeCardProps {
  icon: LucideIcon;
  title: string;
  value: number;
  caption: string;
  color: string;
  rows: [label: string, value: string][];
}

function GaugeCard({
  icon: Icon,
  title,
  value,
  caption,
  color,
  rows,
}: GaugeCardProps) {
  return (
    <Card className="bg-card/70 backdrop-blur-xl">
      <CardHeader className="flex-row items-center gap-2">
        <Icon className="size-4" style={{ color }} strokeWidth={2} />
        <CardTitle>{title}</CardTitle>
      </CardHeader>

      <CardContent className="flex flex-col items-center gap-4">
        <StatRing value={value} label="" caption={caption} color={color} />

        <dl className="w-full">
          {rows.map(([label, rowValue], index) => (
            <div
              key={label}
              className={cn(
                "flex items-center justify-between gap-3 py-1.5 text-[12px]",
                index > 0 && "border-t border-border/70",
              )}
            >
              <dt className="text-muted-foreground">{label}</dt>
              <dd className="truncate font-medium">{rowValue}</dd>
            </div>
          ))}
        </dl>
      </CardContent>
    </Card>
  );
}
