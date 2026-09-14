import { useEffect, useMemo, useState } from "react";
import {
  ArrowUpDown,
  ChevronRight,
  CircleAlert,
  File,
  FileArchive,
  FileText,
  Film,
  Folder,
  HardDrive,
  Image,
  Loader2,
  Radar,
  RotateCcw,
  Trophy,
  type LucideIcon,
} from "lucide-react";
import { Button, Card, CardContent, CardHeader, CardTitle } from "@/components/ui";
import { EmptyState, StatTile } from "@/components/shared";
import { useTranslation } from "@/hooks/useTranslation";
import { cn, formatBytes, formatPercent } from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";
import { analyzePath, onAnalyzeProgress } from "@/services/analyze";
import type { AnalyzeResult, ProgressEvent, TreeNode } from "@/types";

/* ------------------------------ Type colours ----------------------------- */

interface TypeMeta {
  label: string;
  color: string;
  icon: LucideIcon;
}

const FILE_TYPES: [pattern: RegExp, meta: TypeMeta][] = [
  [/\.(mov|mp4|m4v|avi)$/i, { label: "Video", color: "#FF375F", icon: Film }],
  [/\.(zip|dmg|xip|iso|pkg|tar|gz)$/i, { label: "Archive", color: "#FF9500", icon: FileArchive }],
  [/\.(heic|png|jpg|jpeg|gif|tiff)$/i, { label: "Image", color: "#34C759", icon: Image }],
  [/\.(pdf|key|pages|numbers|sketch|txt|md)$/i, { label: "Document", color: "#5E5CE6", icon: FileText }],
  [/\.(sqlite|db|raw|csv|json)$/i, { label: "Data", color: "#00C7BE", icon: FileText }],
];

const FOLDER_PALETTE = ["#007AFF", "#4C6EF5", "#5E5CE6", "#7C5CE6", "#0A84FF"];

function typeMeta(node: TreeNode): TypeMeta {
  if (node.isDirectory) {
    let hash = 0;
    for (const char of node.name) hash = (hash * 31 + char.charCodeAt(0)) % 9973;
    return {
      label: "Folder",
      color: FOLDER_PALETTE[hash % FOLDER_PALETTE.length],
      icon: Folder,
    };
  }

  for (const [pattern, meta] of FILE_TYPES) {
    if (pattern.test(node.name)) return meta;
  }
  return { label: "Other", color: "#8E8E93", icon: File };
}

/* -------------------------------- Treemap -------------------------------- */

