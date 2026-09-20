import { call } from "./ipc";
import type { Platform } from "@/types";

/**
 * Which OS the backend is running on. Anything unexpected falls back to
 * "macos", the platform the interface was originally written for.
 */
export async function getPlatform(): Promise<Platform> {
  const os = await call<string>("platform_info");
  if (os === "windows") return "windows";
  if (os === "linux") return "linux";
  return "macos";
}
