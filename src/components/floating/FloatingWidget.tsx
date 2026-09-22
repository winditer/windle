import { useCallback, useEffect, useRef, useState } from "react";
import { PhysicalPosition } from "@tauri-apps/api/dpi";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { currentMonitor } from "@tauri-apps/api/window";
import { useTranslation } from "@/hooks/useTranslation";
import {
  getWidgetSnapshot,
  hideFloatingWindow,
  isFloatingWindowVisible,
  onFloatingHover,
  onFloatingVisibility,
  rememberFloatingPosition,
  setFloatingExpanded,
} from "@/services/floating";
import { showMainWindow } from "@/services/menubar";
import { runOptimizeTask } from "@/services/optimize";
import type { WidgetPlacement, WidgetSide, WidgetSnapshot } from "@/types";
import { cn, formatBytes } from "@/lib/utils";
import { MEMORY_ALERT_COLOR, MEMORY_ALERT_PERCENT, WaterBall } from "./WaterBall";

/** Matches `floating::BALL_SIZE` / the ball box below. */
const BALL_SIZE = 33;
/** Matches `floating::PANEL_WIDTH` / `floating::PANEL_HEIGHT` and the CSS. */
const PANEL_WIDTH = 132;
const PANEL_HEIGHT = 132;

const POLL_MS = 2000;

/** Pointer travel that turns a press into a drag rather than a click. */
const DRAG_THRESHOLD = 4;

/** How long the pointer may be away before the panel folds back. */
const COLLAPSE_DELAY_MS = 140;

/** How long a lone click waits for a second one. One click frees memory, two
    open the main window, and which it is is only known once the wait is over. */
const DOUBLE_CLICK_MS = 260;

/** The swirl runs at least this long, so a quick release still shows it. It
 * matches the animations' own length: the water has settled by the time the
 * class comes off. */
const SWIRL_MIN_MS = 1100;

interface DragState {
  pointerX: number;
  pointerY: number;
  originX: number;
  originY: number;
  /** CSS pixels to physical pixels. */
  scale: number;
  /** The starting position has arrived; until then the ball cannot move. */
  ready: boolean;
  moved: boolean;
}

type ReleaseStatus = "idle" | "running" | "done" | "failed";

/** One reading in the hover panel. */
function Metric({
  label,
  value,
  hint,
  alert = false,
}: {
  label: string;
  value: string;
  hint?: string | null;
  /** Drawn in the warning colour, the same as the ball's own reading. */
  alert?: boolean;
}) {
  return (
    <div className="flex items-baseline justify-between gap-1">
      <span className="whitespace-nowrap text-[10px] leading-none text-white/60">
        {label}
      </span>
      <span className="flex items-baseline gap-[3px]">
        <span
          className="whitespace-nowrap text-[11px] font-semibold leading-none tabular-nums transition-colors duration-500"
          style={{ color: alert ? MEMORY_ALERT_COLOR : "#ffffff" }}
        >
          {value}
        </span>
        {hint && (
          <span className="whitespace-nowrap text-[9px] leading-none text-white/55 tabular-nums">
            {hint}
          </span>
        )}
      </span>
    </div>
  );
}

/**
 * Network traffic reads as two figures rather than one. Side by side they are
 * wider than the panel, so they stack in the value column instead.
 */
function TrafficMetric({
  label,
  rx,
  tx,
}: {
  label: string;
  rx: number;
  tx: number;
}) {
  return (
    <div className="flex items-center justify-between gap-1">
      <span className="whitespace-nowrap text-[10px] leading-none text-white/60">
        {label}
      </span>
      <span className="flex flex-col items-end gap-[3px] text-[10px] font-semibold leading-none text-white tabular-nums">
        <span className="whitespace-nowrap">↓ {formatBytes(rx)}</span>
        <span className="whitespace-nowrap">↑ {formatBytes(tx)}</span>
      </span>
    </div>
  );
}

/**
 * The desktop widget: a small semi-transparent ball that fills with the
 * machine's memory usage. Hovering opens a panel with the readings, clicking
 * frees memory, double-clicking opens the main window, dragging moves it,
 * right-clicking hides it.
 *
 * The window is only ever as large as what it draws, so opening the panel
 * resizes it — vertically, and sideways too, since the panel is wider than the
 * ball. The ball is anchored to the corner that does not move, and the resize
 * is asked for only after that layout is in place, which is what keeps the ball
 * from jumping.
 */
