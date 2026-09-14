import { useEffect, useState } from "react";
import { Thermometer, Fan, HardDrive } from "lucide-react";
import { useTranslation } from "@/hooks/useTranslation";
import { getDashboardSummary } from "@/services/dashboard";
import { NetworkWaveform, type MonitorSample } from "./NetworkWaveform";
import type { DiskUsage, SystemSnapshot } from "@/types";
import { formatBytes } from "@/lib/utils";

interface SystemStatusTabProps {
  snapshot: SystemSnapshot | null;
  history: MonitorSample[];
}

export function SystemStatusTab({ snapshot, history }: SystemStatusTabProps) {
  const { t } = useTranslation();
  const [diskInfo, setDiskInfo] = useState<DiskUsage | null>(null);

  // Poll disk usage every 10s.
  useEffect(() => {
    let cancelled = false;
    const fetchDisk = () => {
      getDashboardSummary()
        .then((s) => {
          if (!cancelled) setDiskInfo(s.disk);
        })
        .catch(() => {});
    };
    fetchDisk();
    const id = setInterval(fetchDisk, 10000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  const cpuTemp = snapshot?.cpu?.temperatureC;
  const fanRpm = snapshot?.cpu?.fanSpeedRpm;
  const diskPercent = diskInfo
    ? (diskInfo.usedBytes / diskInfo.totalBytes) * 100
    : 0;

  const tiles = [
    {
      icon: Thermometer,
      label: t("menubar.cpuTemp"),
      value: cpuTemp != null ? `${Math.round(cpuTemp)}°` : "—",
    },
    {
      icon: Fan,
      label: t("menubar.fanSpeed"),
      value: fanRpm != null ? `${Math.round(fanRpm)}` : "—",
    },
    {
      icon: HardDrive,
      label: t("menubar.diskUsage"),
      value: diskInfo ? `${Math.round(diskPercent)}%` : "—",
    },
  ];

  return (
    <div className="flex flex-col gap-3 px-3 py-3">
      {/* --------------------------- Stat tiles ------------------------------ */}
      <div className="grid grid-cols-3 gap-1.5">
        {tiles.map(({ icon: Icon, label, value }) => (
          <div
            key={label}
            className="flex flex-col items-center gap-1 rounded-lg bg-muted/50 px-1 py-2 text-center"
          >
            <Icon className="size-3.5 text-muted-foreground" strokeWidth={2} />
            <span className="text-[16px] font-bold leading-none tabular-nums">
              {value}
            </span>
            <span className="text-[9px] leading-tight tracking-wide text-muted-foreground uppercase">
              {label}
            </span>
          </div>
        ))}
      </div>

      {/* Disk detail line */}
      {diskInfo && (
        <p className="px-1 text-[10.5px] text-muted-foreground tabular-nums">
          {formatBytes(diskInfo.usedBytes)} / {formatBytes(diskInfo.totalBytes)}
        </p>
      )}

      {/* ------------------------- Network waveform -------------------------- */}
      <NetworkWaveform history={history} />
    </div>
  );
}
