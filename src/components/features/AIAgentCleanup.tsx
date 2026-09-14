import { useEffect, useMemo, useRef, useState } from "react";
import {
  Bot,
  CircleAlert,
  Database,
  Loader2,
  RotateCcw,
  ShieldCheck,
  Sparkles,
  Trash2,
  Wand2,
} from "lucide-react";
import { Button, Card, CardContent, Progress } from "@/components/ui";
import {
  Badge,
  Checkbox,
  ConfirmDialog,
  EmptyState,
  StatTile,
} from "@/components/shared";
import { useTranslation } from "@/hooks/useTranslation";
import { cn, formatBytes } from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";
import { removeAiData, scanAiAgents } from "@/services/agent";
import type { AiAgentScanResult, AiDataItem, AiTool } from "@/types";

const TOOL_LABELS: Record<AiTool, string> = {
  trae: "Trae",
  qoder: "Qoder",
  codex: "Codex",
  real: "Real",
  yuanbao: "Yuanbao",
  openclaw: "Openclaw",
  comate: "Comate",
  opencode: "OpenCode",
  "cc-switch": "cc-switch",
  "claude-code": "Claude Code",
  "gemini-cli": "Gemini CLI",
  omega: "Omega",
  dsh: "DeepSeek DSH",
  other: "Other",
};

