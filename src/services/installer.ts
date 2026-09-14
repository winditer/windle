import { call } from "./ipc";
import type { CleanOutcome, InstallerScanResult } from "@/types";

/** Find .dmg/.pkg/.zip installers in Downloads and other common folders. */
export function scanInstallers(roots?: string[]): Promise<InstallerScanResult> {
  return call<InstallerScanResult>("scan_installers", { roots: roots ?? null });
}

/** Remove the selected installers, trashing them by default. */
export function removeInstallers(
  paths: string[],
  permanent = false,
): Promise<CleanOutcome> {
  return call<CleanOutcome>("remove_installers", { paths, permanent });
}

/** Detach any mounted volume that came from one of these disk images. */
export function detachMountedImages(): Promise<string[]> {
  return call<string[]>("detach_mounted_images");
}
