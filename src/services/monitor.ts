import { call, subscribe } from "./ipc";
import type { ProcessInfo, SystemSnapshot } from "@/types";

export const SNAPSHOT_EVENT = "monitor://snapshot";

/** One-shot reading of every metric. */
export function getSnapshot(): Promise<SystemSnapshot> {
  return call<SystemSnapshot>("get_snapshot");
}

/** Start pushing snapshots on `SNAPSHOT_EVENT` every `intervalMs`. */
export function startMonitor(intervalMs = 1000): Promise<void> {
  return call<void>("start_monitor", { intervalMs });
}

export function stopMonitor(): Promise<void> {
  return call<void>("stop_monitor");
}

/** Processes sorted by CPU, then memory. */
export function listProcesses(limit = 100): Promise<ProcessInfo[]> {
  return call<ProcessInfo[]>("list_processes", { limit });
}

/** Send SIGTERM (or SIGKILL when `force`) to a process. */
export function killProcess(pid: number, force = false): Promise<void> {
  return call<void>("kill_process", { pid, force });
}

export function onSnapshot(handler: (snapshot: SystemSnapshot) => void) {
  return subscribe<SystemSnapshot>(SNAPSHOT_EVENT, handler);
}
