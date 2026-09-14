import type { TranslationKey } from "@/i18n/translations";
import type { CleanCategoryId } from "@/types";

/** Translation function shape returned by `useTranslation()`. */
export type TranslateFn = (
  key: TranslationKey,
  params?: Record<string, string | number>,
) => string;

/** Translation keys for localized category names — exhaustive over ids. */
export const CATEGORY_LABEL_KEYS: Record<CleanCategoryId, TranslationKey> = {
  "system-cache": "deepClean.category.system-cache",
  "user-cache": "deepClean.category.user-cache",
  "app-junk": "deepClean.category.app-junk",
  "browser-cache": "deepClean.category.browser-cache",
  "app-logs": "deepClean.category.app-logs",
  "xcode-derived-data": "deepClean.category.xcode-derived-data",
  "ios-backups": "deepClean.category.ios-backups",
  "language-files": "deepClean.category.language-files",
  "mail-attachments": "deepClean.category.mail-attachments",
  downloads: "deepClean.category.downloads",
  trash: "deepClean.category.trash",
  "broken-symlinks": "deepClean.category.broken-symlinks",
};

/**
 * `t()` returns the key itself when a translation is missing — fall back to
 * the backend-provided English text in that case.
 */
export function localizedText(
  t: TranslateFn,
  key: TranslationKey,
  fallback: string,
): string {
  const localized = t(key);
  return localized === key ? fallback : localized;
}

/**
 * Scan-progress labels are kebab-case category ids — map them to localized
 * names. Anything else (e.g. a real file path pushed by other modules such
 * as docker/purge/analyze) is shown as-is.
 */
export function localizedScanStage(
  t: TranslateFn,
  currentPath: string | undefined,
): string {
  if (!currentPath) return "";
  const key = CATEGORY_LABEL_KEYS[currentPath as CleanCategoryId];
  return key ? localizedText(t, key, currentPath) : currentPath;
}
