import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** Merge conditional class names, de-duplicating conflicting Tailwind utilities. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** Await a fixed delay — used to pace the mock task runners. */
export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

/** Format a byte count the way Finder does (base-10 units). */
export function formatBytes(bytes: number, decimals = 1): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";

  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  const exponent = Math.min(
    Math.floor(Math.log10(bytes) / 3),
    units.length - 1,
  );
  const value = bytes / 1000 ** exponent;

  return `${value.toFixed(exponent === 0 ? 0 : decimals)} ${units[exponent]}`;
}

/** Format a fraction (0–1) as a rounded percentage string. */
export function formatPercent(ratio: number, decimals = 0): string {
  if (!Number.isFinite(ratio)) return "0%";
  return `${(ratio * 100).toFixed(decimals)}%`;
}

/** Compact duration for uptime and elapsed times: "4d 7h 22m". */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 60) return "less than a minute";

  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);

  return [days && `${days}d`, hours && `${hours}h`, minutes && `${minutes}m`]
    .filter(Boolean)
    .join(" ");
}

/** Human readable "3 days ago" style timestamps. */
export function formatRelativeTime(
  timestamp: number | null,
  locale = "en-US",
): string {
  if (!timestamp) return "Never";

  const seconds = Math.round((Date.now() - timestamp) / 1000);
  if (seconds < 45) return "Just now";

  const steps: [Intl.RelativeTimeFormatUnit, number][] = [
    ["second", 60],
    ["minute", 60],
    ["hour", 24],
    ["day", 7],
    ["week", 4.348],
    ["month", 12],
    ["year", Number.POSITIVE_INFINITY],
  ];

  // Defaults to en-US so existing callers keep their previous output; pass
  // the active UI language to localize the relative strings.
  const formatter = new Intl.RelativeTimeFormat(locale, {
    numeric: "auto",
  });

  let value = -seconds;
  for (const [unit, size] of steps) {
    if (Math.abs(value) < size) return formatter.format(Math.round(value), unit);
    value /= size;
  }

  return formatter.format(Math.round(value), "year");
}

/** Shorten a long path for display: /Users/me/…/Caches/foo. */
export function truncatePath(path: string, keep = 3): string {
  const segments = path.split("/").filter(Boolean);
  if (segments.length <= keep + 1) return path;

  return `/${segments[0]}/…/${segments.slice(-keep).join("/")}`;
}
