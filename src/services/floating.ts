import { call, subscribe } from "./ipc";
import type { WidgetPlacement, WidgetSide, WidgetSnapshot } from "@/types";

/** Emitted whenever the widget is shown or hidden, so every surface agrees. */
export const VISIBILITY_EVENT = "floating://visibility";

/** Emitted when the pointer arrives over the widget and when it leaves. */
export const HOVER_EVENT = "floating://hover";

/** The floating widget's own reading; no process table, so it is cheap. */
export function getWidgetSnapshot(): Promise<WidgetSnapshot> {
  return call<WidgetSnapshot>("get_widget_snapshot");
}

export function showFloatingWindow(): Promise<void> {
  return call<void>("show_floating_window");
}

export function hideFloatingWindow(): Promise<void> {
  return call<void>("hide_floating_window");
}

/** Flip the widget and resolve with its new visibility. */
export function toggleFloatingWindow(): Promise<boolean> {
  return call<boolean>("toggle_floating_window");
}

export function isFloatingWindowVisible(): Promise<boolean> {
  return call<boolean>("floating_window_visible");
}

/**
 * Save where the ball was left. Called when a drag ends: the backend's own
 * window-move stream is throttled, so a quick drag would otherwise keep only
 * its first frames and the next launch would park the ball at the start of it.
 */
export function rememberFloatingPosition(): Promise<void> {
  return call<void>("remember_floating_position");
}

/**
 * Grow the window for the hover panel, or fold it back onto the ball. The
 * layout has to be applied (ball anchored to the corner that stays put) before
 * this is called, so the ball does not jump while the window resizes.
 */
export function setFloatingExpanded(
  expanded: boolean,
  placement: WidgetPlacement,
  side: WidgetSide,
): Promise<void> {
  return call<void>("set_floating_expanded", { expanded, placement, side });
}

export function onFloatingVisibility(
  handler: (visible: boolean) => void,
): Promise<() => void> {
  return subscribe<boolean>(VISIBILITY_EVENT, handler);
}

/**
 * The pointer over the widget, from the backend rather than from the DOM.
 *
 * Only the active app is given mouse tracking by macOS, so a widget in the
 * background never sees `pointerenter`; the backend watches the pointer itself
 * and reports the comings and goings here.
 */
export function onFloatingHover(handler: (hovered: boolean) => void): Promise<() => void> {
  return subscribe<boolean>(HOVER_EVENT, handler);
}
