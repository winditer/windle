import { call } from "./ipc";
import type { ProcessInfo } from "@/types";

/** Quit the status bar app entirely (stops the menu bar helper process). */
export function quitStatusBar(): Promise<void> {
  return call<void>("quit_status_bar");
}

/** Reveal and focus the main Windle window. */
export function showMainWindow(): Promise<void> {
  return call<void>("show_main_window");
}

/** Top N processes sorted by memory footprint (descending). */
export function listTopMemoryProcesses(limit: number): Promise<ProcessInfo[]> {
  return call<ProcessInfo[]>("list_top_memory_processes", { limit });
}
