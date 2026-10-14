import { call, subscribe } from "./ipc";
import type {
  AutomationAction,
  AutomationConfig,
  AutomationLogEntry,
  AutomationStatus,
} from "@/types";

/** Event name emitted by the Rust side when a run finishes. */
export const AUTOMATION_LOG_EVENT = "automation://log";

/** Current settings, next scheduled run and webhook state. */
export function getStatus(): Promise<AutomationStatus> {
  return call<AutomationStatus>("automation_status");
}

/** Save settings; returns the applied state (including a fresh `nextRunAt`). */
export function setConfig(config: AutomationConfig): Promise<AutomationStatus> {
  return call<AutomationStatus>("automation_set_config", { config });
}

/** Run log, newest first. */
export function getLogs(limit?: number): Promise<AutomationLogEntry[]> {
  return call<AutomationLogEntry[]>("automation_logs", { limit: limit ?? null });
}

/** Forget every log entry. */
export function clearLogs(): Promise<void> {
  return call<void>("automation_clear_logs");
}

/** Start one of the actions right now, on top of the automatic rules. */
export function runNow(action: AutomationAction): Promise<void> {
  return call<void>("automation_run_now", { action });
}

/** A fresh webhook token; it only becomes real once the settings are saved. */
export function generateToken(): Promise<string> {
  return call<string>("automation_generate_token");
}

export function onAutomationLog(
  handler: (event: AutomationLogEntry) => void,
) {
  return subscribe<AutomationLogEntry>(AUTOMATION_LOG_EVENT, handler);
}