export function FloatingWidget() {
  const { t } = useTranslation();

  const [visible, setVisible] = useState(false);
  const [snapshot, setSnapshot] = useState<WidgetSnapshot | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [placement, setPlacement] = useState<WidgetPlacement>("above");
  const [side, setSide] = useState<WidgetSide>("right");
  const [status, setStatus] = useState<ReleaseStatus>("idle");
  const [freedBytes, setFreedBytes] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [swirling, setSwirling] = useState(false);

  const drag = useRef<DragState | null>(null);
  const collapseTimer = useRef<number | null>(null);
  const clickTimer = useRef<number | null>(null);
  const placementRef = useRef<{ placement: WidgetPlacement; side: WidgetSide }>({
    placement: "above",
    side: "right",
  });
  const busyRef = useRef(false);
  const insideRef = useRef(false);

  // The backend has already decided whether this window is on screen — it has
  // to, since a hidden window does not load its webview — so all this has to
  // learn is which way that went.
  useEffect(() => {
    let cancelled = false;
    isFloatingWindowVisible()
      .then((shown) => {
        if (!cancelled) setVisible(shown);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // Shown or hidden from the tray or the sidebar.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    onFloatingVisibility((shown) => {
      setVisible(shown);
      // Rust folds the window back when it hides, so the frontend follows.
      if (!shown) setExpanded(false);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // Poll while the ball is on screen; a hidden window costs nothing.
  useEffect(() => {
    if (!visible) return;

    let cancelled = false;
    const read = () => {
      getWidgetSnapshot()
        .then((next) => {
          if (!cancelled) setSnapshot(next);
        })
        .catch(() => {});
    };

    read();
    const id = window.setInterval(read, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [visible]);

  // Clear the release message after it has been read.
  useEffect(() => {
    if (status !== "done" && status !== "failed") return;
    const id = window.setTimeout(() => setStatus("idle"), 4000);
    return () => window.clearTimeout(id);
  }, [status]);

  useEffect(
    () => () => {
      if (collapseTimer.current) window.clearTimeout(collapseTimer.current);
      if (clickTimer.current) window.clearTimeout(clickTimer.current);
    },
    [],
  );

  /** Drops a click that is still waiting to find out whether it is a lone one. */
  const cancelPendingClick = useCallback(() => {
    if (clickTimer.current) {
      window.clearTimeout(clickTimer.current);
      clickTimer.current = null;
    }
  }, []);

  /** Which way the panel has room to open. */
  const pickPlacement = useCallback(async (): Promise<{
    placement: WidgetPlacement;
    side: WidgetSide;
  }> => {
    try {
      const win = getCurrentWebviewWindow();
      const [position, monitor] = await Promise.all([
        win.outerPosition(),
        currentMonitor(),
      ]);
      if (!monitor) return { placement: "above", side: "right" };

      const scale = monitor.scaleFactor;
      const roomAbove = position.y - monitor.position.y;
      const roomLeft = position.x - monitor.position.x;

      return {
        // Upward unless the ball is against the top of the screen.
        placement: roomAbove >= PANEL_HEIGHT * scale ? "above" : "below",
        // The panel is wider than the ball, so it reaches toward the middle of
        // the screen and the ball keeps the edge nearest the screen's edge.
        side: roomLeft >= (PANEL_WIDTH - BALL_SIZE) * scale ? "right" : "left",
      };
    } catch {
      return { placement: "above", side: "right" };
    }
  }, []);

  const collapsePanel = useCallback(() => {
    setExpanded(false);
    const where = placementRef.current;
    setFloatingExpanded(false, where.placement, where.side).catch(() => {});
  }, []);

  const handleEnter = useCallback(() => {
    insideRef.current = true;
    if (collapseTimer.current) {
      window.clearTimeout(collapseTimer.current);
      collapseTimer.current = null;
    }
    if (drag.current || busyRef.current) return;

    void pickPlacement().then(async (choice) => {
      if (!insideRef.current || drag.current) return;
      placementRef.current = choice;
      setPlacement(choice.placement);
      setSide(choice.side);

      // The window has to grow before the panel is drawn: the ball sits in the
      // corner that does not move, so growing first leaves it exactly where it
      // was, while a panel drawn into the old cramped window would land on top
      // of it for a frame.
      await setFloatingExpanded(true, choice.placement, choice.side).catch(() => {});
      if (!insideRef.current || drag.current) return;
      setExpanded(true);
    });
  }, [pickPlacement]);

  const handleLeave = useCallback(() => {
    insideRef.current = false;
    if (drag.current) return;
    if (collapseTimer.current) window.clearTimeout(collapseTimer.current);
    collapseTimer.current = window.setTimeout(() => {
      collapseTimer.current = null;
      if (!insideRef.current && !drag.current) collapsePanel();
    }, COLLAPSE_DELAY_MS);
  }, [collapsePanel]);

  // The pointer over the widget, as the backend sees it. macOS hands mouse
  // tracking to the active app alone, so the DOM's own enter and leave go quiet
  // the moment the user turns to another app — and the panel has to keep
  // working then. Both sources are idempotent, so hearing it twice is fine.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    onFloatingHover((hovered) => (hovered ? handleEnter() : handleLeave())).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [handleEnter, handleLeave]);

  const releaseMemory = useCallback(async () => {
    if (busyRef.current) return;

    busyRef.current = true;
    setStatus("running");
    setSwirling(true);
    const startedAt = Date.now();

    try {
      // The purge reports how much it freed itself, but the widget's own
      // readings are what the ball draws, so it measures the same way.
      const before = await getWidgetSnapshot();
      const outcome = await runOptimizeTask("purge-memory");
      const after = await getWidgetSnapshot();
      setSnapshot(after);

      if (!outcome.succeeded) {
        setStatus("failed");
        return;
      }

      setFreedBytes(Math.max(0, before.memoryUsedBytes - after.memoryUsedBytes));
      setStatus("done");
    } catch {
      setStatus("failed");
    } finally {
      // The water keeps turning until the swirl has been seen through, however
      // quick the purge itself was.
      const remaining = SWIRL_MIN_MS - (Date.now() - startedAt);
      if (remaining > 0) {
        await new Promise((resolve) => window.setTimeout(resolve, remaining));
      }
      setSwirling(false);
      busyRef.current = false;
    }
  }, []);

  const handlePointerDown = useCallback((event: React.PointerEvent) => {
    if (event.button !== 0) return;

    // The window travels with the pointer, but a fast drag outruns it; capture
    // keeps the moves and the release coming back to this element either way.
    event.currentTarget.setPointerCapture(event.pointerId);

    // Recorded before the position is known: a click can be over before the
    // round trip finishes, and it still has to count as a click.
    const state: DragState = {
      pointerX: event.screenX,
      pointerY: event.screenY,
      originX: 0,
      originY: 0,
      scale: 1,
      ready: false,
      moved: false,
    };
    drag.current = state;

    const win = getCurrentWebviewWindow();
    void Promise.all([win.outerPosition(), win.scaleFactor()])
      .then(([origin, scale]) => {
        state.originX = origin.x;
        state.originY = origin.y;
        state.scale = scale;
        state.ready = true;
      })
      .catch(() => {
        // Without a starting position the ball cannot move, but the press can
        // still end as a click.
      });
  }, []);

  const handlePointerMove = useCallback(
    async (event: React.PointerEvent) => {
      const state = drag.current;
      if (!state) return;

      const travel = Math.hypot(
        event.screenX - state.pointerX,
        event.screenY - state.pointerY,
      );
      if (!state.moved && travel < DRAG_THRESHOLD) return;
      if (!state.ready) return;

      if (!state.moved) {
        state.moved = true;
        setDragging(true);
        // A press that turned into a drag is not the click it may have looked
        // like a moment ago.
        cancelPendingClick();

        // A drag has to start from the collapsed ball: the window's origin is
        // its top-left corner only then, which is what the pointer offsets are
        // measured against.
        if (expanded) {
          collapsePanel();
          const win = getCurrentWebviewWindow();
          try {
            const origin = await win.outerPosition();
            state.originX = origin.x;
            state.originY = origin.y;
            state.pointerX = event.screenX;
            state.pointerY = event.screenY;
          } catch {
            // Keep the stale origin; the drag still ends up close.
          }
        }
      }

      const x = state.originX + (event.screenX - state.pointerX) * state.scale;
      const y = state.originY + (event.screenY - state.pointerY) * state.scale;

      getCurrentWebviewWindow()
        .setPosition(new PhysicalPosition(Math.round(x), Math.round(y)))
        .catch(() => {});
    },
    [cancelPendingClick, collapsePanel, expanded],
  );

  /** A press that never travelled is a click: one frees memory, two open the
      main window. Which of the two it was is only known once the second click
      has either arrived or not, so the release waits the difference out. */
  const endDrag = useCallback(
    (released: boolean) => {
      const state = drag.current;
      drag.current = null;
      setDragging(false);

      // A drop is saved from here rather than left to the backend's own
      // window-move stream: that one is throttled, so the last frames of a
      // quick drag would be lost and the ball would come back to where the
      // drag started.
      if (state?.moved) {
        rememberFloatingPosition().catch(() => {});
      }

      if (!released || !state || state.moved) return;

      cancelPendingClick();
      clickTimer.current = window.setTimeout(() => {
        clickTimer.current = null;
        void releaseMemory();
      }, DOUBLE_CLICK_MS);
    },
    [cancelPendingClick, releaseMemory],
  );

  const handleDoubleClick = useCallback(() => {
    cancelPendingClick();
    showMainWindow().catch(() => {});
  }, [cancelPendingClick]);

  const handleContextMenu = useCallback(
    (event: React.MouseEvent) => {
      event.preventDefault();
      cancelPendingClick();
      setExpanded(false);
      hideFloatingWindow().catch(() => {});
    },
    [cancelPendingClick],
  );

  const memory = snapshot;
  const level =
    memory && memory.memoryTotalBytes > 0
      ? memory.memoryUsedBytes / memory.memoryTotalBytes
      : 0;
  // Rounded once and read from here, so the ball's reading, the panel's and the
  // colour they turn are all the same number.
  const memoryPercent = Math.round(level * 100);

  // A release takes the memory row over for a few seconds, since that is what
  // the click was about.
  const releaseText =
    status === "running"
      ? t("floating.releasing")
      : status === "done"
        ? t("floating.freed", { bytes: formatBytes(freedBytes) })
        : status === "failed"
          ? t("floating.releaseFailed")
          : null;

  const diskPercent =
    memory && memory.diskTotalBytes > 0
      ? Math.round((memory.diskUsedBytes / memory.diskTotalBytes) * 100)
      : null;

  return (
    <div
      className="relative h-screen w-screen overflow-hidden select-none"
      onPointerEnter={handleEnter}
      onPointerLeave={handleLeave}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={() => endDrag(true)}
      onPointerCancel={() => endDrag(false)}
      onDoubleClick={handleDoubleClick}
      onContextMenu={handleContextMenu}
    >
      {expanded && (
        <div
          className={cn(
            "absolute inset-x-0 h-[132px] px-[3px] py-[5px]",
            placement === "above" ? "top-0" : "bottom-0",
          )}
        >
          <div
            className={cn(
              "flex h-[122px] flex-col justify-center gap-[5px] rounded-2xl px-2",
              "border border-white/12 bg-[rgb(8_20_14/0.72)] backdrop-blur-md",
              "shadow-[0_6px_20px_rgb(0_0_0/0.25)]",
            )}
          >
            {releaseText ? (
              // Alone on its row: the message is longer than a label plus value
              // would allow, and it says which reading it belongs to anyway.
              <p
                className={cn(
                  "whitespace-nowrap text-[10px] font-semibold leading-none",
                  status === "failed" ? "text-red-300" : "text-white",
                  status === "running" && "animate-pulse",
                )}
              >
                {releaseText}
              </p>
            ) : (
              // The share is what the ball draws; the absolute figure is in the
              // dashboard, and the panel has no room for both.
              <Metric
                label={t("floating.memory")}
                value={`${memoryPercent}%`}
                alert={memoryPercent >= MEMORY_ALERT_PERCENT}
              />
            )}
            <Metric
              label={t("floating.cpu")}
              value={`${Math.round((memory?.cpuUsage ?? 0) * 100)}%`}
              hint={
                memory?.temperatureC != null
                  ? `${Math.round(memory.temperatureC)}°C`
                  : null
              }
            />
            <Metric
              label={t("floating.fan")}
              value={
                memory?.fanSpeedRpm != null
                  ? `${Math.round(memory.fanSpeedRpm)}`
                  : "—"
              }
              hint={memory?.fanSpeedRpm != null ? "RPM" : null}
            />
            <Metric
              label={t("floating.disk")}
              value={diskPercent != null ? `${diskPercent}%` : "—"}
            />
            <TrafficMetric
              label={t("floating.network")}
              rx={memory?.networkRxBytesPerSec ?? 0}
              tx={memory?.networkTxBytesPerSec ?? 0}
            />
            {/* The ball is drawn over the card's corner, so the hint keeps clear
                of it: at this width its longest line still ends in the open. */}
            <p className="pr-[24px] text-[8.5px] leading-[1.3] text-white/40">
              {t("floating.tip")}
            </p>
          </div>
        </div>
      )}

      <div
        className={cn(
          "absolute cursor-grab active:cursor-grabbing",
          placement === "above" ? "bottom-0" : "top-0",
          side === "left" ? "left-0" : "right-0",
        )}
        style={{ width: BALL_SIZE, height: BALL_SIZE }}
      >
        <WaterBall level={level} dragging={dragging} swirling={swirling} />
      </div>
    </div>
  );
}