interface Rect {
  node: TreeNode;
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Virtual canvas — rects are converted to percentages of this box. */
const CANVAS_W = 1600;
const CANVAS_H = 600;

/**
 * Squarified treemap (Bruls et al.): fill the shorter edge with rows of blocks,
 * always picking the row that keeps aspect ratios closest to square.
 */
function squarify(nodes: TreeNode[], width: number, height: number): Rect[] {
  const rects: Rect[] = [];
  let remaining = nodes.filter((node) => node.size > 0).sort((a, b) => b.size - a.size);
  let totalSize = remaining.reduce((sum, node) => sum + node.size, 0);

  let x = 0;
  let y = 0;
  let w = width;
  let h = height;

  while (remaining.length > 0 && totalSize > 0 && w > 0.5 && h > 0.5) {
    const scale = (w * h) / totalSize;
    const vertical = w >= h;
    const side = vertical ? h : w;

    const row: TreeNode[] = [];
    let rowSize = 0;
    let worst = Number.POSITIVE_INFINITY;

    for (const node of remaining) {
      const candidateSize = rowSize + node.size;
      const thickness = (candidateSize * scale) / side;
      const candidateWorst = Math.max(
        ...[...row, node].map((entry) => {
          const length = (entry.size * scale) / thickness;
          return Math.max(thickness / length, length / thickness);
        }),
      );

      if (row.length > 0 && candidateWorst > worst) break;

      row.push(node);
      rowSize = candidateSize;
      worst = candidateWorst;
    }

    const thickness = (rowSize * scale) / side;
    let offset = 0;

    for (const node of row) {
      const length = (node.size * scale) / thickness;
      rects.push(
        vertical
          ? { node, x, y: y + offset, w: thickness, h: length }
          : { node, x: x + offset, y, w: length, h: thickness },
      );
      offset += length;
    }

    if (vertical) {
      x += thickness;
      w -= thickness;
    } else {
      y += thickness;
      h -= thickness;
    }

    totalSize -= rowSize;
    remaining = remaining.slice(row.length);
  }

  return rects;
}

/* --------------------------------- Sorting ------------------------------- */

type SortKey = "name" | "type" | "share" | "size";

export function DiskAnalyzer() {
  const status = useAppStore((state) => state.moduleStatus.analyze);
  const progress = useAppStore((state) => state.progress);
  const setProgress = useAppStore((state) => state.setProgress);
  const setModuleStatus = useAppStore((state) => state.setModuleStatus);
  const { t } = useTranslation();

  const [result, setResult] = useState<AnalyzeResult | null>(null);
  /** Path from the scan root down to the folder on screen. */
  const [stack, setStack] = useState<TreeNode[]>([]);
  const [sortKey, setSortKey] = useState<SortKey>("size");
  const [ascending, setAscending] = useState(false);
  const [hovered, setHovered] = useState<string | null>(null);
  const [error, setError] = useState(false);
  const [errorMsg, setErrorMsg] = useState("");

  // Subscribe to `analyze://progress` events for real-time scan progress.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    onAnalyzeProgress((event: ProgressEvent) => {
      if (cancelled) return;
      setProgress({
        progress: event.progress,
        currentPath: event.currentPath,
        itemsScanned: event.itemsScanned,
        bytesFound: event.bytesFound,
      });
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    }).catch(() => {});
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [setProgress]);

  const scanning = status === "scanning";
  const current = stack.at(-1) ?? null;
  const children = current?.children ?? [];

  const rects = useMemo(
    () => (children.length ? squarify(children, CANVAS_W, CANVAS_H) : []),
    [children],
  );

  const sorted = useMemo(() => {
    const factor = ascending ? 1 : -1;

    return [...children].sort((a, b) => {
      switch (sortKey) {
        case "name":
          return a.name.localeCompare(b.name) * factor;
        case "type":
          return typeMeta(a).label.localeCompare(typeMeta(b).label) * factor;
        case "share":
        case "size":
          return (a.size - b.size) * factor;
      }
    });
  }, [children, sortKey, ascending]);

  async function startScan() {
    setResult(null);
    setStack([]);
    setError(false);
    setErrorMsg("");

    setModuleStatus("analyze", "scanning");
    try {
      const realResult = await analyzePath("~", 3);
      setResult(realResult);
      setStack([realResult.root]);
      setModuleStatus("analyze", "ready");
    } catch (err) {
      console.error("[DiskAnalyzer] analyzePath failed:", err);
      const msg =
        err && typeof err === "object" && "message" in err
          ? String((err as { message: unknown }).message)
          : t("common.scanFailedMsg");
      setErrorMsg(msg);
      setProgress(null);
      setModuleStatus("analyze", "error");
      setError(true);
    }
  }

  function handleCancel() {
    setModuleStatus("analyze", "idle");
    setProgress(null);
  }

  function openNode(node: TreeNode) {
    if (!node.isDirectory || !node.children?.length) return;
    setStack((current) => [...current, node]);
  }

  function toggleSort(key: SortKey) {
    if (key === sortKey) {
      setAscending((value) => !value);
      return;
    }
    setSortKey(key);
    setAscending(key === "name" || key === "type");
  }

  if (!result) {
    return (
      <Card>
        {scanning ? (
          <CardContent className="flex flex-col items-center gap-3 px-8 py-14 text-center">
            <Loader2 className="size-6 animate-spin text-primary" />
            <p className="text-[14px] font-semibold tracking-tight">
              {t("analyzer.indexing", { bytes: formatBytes(progress?.bytesFound ?? 0) })}
            </p>
            <p className="max-w-md truncate font-mono text-[11px] text-muted-foreground">
              {progress?.currentPath ?? ""}
            </p>
            <Button variant="outline" className="mt-1" onClick={handleCancel}>
              {t("common.stop")}
            </Button>
          </CardContent>
        ) : error ? (
          <EmptyState
            icon={CircleAlert}
            title={t("common.scanFailed")}
            message={errorMsg || t("common.scanFailedMsg")}
            actionLabel={t("common.retry")}
            onAction={startScan}
          />
        ) : (
          <EmptyState
            icon={HardDrive}
            title={t("analyzer.mapDisk")}
            message={t("analyzer.mapDiskMsg")}
            actionLabel={t("analyzer.scanHomeFolder")}
            onAction={startScan}
          />
        )}
      </Card>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* ------------------------------ Breadcrumb -------------------------- */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <nav className="flex min-w-0 items-center gap-0.5 text-[13px]">
          <span className="shrink-0 text-muted-foreground">/Users</span>
          {stack.map((node, index) => (
            <span key={node.path} className="flex min-w-0 items-center gap-0.5">
              <ChevronRight className="size-3.5 shrink-0 text-muted-foreground/60" />
              <button
                type="button"
                onClick={() => setStack((current) => current.slice(0, index + 1))}
                className={cn(
                  "truncate rounded px-1.5 py-0.5 font-medium transition-colors duration-150",
                  index === stack.length - 1
                    ? "text-foreground"
                    : "text-muted-foreground hover:bg-accent hover:text-foreground",
                )}
              >
                {node.name}
              </button>
            </span>
          ))}
        </nav>

        <div className="flex items-center gap-2">
          {scanning ? (
            <Button variant="outline" onClick={handleCancel}>
              {t("common.stop")}
            </Button>
          ) : (
            <Button variant="outline" onClick={startScan}>
              <RotateCcw className="size-4" />
              {t("common.rescan")}
            </Button>
          )}
        </div>
      </div>

      {/* -------------------------------- Stats ----------------------------- */}
      <div className="grid gap-3 sm:grid-cols-3">
        <StatTile
          icon={Folder}
          label={t("analyzer.thisFolder")}
          value={formatBytes(current?.size ?? 0)}
          hint={t("analyzer.itemsInside", { count: children.length })}
        />
        <StatTile
          icon={HardDrive}
          label={t("analyzer.scanRoot")}
          value={formatBytes(result.root.size)}
          hint={result.root.path}
        />
        <StatTile
          icon={Radar}
          label={t("analyzer.scanTime")}
          value={`${(result.durationMs / 1000).toFixed(1)}s`}
          hint={t("analyzer.sizeMapHint")}
          accent="hsl(var(--primary))"
        />
      </div>

      {/* ------------------------------- Treemap ---------------------------- */}
      <Card className="overflow-hidden">
        <CardHeader className="flex-row items-center justify-between">
          <CardTitle>{current?.name ?? ""} — {t("analyzer.sizeMap")}</CardTitle>
          <span className="text-[11.5px] text-muted-foreground">
            {t("analyzer.sizeMapHint")}
          </span>
        </CardHeader>

        <CardContent>
          <div className="relative aspect-[8/3] w-full overflow-hidden rounded-lg bg-muted/40">
            {rects.map(({ node, x, y, w, h }) => {
              const meta = typeMeta(node);
              const navigable = node.isDirectory && (node.children?.length ?? 0) > 0;
              const showLabel = w / CANVAS_W > 0.07 && h / CANVAS_H > 0.12;

              return (
                <button
                  key={node.path}
                  type="button"
                  onClick={() => openNode(node)}
                  onMouseEnter={() => setHovered(node.path)}
                  onMouseLeave={() => setHovered(null)}
                  title={`${node.name} — ${formatBytes(node.size)}`}
                  className={cn(
                    "absolute overflow-hidden rounded-[5px] border border-background/60 p-1.5 text-left transition-all duration-200",
                    navigable ? "cursor-pointer" : "cursor-default",
                    hovered === node.path
                      ? "z-10 brightness-110 ring-2 ring-white/70"
                      : "brightness-100",
                  )}
                  style={{
                    left: `${(x / CANVAS_W) * 100}%`,
                    top: `${(y / CANVAS_H) * 100}%`,
                    width: `${(w / CANVAS_W) * 100}%`,
                    height: `${(h / CANVAS_H) * 100}%`,
                    background: `linear-gradient(140deg, ${meta.color}, ${meta.color}C0)`,
                  }}
                >
                  {showLabel && (
                    <span className="flex flex-col text-white drop-shadow-sm">
                      <span className="truncate text-[11.5px] font-semibold">
                        {node.name}
                      </span>
                      <span className="truncate text-[10.5px] opacity-85 tabular-nums">
                        {formatBytes(node.size)}
                      </span>
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        </CardContent>
      </Card>

      {/* --------------------------- List + sidebar ------------------------- */}
      <div className="grid gap-4 xl:grid-cols-5">
        <Card className="xl:col-span-3">
          <CardHeader>
            <CardTitle>{t("analyzer.contents")}</CardTitle>
          </CardHeader>

          <CardContent className="pb-2">
            <div className="grid grid-cols-[1fr_92px_84px_92px] items-center gap-3 border-b border-border pb-2">
              {(
                [
                  ["name", t("common.name")],
                  ["type", t("common.type")],
                  ["share", t("analyzer.share")],
                  ["size", t("common.size")],
                ] as [SortKey, string][]
              ).map(([key, label], index) => (
                <button
                  key={key}
                  type="button"
                  onClick={() => toggleSort(key)}
                  className={cn(
                    "flex items-center gap-1 text-[11px] font-semibold tracking-wide uppercase transition-colors duration-150 hover:text-foreground",
                    index > 0 && "justify-end",
                    sortKey === key ? "text-foreground" : "text-muted-foreground",
                  )}
                >
                  {label}
                  <ArrowUpDown
                    className={cn(
                      "size-3 transition-opacity",
                      sortKey === key ? "opacity-100" : "opacity-30",
                    )}
                  />
                </button>
              ))}
            </div>

            <div className="flex flex-col">
              {sorted.map((node) => {
                const meta = typeMeta(node);
                const navigable =
                  node.isDirectory && (node.children?.length ?? 0) > 0;
                const Icon = meta.icon;

                return (
                  <button
                    key={node.path}
                    type="button"
                    onClick={() => openNode(node)}
                    onMouseEnter={() => setHovered(node.path)}
                    onMouseLeave={() => setHovered(null)}
                    className={cn(
                      "grid grid-cols-[1fr_92px_84px_92px] items-center gap-3 border-b border-border/60 py-2 text-left transition-colors duration-150 last:border-0",
                      navigable && "cursor-pointer hover:bg-accent/50",
                      hovered === node.path && "bg-accent/50",
                    )}
                  >
                    <span className="flex min-w-0 items-center gap-2.5">
                      <Icon
                        className="size-4 shrink-0"
                        style={{ color: meta.color }}
                        strokeWidth={2}
                      />
                      <span className="truncate text-[12.5px] font-medium">
                        {node.name}
                      </span>
                      {navigable && (
                        <ChevronRight className="size-3.5 shrink-0 text-muted-foreground/70" />
                      )}
                    </span>

                    <span className="text-right text-[11.5px] text-muted-foreground">
                      {meta.label}
                    </span>

                    <span className="flex items-center justify-end gap-2">
                      <span className="h-1.5 w-10 overflow-hidden rounded-full bg-muted">
                        <span
                          className="block h-full rounded-full"
                          style={{
                            width: `${Math.max(3, node.share * 100)}%`,
                            backgroundColor: meta.color,
                          }}
                        />
                      </span>
                      <span className="w-8 text-right text-[11px] text-muted-foreground tabular-nums">
                        {formatPercent(node.share)}
                      </span>
                    </span>

                    <span className="text-right text-[12.5px] font-semibold tabular-nums">
                      {formatBytes(node.size)}
                    </span>
                  </button>
                );
              })}
            </div>
          </CardContent>
        </Card>

        <div className="flex flex-col gap-4 xl:col-span-2">
          <Card>
            <CardHeader className="flex-row items-center gap-2">
              <Trophy className="size-4 text-warning" />
              <CardTitle>{t("analyzer.topFiles")}</CardTitle>
            </CardHeader>

            <CardContent className="flex flex-col">
              {result.largestFiles.slice(0, 10).map((node, index) => {
                const meta = typeMeta(node);

                return (
                  <div
                    key={node.path}
                    className="flex items-center gap-2.5 border-b border-border/60 py-1.5 last:border-0"
                  >
                    <span className="w-4 text-right text-[11px] font-semibold text-muted-foreground tabular-nums">
                      {index + 1}
                    </span>
                    <span
                      className="size-2 shrink-0 rounded-full"
                      style={{ backgroundColor: meta.color }}
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[12px] font-medium">
                        {node.name}
                      </span>
                      <span
                        className="block truncate font-mono text-[10.5px] text-muted-foreground"
                        data-selectable
                      >
                        {node.path}
                      </span>
                    </span>
                    <span className="shrink-0 text-[12px] font-semibold tabular-nums">
                      {formatBytes(node.size)}
                    </span>
                  </div>
                );
              })}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>{t("analyzer.spaceByType")}</CardTitle>
            </CardHeader>

            <CardContent className="flex flex-col gap-2.5">
              {result.byType.map((entry) => {
                const share = entry.size / result.root.size;

                return (
                  <div key={entry.label} className="flex flex-col gap-1">
                    <div className="flex items-baseline justify-between gap-3 text-[12px]">
                      <span className="truncate font-medium">{entry.label}</span>
                      <span className="shrink-0 text-muted-foreground tabular-nums">
                        {formatBytes(entry.size)} ·{" "}
                        {entry.fileCount.toLocaleString()} {t("common.files")}
                      </span>
                    </div>
                    <span className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                      <span
                        className="block h-full rounded-full bg-primary transition-[width] duration-500"
                        style={{ width: `${Math.min(100, share * 100)}%` }}
                      />
                    </span>
                  </div>
                );
              })}
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  );
}
