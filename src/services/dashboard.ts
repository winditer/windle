import { call } from "./ipc";
import type { DashboardSummary, PermissionState } from "@/types";

/** Everything the dashboard needs, gathered in one round-trip. */
export function getDashboardSummary(): Promise<DashboardSummary> {
  return call<DashboardSummary>("get_dashboard_summary");
}

/** Check whether we hold Full Disk Access and an admin session. */
export function checkPermissions(): Promise<PermissionState> {
  return call<PermissionState>("check_permissions");
}

/** Open System Settings on the Full Disk Access pane. */
export function openFullDiskAccessSettings(): Promise<void> {
  return call<void>("open_full_disk_access_settings");
}
