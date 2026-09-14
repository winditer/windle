import { Area, AreaChart, ResponsiveContainer, YAxis } from "recharts";
import { ArrowDown, ArrowUp } from "lucide-react";
import { useTranslation } from "@/hooks/useTranslation";
import { formatBytes } from "@/lib/utils";

/** A single point in the rolling network waveform. */
export interface MonitorSample {
  time: number;
  netDown: number;
  netUp: number;
}

const COLOR_DOWN = "#34C759";
const COLOR_UP = "#AF52DE";

interface NetworkWaveformProps {
  history: MonitorSample[];
}

export function NetworkWaveform({ history }: NetworkWaveformProps) {
  const { t } = useTranslation();
  const latest = history[history.length - 1];
  const down = latest?.netDown ?? 0;
  const up = latest?.netUp ?? 0;

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center justify-between px-1">
        <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
          {t("menubar.networkStatus")}
        </span>
        <div className="flex items-center gap-3 text-[11px] tabular-nums">
          <span className="flex items-center gap-0.5">
            <ArrowDown className="size-3" style={{ color: COLOR_DOWN }} strokeWidth={2.5} />
            {formatBytes(down)}/s
          </span>
          <span className="flex items-center gap-0.5">
            <ArrowUp className="size-3" style={{ color: COLOR_UP }} strokeWidth={2.5} />
            {formatBytes(up)}/s
          </span>
        </div>
      </div>

      <div className="h-[100px] w-full">
        {history.length > 1 ? (
          <ResponsiveContainer width="100%" height="100%">
            <AreaChart data={history} margin={{ top: 4, right: 0, bottom: 0, left: 0 }}>
              <defs>
                <linearGradient id="mbNetDown" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor={COLOR_DOWN} stopOpacity={0.35} />
                  <stop offset="100%" stopColor={COLOR_DOWN} stopOpacity={0.02} />
                </linearGradient>
                <linearGradient id="mbNetUp" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor={COLOR_UP} stopOpacity={0.3} />
                  <stop offset="100%" stopColor={COLOR_UP} stopOpacity={0.02} />
                </linearGradient>
              </defs>
              <YAxis hide domain={[0, "dataMax"]} />
              <Area
                type="monotone"
                dataKey="netDown"
                stroke={COLOR_DOWN}
                strokeWidth={1.6}
                fill="url(#mbNetDown)"
                isAnimationActive={false}
                dot={false}
              />
              <Area
                type="monotone"
                dataKey="netUp"
                stroke={COLOR_UP}
                strokeWidth={1.6}
                fill="url(#mbNetUp)"
                isAnimationActive={false}
                dot={false}
              />
            </AreaChart>
          </ResponsiveContainer>
        ) : (
          <div className="flex h-full items-center justify-center text-[11px] text-muted-foreground">
            {t("menubar.networkStatus")}
          </div>
        )}
      </div>
    </div>
  );
}
