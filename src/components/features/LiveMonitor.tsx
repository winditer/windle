import { useEffect, useMemo, useRef, useState } from "react";
import {
  Area,
  AreaChart,
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import {
  Activity,
  CircleAlert,
  Cpu,
  Gauge,
  HardDrive,
  Loader2,
  MemoryStick,
  Network,
  Pause,
  Play,
  Server,
  type LucideIcon,
} from "lucide-react";
import { Button, Card, CardContent, CardHeader, CardTitle } from "@/components/ui";
import { Badge, EmptyState } from "@/components/shared";
import { useTranslation } from "@/hooks/useTranslation";
import { cn, formatBytes, formatDuration } from "@/lib/utils";
import { getSnapshot, onSnapshot, startMonitor, stopMonitor } from "@/services/monitor";
import type { ProcessInfo, SystemSnapshot } from "@/types";

/** One sample in the rolling chart series. */
interface MonitorSample {
  t: number;
  time: string;
  cpu: number;
  memUsed: number;
  memCached: number;
  memFree: number;
  diskRead: number;
  diskWrite: number;
  netDown: number;
  netUp: number;
}

/** Rolling buffer length — five minutes of one-second samples. */
const HISTORY_SIZE = 300;

/** Selectable chart windows, in samples (= seconds). */
const RANGES: [samples: number, label: string][] = [
  [60, "60s"],
  [HISTORY_SIZE, "5m"],
];

const COLORS = {
  cpu: "#0A84FF",
  memUsed: "#5E5CE6",
  memCached: "#00C7BE",
  memFree: "#8E8E93",
  diskRead: "#FF9500",
  diskWrite: "#FF375F",
  netDown: "#34C759",
  netUp: "#AF52DE",
};

const TOOLTIP_STYLE = {
  borderRadius: 10,
  border: "1px solid hsl(var(--border))",
  backgroundColor: "hsl(var(--popover))",
  color: "hsl(var(--popover-foreground))",
  fontSize: 11.5,
  padding: "6px 10px",
  boxShadow: "0 8px 24px rgb(0 0 0 / 0.16)",
};

const LABEL_STYLE = { color: "hsl(var(--muted-foreground))" };

const AXIS_PROPS = {
  stroke: "hsl(var(--muted-foreground))",
  tick: { fontSize: 10 },
  tickLine: false,
  axisLine: false,
} as const;

const CHART_MARGIN = { top: 6, right: 6, bottom: 0, left: -18 };

interface HostSnapshot {
  hostname: string;
  osVersion: string;
  cpuBrand: string;
  coreCount: number;
  cpuTemp: number | null;
  memPressure: number;
  uptimeSeconds: number;
}

function snapshotToHost(snap: SystemSnapshot): HostSnapshot {
  return {
    hostname: snap.host.hostname ?? "—",
    osVersion: snap.host.osVersion ?? "—",
    cpuBrand: snap.host.cpuBrand ?? "—",
    coreCount: snap.host.physicalCores ?? 0,
    cpuTemp: snap.cpu.temperatureC,
    memPressure: snap.memory.pressure,
    uptimeSeconds: snap.uptimeSeconds,
  };
}

function snapshotToSample(snap: SystemSnapshot): MonitorSample {
  const now = Date.now();
  return {
    t: now,
    time: new Date(now).toLocaleTimeString("en-US", {
      minute: "2-digit",
      second: "2-digit",
    }),
    cpu: snap.cpu.usage * 100,
    memUsed: snap.memory.usedBytes / 1_000_000_000,
    memCached: (snap.memory.totalBytes - snap.memory.usedBytes - snap.memory.availableBytes) / 1_000_000_000,
    memFree: snap.memory.availableBytes / 1_000_000_000,
    diskRead: snap.diskIo.readBytesPerSec / 1_000_000,
    diskWrite: snap.diskIo.writeBytesPerSec / 1_000_000,
    netDown: (snap.network[0]?.rxBytesPerSec ?? 0) / 1_000_000,
    netUp: (snap.network[0]?.txBytesPerSec ?? 0) / 1_000_000,
  };
}

export function LiveMonitor() {
  const { t } = useTranslation();
  const [history, setHistory] = useState<MonitorSample[]>([]);
  const [range, setRange] = useState(60);
  const [live, setLive] = useState(true);
  const [hostInfo, setHostInfo] = useState<HostSnapshot | null>(null);
  const [processes, setProcesses] = useState<ProcessInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [retryCount, setRetryCount] = useState(0);
  const liveRef = useRef(true);

  useEffect(() => {
    liveRef.current = live;
  }, [live]);

  // Connect to the real Tauri monitor stream.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    async function connect() {
      try {
        // Grab one snapshot immediately so the host info panel is populated.
        const initial = await getSnapshot();
        if (cancelled) return;

        setHostInfo(snapshotToHost(initial));
        setProcesses(initial.topProcesses);
        setHistory([snapshotToSample(initial)]);
        setLoading(false);

        // Start the streaming backend and subscribe to its events.
        await startMonitor(1000);

        const fn = await onSnapshot((snapshot) => {
          if (cancelled) return;
          if (!liveRef.current) return;

          setHistory((current) => [
            ...current.slice(-(HISTORY_SIZE - 1)),
            snapshotToSample(snapshot),
          ]);
          setProcesses(snapshot.topProcesses);
          setHostInfo(snapshotToHost(snapshot));
        });
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      } catch (err) {
        console.error("[LiveMonitor] connect failed:", err);
        if (cancelled) return;
        setError(true);
        setLoading(false);
      }
    }

    void connect();

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
      stopMonitor().catch(() => {});
    };
  }, [retryCount]);

  const series = useMemo(() => history.slice(-range), [history, range]);
  const latest = series[series.length - 1];

  const averages = useMemo(() => {
    if (series.length === 0) {
      return { cpu: 0, diskRead: 0, netDown: 0 };
    }
    const mean = (pick: (sample: MonitorSample) => number) =>
      series.reduce((sum, sample) => sum + pick(sample), 0) / series.length;

    return {
      cpu: mean((sample) => sample.cpu),
      diskRead: mean((sample) => sample.diskRead),
      netDown: mean((sample) => sample.netDown),
    };
  }, [series]);

  const memoryTotal = latest ? latest.memUsed + latest.memCached + latest.memFree : 0;

  if (loading) {
    return (
      <Card>
        <CardContent className="flex flex-col items-center gap-3 px-8 py-14 text-center">
          <Loader2 className="size-6 animate-spin text-primary" />
          <p className="text-[14px] font-medium">{t("common.loading")}</p>
        </CardContent>
      </Card>
    );
  }

  if (error) {
    return (
      <Card>
        <EmptyState
          icon={CircleAlert}
          title={t("monitor.connectFailed")}
          message={t("monitor.connectFailedMsg")}
          actionLabel={t("common.retry")}
          onAction={() => {
            setError(false);
            setLoading(true);
            setRetryCount((c) => c + 1);
          }}
        />
      </Card>
    );
  }

  if (!latest) {
    return (
      <Card>
        <CardContent className="flex flex-col items-center gap-3 px-8 py-14 text-center">
          <Loader2 className="size-6 animate-spin text-primary" />
          <p className="text-[14px] font-medium">{t("monitor.waitingData")}</p>
        </CardContent>
      </Card>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* -------------------------------- Header ---------------------------- */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2.5">
          <span className="relative flex size-2.5">
            {live && (
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-success/70" />
            )}
            <span
              className={cn(
                "relative inline-flex size-2.5 rounded-full",
                live ? "bg-success" : "bg-muted-foreground",
              )}
            />
          </span>
          <span className="text-[13px] font-medium">
            {live ? t("monitor.streaming") : t("monitor.paused")}
          </span>
          <Badge tone="neutral">
            {t("monitor.buffered", { seconds: history.length })}
          </Badge>
        </div>

        <div className="flex items-center gap-2">
          <div className="flex items-center gap-1 rounded-lg bg-muted p-0.5">
            {RANGES.map(([samples, label]) => (
              <button
                key={label}
                type="button"
                onClick={() => setRange(samples)}
                className={cn(
                  "rounded-[7px] px-2.5 py-1 text-[12px] font-medium transition-all duration-150",
                  range === samples
                    ? "bg-card text-foreground shadow-sm"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {label}
              </button>
            ))}
          </div>

          <Button variant="outline" onClick={() => setLive((value) => !value)}>
            {live ? <Pause className="size-4" /> : <Play className="size-4" />}
            {live ? t("monitor.pause") : t("monitor.resume")}
          </Button>
        </div>
      </div>

      {/* -------------------------------- Charts ---------------------------- */}
      <div className="grid gap-4 xl:grid-cols-2">
        <ChartCard
          icon={Cpu}
          title={t("monitor.cpuUsage")}
          value={`${latest.cpu.toFixed(0)}%`}
          accent={COLORS.cpu}
          hint={`${t("monitor.avg")} ${averages.cpu.toFixed(0)}% · ${t("monitor.coresCount", { count: hostInfo?.coreCount ?? 0 })}`}
        >
          <ResponsiveContainer width="100%" height={168}>
            <AreaChart data={series} margin={CHART_MARGIN}>
              <defs>
                <linearGradient id="cpuFill" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor={COLORS.cpu} stopOpacity={0.35} />
                  <stop offset="100%" stopColor={COLORS.cpu} stopOpacity={0.02} />
                </linearGradient>
              </defs>
              <CartesianGrid
                strokeDasharray="3 3"
                stroke="hsl(var(--border))"
                vertical={false}
              />
              <XAxis dataKey="time" minTickGap={48} {...AXIS_PROPS} />
              <YAxis domain={[0, 100]} unit="%" width={44} {...AXIS_PROPS} />
              <Tooltip
                contentStyle={TOOLTIP_STYLE}
                labelStyle={LABEL_STYLE}
                formatter={(value) => [`${Number(value).toFixed(1)}%`, "CPU"]}
              />
              <Area
                type="monotone"
                dataKey="cpu"
                stroke={COLORS.cpu}
                strokeWidth={2}
                fill="url(#cpuFill)"
                isAnimationActive={false}
              />
            </AreaChart>
          </ResponsiveContainer>
        </ChartCard>

        <ChartCard
          icon={MemoryStick}
          title={t("monitor.memoryUsage")}
          value={`${latest.memUsed.toFixed(1)} GB`}
          accent={COLORS.memUsed}
          hint={t("monitor.memoryHint", { total: memoryTotal.toFixed(0), pressure: Math.round((hostInfo?.memPressure ?? 0) * 100) })}
          legend={[
            [t("monitor.used"), COLORS.memUsed],
            [t("monitor.cached"), COLORS.memCached],
            [t("monitor.free"), COLORS.memFree],
          ]}
        >
          <ResponsiveContainer width="100%" height={168}>
            <AreaChart data={series} margin={CHART_MARGIN}>
              <CartesianGrid
                strokeDasharray="3 3"
                stroke="hsl(var(--border))"
                vertical={false}
              />
              <XAxis dataKey="time" minTickGap={48} {...AXIS_PROPS} />
              <YAxis domain={[0, 18]} unit=" GB" width={44} {...AXIS_PROPS} />
              <Tooltip
                contentStyle={TOOLTIP_STYLE}
                labelStyle={LABEL_STYLE}
                formatter={(value) => `${Number(value).toFixed(2)} GB`}
              />
              <Area
                type="monotone"
                dataKey="memUsed"
                name="Used"
                stackId="mem"
                stroke={COLORS.memUsed}
                fill={COLORS.memUsed}
                fillOpacity={0.3}
                strokeWidth={1.8}
                isAnimationActive={false}
              />
              <Area
                type="monotone"
                dataKey="memCached"
                name="Cached"
                stackId="mem"
                stroke={COLORS.memCached}
                fill={COLORS.memCached}
                fillOpacity={0.24}
                strokeWidth={1.8}
                isAnimationActive={false}
              />
              <Area
                type="monotone"
                dataKey="memFree"
                name="Free"
                stackId="mem"
                stroke={COLORS.memFree}
                fill={COLORS.memFree}
                fillOpacity={0.14}
                strokeWidth={1.5}
                isAnimationActive={false}
              />
            </AreaChart>
          </ResponsiveContainer>
        </ChartCard>

        <ChartCard
          icon={HardDrive}
          title={t("monitor.diskIoTitle")}
          value={`${latest.diskRead.toFixed(0)} / ${latest.diskWrite.toFixed(0)} MB/s`}
          accent={COLORS.diskRead}
          hint={t("monitor.avgRead", { mb: averages.diskRead.toFixed(0) })}
          legend={[
            [t("monitor.diskRead"), COLORS.diskRead],
            [t("monitor.diskWrite"), COLORS.diskWrite],
          ]}
        >
          <ResponsiveContainer width="100%" height={168}>
            <LineChart data={series} margin={CHART_MARGIN}>
              <CartesianGrid
                strokeDasharray="3 3"
                stroke="hsl(var(--border))"
                vertical={false}
              />
              <XAxis dataKey="time" minTickGap={48} {...AXIS_PROPS} />
              <YAxis unit=" MB" width={44} {...AXIS_PROPS} />
              <Tooltip
                contentStyle={TOOLTIP_STYLE}
                labelStyle={LABEL_STYLE}
                formatter={(value) => `${Number(value).toFixed(1)} MB/s`}
              />
              <Line
                type="monotone"
                dataKey="diskRead"
                name="Read"
                stroke={COLORS.diskRead}
                strokeWidth={2}
                dot={false}
                isAnimationActive={false}
              />
              <Line
                type="monotone"
                dataKey="diskWrite"
                name="Write"
                stroke={COLORS.diskWrite}
                strokeWidth={2}
                dot={false}
                isAnimationActive={false}
              />
            </LineChart>
          </ResponsiveContainer>
        </ChartCard>

        <ChartCard
          icon={Network}
          title={t("monitor.networkTitle")}
          value={`↓ ${latest.netDown.toFixed(1)} · ↑ ${latest.netUp.toFixed(1)} MB/s`}
          accent={COLORS.netDown}
          hint={t("monitor.avgDown", { mb: averages.netDown.toFixed(1) })}
          legend={[
            [t("monitor.download"), COLORS.netDown],
            [t("monitor.upload"), COLORS.netUp],
          ]}
        >
          <ResponsiveContainer width="100%" height={168}>
            <LineChart data={series} margin={CHART_MARGIN}>
              <CartesianGrid
                strokeDasharray="3 3"
                stroke="hsl(var(--border))"
                vertical={false}
              />
              <XAxis dataKey="time" minTickGap={48} {...AXIS_PROPS} />
              <YAxis unit=" MB" width={44} {...AXIS_PROPS} />
              <Tooltip
                contentStyle={TOOLTIP_STYLE}
                labelStyle={LABEL_STYLE}
                formatter={(value) => `${Number(value).toFixed(2)} MB/s`}
              />
              <Line
                type="monotone"
                dataKey="netDown"
                name="Download"
                stroke={COLORS.netDown}
                strokeWidth={2}
                dot={false}
                isAnimationActive={false}
              />
              <Line
                type="monotone"
                dataKey="netUp"
                name="Upload"
                stroke={COLORS.netUp}
                strokeWidth={2}
                dot={false}
                isAnimationActive={false}
              />
            </LineChart>
          </ResponsiveContainer>
        </ChartCard>
      </div>

      {/* -------------------------- System + processes ----------------------- */}
      <div className="grid gap-4 lg:grid-cols-5">
        <Card className="lg:col-span-2">
          <CardHeader className="flex-row items-center gap-2">
            <Server className="size-4 text-muted-foreground" />
            <CardTitle>{t("monitor.system")}</CardTitle>
          </CardHeader>

          <CardContent>
            <dl className="flex flex-col">
              {(
                [
                  [t("monitor.hostname"), hostInfo?.hostname ?? "—"],
                  [t("monitor.osVersion"), hostInfo?.osVersion ?? "—"],
                  [t("monitor.processor"), hostInfo?.cpuBrand ?? "—"],
                  [t("monitor.memory"), formatBytes(memoryTotal * 1_000_000_000)],
                  [
                    t("monitor.cpuTemp"),
                    hostInfo?.cpuTemp != null
                      ? `${Math.round(hostInfo.cpuTemp)} °C`
                      : t("monitor.notReported"),
                  ],
                  [t("monitor.uptime"), formatDuration(hostInfo?.uptimeSeconds ?? 0)],
                ] as [string, string][]
              ).map(([label, value], index) => (
                <div
                  key={label}
                  className={cn(
                    "flex items-baseline justify-between gap-3 py-2 text-[12.5px]",
                    index > 0 && "border-t border-border/60",
                  )}
                >
                  <dt className="shrink-0 text-muted-foreground">{label}</dt>
                  <dd className="truncate font-medium" data-selectable>
                    {value}
                  </dd>
                </div>
              ))}
            </dl>
          </CardContent>
        </Card>

        <Card className="lg:col-span-3">
          <CardHeader className="flex-row items-center justify-between">
            <div className="flex items-center gap-2">
              <Activity className="size-4 text-muted-foreground" />
              <CardTitle>{t("monitor.topProcesses")}</CardTitle>
            </div>
            <span className="flex items-center gap-3 text-[11px] text-muted-foreground">
              <span className="flex items-center gap-1">
                <Gauge className="size-3" />
                {t("monitor.cpu")}
              </span>
              <span className="flex items-center gap-1">
                <MemoryStick className="size-3" />
                {t("monitor.memory")}
              </span>
            </span>
          </CardHeader>

          <CardContent className="flex flex-col">
            {processes.length > 0 ? (
              processes.map((process, index) => (
                <div
                  key={process.pid}
                  className={cn(
                    "grid grid-cols-[1fr_84px_80px] items-center gap-3 py-2",
                    index > 0 && "border-t border-border/60",
                  )}
                >
                  <div className="min-w-0">
                    <p className="truncate text-[12.5px] font-medium">
                      {process.name}
                    </p>
                    <p className="truncate font-mono text-[10.5px] text-muted-foreground">
                      pid {process.pid} · {process.user ?? "—"}
                    </p>
                  </div>

                  <div className="flex items-center justify-end gap-2">
                    <span className="h-1.5 w-9 overflow-hidden rounded-full bg-muted">
                      <span
                        className="block h-full rounded-full bg-primary"
                        style={{ width: `${Math.min(100, process.cpuUsage * 100)}%` }}
                      />
                    </span>
                    <span className="text-[11.5px] tabular-nums">
                      {Math.round(process.cpuUsage * 100)}%
                    </span>
                  </div>

                  <span className="text-right text-[12px] font-medium tabular-nums">
                    {formatBytes(process.memoryBytes)}
                  </span>
                </div>
              ))
            ) : (
              <p className="py-6 text-center text-[13px] text-muted-foreground">
                {t("monitor.waitingData")}
              </p>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}

interface ChartCardProps {
  icon: LucideIcon;
  title: string;
  value: string;
  hint?: string;
  accent: string;
  legend?: [label: string, color: string][];
  children: React.ReactNode;
}

function ChartCard({
  icon: Icon,
  title,
  value,
  hint,
  accent,
  legend,
  children,
}: ChartCardProps) {
  return (
    <Card className="bg-card/70 backdrop-blur-xl">
      <CardHeader className="flex-row items-start justify-between gap-3">
        <div className="flex items-center gap-2">
          <Icon className="size-4" style={{ color: accent }} strokeWidth={2} />
          <div>
            <CardTitle>{title}</CardTitle>
            {hint && (
              <p className="text-[11px] text-muted-foreground tabular-nums">{hint}</p>
            )}
          </div>
        </div>

        <div className="text-right">
          <p
            className="text-[17px] leading-none font-semibold tabular-nums"
            style={{ color: accent }}
          >
            {value}
          </p>
          {legend && (
            <div className="mt-1.5 flex items-center justify-end gap-2.5">
              {legend.map(([label, color]) => (
                <span
                  key={label}
                  className="flex items-center gap-1 text-[10.5px] text-muted-foreground"
                >
                  <span
                    className="size-2 rounded-full"
                    style={{ backgroundColor: color }}
                  />
                  {label}
                </span>
              ))}
            </div>
          )}
        </div>
      </CardHeader>

      <CardContent className="pb-3">{children}</CardContent>
    </Card>
  );
}