export function AIAgentCleanup() {
  const status = useAppStore((state) => state.moduleStatus.agent);
  const progress = useAppStore((state) => state.progress);
  const setModuleStatus = useAppStore((state) => state.setModuleStatus);
  const addFreedBytes = useAppStore((state) => state.addFreedBytes);
  const setProgress = useAppStore((state) => state.setProgress);
  const { t } = useTranslation();

  const [result, setResult] = useState<AiAgentScanResult | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [freedBytes, setFreedBytes] = useState(0);
  const [error, setError] = useState(false);
  const cancelledRef = useRef(false);

  const scanning = status === "scanning";
  const deleting = status === "running";

  const groups = result?.groups ?? [];
  const allItems = useMemo(
    () => groups.flatMap((g) => g.items),
    [groups],
  );

  const selectedItems = allItems.filter((item) => selected.has(item.id));
  const selectedSize = selectedItems.reduce((sum, item) => sum + item.size, 0);
  const safeItems = allItems.filter((item) => item.risk === "safe");
  const safeSize = safeItems.reduce((sum, item) => sum + item.size, 0);

  const startScanRef = useRef<() => Promise<void>>(async () => {});

  startScanRef.current = async () => {
    cancelledRef.current = false;
    setResult(null);
    setSelected(new Set());
    setFreedBytes(0);
    setError(false);
    setModuleStatus("agent", "scanning");

    try {
      const real = await scanAiAgents();
      if (cancelledRef.current) return;
      setResult(real);
      setSelected(
        new Set(
          real.groups
            .flatMap((g) => g.items)
            .filter((item) => item.risk === "safe")
            .map((item) => item.id),
        ),
      );
      setModuleStatus("agent", "ready");
    } catch (err) {
      if (cancelledRef.current) return;
      console.error("[AIAgentCleanup] scanAiAgents failed:", err);
      setProgress(null);
      setModuleStatus("agent", "error");
      setError(true);
    }
  };

  // AI data paths are cheap to walk, so scan on arrival.
  useEffect(() => {
    if (status === "idle") void startScanRef.current();
  }, [status]);

  async function deleteSelected() {
    setConfirmOpen(false);
    const paths = selectedItems.map((item) => item.path);

    setModuleStatus("agent", "running");
    try {
      const outcome = await removeAiData(paths);
      const actuallyRemoved = new Set(outcome.removedPaths);
      const freed = outcome.freedBytes;

      setResult((current) =>
        current
          ? {
              ...current,
              groups: current.groups
                .map((g) => ({
                  ...g,
                  items: g.items.filter((item) => !actuallyRemoved.has(item.path)),
                  totalSize: g.items
                    .filter((item) => !actuallyRemoved.has(item.path))
                    .reduce((sum, item) => sum + item.size, 0),
                }))
                .filter((g) => g.items.length > 0),
              totalSize: Math.max(0, current.totalSize - freed),
              safeSize: Math.max(
                0,
                current.safeSize -
                  current.groups
                    .flatMap((g) => g.items)
                    .filter((item) => actuallyRemoved.has(item.path) && item.risk === "safe")
                    .reduce((sum, item) => sum + item.size, 0),
              ),
              cautionSize: Math.max(
                0,
                current.cautionSize -
                  current.groups
                    .flatMap((g) => g.items)
                    .filter((item) => actuallyRemoved.has(item.path) && item.risk === "caution")
                    .reduce((sum, item) => sum + item.size, 0),
              ),
            }
          : current,
      );
      setSelected(new Set());
      setFreedBytes(freed);
      addFreedBytes(freed);
      setModuleStatus("agent", "done");
    } catch (err) {
      console.error("[AIAgentCleanup] removeAiData failed:", err);
      setProgress(null);
      setModuleStatus("agent", "error");
      setError(true);
    }
  }

  function handleCancel() {
    cancelledRef.current = true;
    setModuleStatus("agent", "idle");
    setProgress(null);
  }

  function toggle(id: string, checked: boolean) {
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  // Error state
  if (error && !scanning && !deleting) {
    return (
      <Card>
        <EmptyState
          icon={CircleAlert}
          title={t("common.scanFailed")}
          message={t("common.scanFailedMsg")}
          actionLabel={t("common.retry")}
          onAction={() => void startScanRef.current()}
        />
      </Card>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* -------------------------------- Stats ----------------------------- */}
      <div className="grid gap-3 sm:grid-cols-3">
        <StatTile
          icon={Bot}
          label={t("agent.toolsFound")}
          value={`${groups.length}`}
          hint={t("agent.toolsFoundHint")}
        />
        <StatTile
          icon={ShieldCheck}
          label={t("agent.cleanable")}
          value={formatBytes(safeSize)}
          hint={t("agent.cleanableHint")}
          accent="hsl(var(--success))"
        />
        <StatTile
          icon={Trash2}
          label={t("agent.selectedLabel")}
          value={formatBytes(selectedSize)}
          hint={t("agent.selectedHint", {
            selected: selected.size,
            total: allItems.length,
          })}
          accent="hsl(var(--primary))"
        />
      </div>

      {/* ------------------------------- Toolbar ---------------------------- */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          {safeItems.length > 0 && (
            <Button
              variant="ghost"
              onClick={() =>
                setSelected(new Set(safeItems.map((item) => item.id)))
              }
              disabled={scanning || deleting}
            >
              <Wand2 className="size-4" />
              {t("agent.selectSafe")}
            </Button>
          )}
        </div>

        <div className="flex items-center gap-2">
          {scanning ? (
            <Button variant="outline" onClick={handleCancel}>
              {t("common.stop")}
            </Button>
          ) : (
            <Button
              variant="outline"
              onClick={() => void startScanRef.current()}
              disabled={deleting}
            >
              <RotateCcw className="size-4" />
              {t("common.rescan")}
            </Button>
          )}

          <Button
            variant="destructive"
            disabled={selected.size === 0 || scanning || deleting}
            onClick={() => setConfirmOpen(true)}
          >
            {deleting ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <Trash2 className="size-4" />
            )}
            {deleting
              ? t("agent.deleting")
              : `${t("agent.deleteSelected")}${selectedSize > 0 ? ` · ${formatBytes(selectedSize)}` : ""}`}
          </Button>
        </div>
      </div>

      {/* ------------------------------- Progress --------------------------- */}
      {(scanning || deleting) && (
        <Card className="bg-card/70 backdrop-blur-xl">
          <CardContent className="flex flex-col gap-2.5 pt-5">
            <div className="flex items-center justify-between gap-4">
              <span className="flex items-center gap-2.5 text-[13px] font-medium">
                <Loader2 className="size-4 animate-spin text-primary" />
                {scanning ? t("agent.scanning") : t("agent.deleting")}
              </span>
              <span className="text-[13px] font-semibold tabular-nums">
                {formatBytes(progress?.bytesFound ?? 0)}
              </span>
            </div>
            <Progress
              value={progress?.progress != null ? progress.progress * 100 : null}
            />
            <p className="truncate font-mono text-[11px] text-muted-foreground">
              {progress?.currentPath ?? ""}
            </p>
          </CardContent>
        </Card>
      )}

      {/* -------------------------------- Result ---------------------------- */}
      {status === "done" && freedBytes > 0 && (
        <Card className="border-success/30 bg-success/8">
          <CardContent className="flex items-center gap-3.5 pt-5">
            <span className="flex size-10 items-center justify-center rounded-xl bg-success/15 text-success">
              <Sparkles className="size-5" strokeWidth={1.9} />
            </span>
            <div>
              <p className="text-[15px] font-semibold tracking-tight">
                {t("agent.freedResult", { bytes: formatBytes(freedBytes) })}
              </p>
              <p className="text-[12.5px] text-muted-foreground">
                {t("agent.freedDetail")}
              </p>
            </div>
          </CardContent>
        </Card>
      )}

      {/* --------------------------------- List ----------------------------- */}
      {groups.length > 0 ? (
        <div className="flex flex-col gap-3">
          {groups.map((group) => (
            <Card key={group.tool} className="overflow-hidden">
              <div className="flex items-center justify-between gap-4 border-b border-border px-4 py-2.5">
                <div className="flex items-center gap-2.5">
                  <span className="flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
                    <Database className="size-4" strokeWidth={1.9} />
                  </span>
                  <span className="text-[13px] font-semibold">
                    {TOOL_LABELS[group.tool]}
                  </span>
                  <Badge tone="neutral">
                    {group.items.length} {t("common.files")}
                  </Badge>
                </div>
                <span className="text-[12.5px] text-muted-foreground tabular-nums">
                  {formatBytes(group.totalSize)}
                </span>
              </div>

              <div className="flex flex-col">
                {group.items.map((item) => (
                  <AgentRow
                    key={item.id}
                    item={item}
                    checked={selected.has(item.id)}
                    disabled={deleting}
                    onToggle={(checked) => toggle(item.id, checked)}
                  />
                ))}
              </div>
            </Card>
          ))}
        </div>
      ) : (
        !scanning && (
          <Card>
            <EmptyState
              icon={Bot}
              title={t("agent.noDataLeft")}
              message={t("agent.noDataLeftMsg")}
              actionLabel={t("common.rescan")}
              onAction={() => void startScanRef.current()}
            />
          </Card>
        )
      )}

      <ConfirmDialog
        open={confirmOpen}
        destructive
        title={t("agent.confirmDelete", { count: selectedItems.length })}
        message={t("agent.confirmDeleteMsg", { bytes: formatBytes(selectedSize) })}
        confirmLabel={t("agent.deleteNow")}
        onConfirm={deleteSelected}
        onClose={() => setConfirmOpen(false)}
      >
        <div className="flex max-h-44 flex-col gap-1.5 overflow-y-auto rounded-lg bg-muted/60 p-3 text-[12px]">
          {selectedItems.map((item) => (
            <div
              key={item.id}
              className="flex items-center justify-between gap-3"
            >
              <span className="truncate font-mono" data-selectable>
                {item.path}
              </span>
              <span className="shrink-0 text-muted-foreground tabular-nums">
                {formatBytes(item.size)}
              </span>
            </div>
          ))}
        </div>
      </ConfirmDialog>
    </div>
  );
}

interface AgentRowProps {
  item: AiDataItem;
  checked: boolean;
  disabled: boolean;
  onToggle: (checked: boolean) => void;
}

function AgentRow({ item, checked, disabled, onToggle }: AgentRowProps) {
  const { t } = useTranslation();

  return (
    <label
      className={cn(
        "flex cursor-pointer items-center gap-3.5 border-b border-border/60 px-4 py-2.5 transition-colors duration-150 last:border-0",
        checked ? "bg-primary/[0.04]" : "hover:bg-accent/40",
      )}
    >
      <Checkbox
        checked={checked}
        onChange={onToggle}
        disabled={disabled}
        label={item.path}
      />

      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2">
          <span className="truncate text-[13px] font-mono">{item.path}</span>
        </span>
        <span className="mt-0.5 flex items-center gap-2 text-[11.5px] text-muted-foreground">
          <span className="uppercase">{item.dataType}</span>
          <span>·</span>
          <span>{item.description}</span>
        </span>
      </span>

      <Badge
        tone={item.risk === "safe" ? "success" : "warning"}
        className="shrink-0"
      >
        {item.risk === "safe" ? t("agent.cleanable") : item.risk}
      </Badge>

      <span className="shrink-0 text-right text-[13.5px] font-semibold tabular-nums">
        {formatBytes(item.size)}
      </span>
    </label>
  );
}
