import {
  createElement,
  createContext,
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { emit } from "@tauri-apps/api/event";
import { useAppStore } from "@/stores/appStore";
import { subscribe } from "@/services/ipc";
import { translations, type Lang, type TranslationKey } from "./translations";

const STORAGE_KEY = "windle-lang";

/**
 * Broadcast whenever the language changes. The widget and the status bar are
 * windows of their own, each holding its own copy of this state; without this
 * they would keep the language they loaded with.
 */
const LANG_EVENT = "windle://lang";

export interface LanguageContextValue {
  lang: Lang;
  t: (key: TranslationKey, params?: Record<string, string | number>) => string;
  setLang: (lang: Lang) => void;
  toggleLang: () => void;
}

export const LanguageContext = createContext<LanguageContextValue>({
  lang: "zh-CN",
  t: (key) => key,
  setLang: () => {},
  toggleLang: () => {},
});

function getInitialLang(): Lang {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === "zh-CN" || stored === "en-US") return stored;
  } catch {
    // localStorage not available — fall through to default.
  }
  return "zh-CN";
}

function isLang(value: unknown): value is Lang {
  return value === "zh-CN" || value === "en-US";
}

/** Remember the choice and tell the other windows about it. */
function applyLang(next: Lang) {
  try {
    localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // Ignore write errors.
  }
  void emit(LANG_EVENT, next).catch(() => {});
}

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(getInitialLang);
  const platform = useAppStore((state) => state.platform);

  // Another window changed the language: follow it. The event carries the
  // choice itself, so no window has to guess which one is authoritative.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    subscribe<Lang>(LANG_EVENT, (next) => {
      if (isLang(next)) setLangState(next);
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  const setLang = useCallback((next: Lang) => {
    setLangState(next);
    applyLang(next);
  }, []);

  const toggleLang = useCallback(() => {
    setLang(lang === "zh-CN" ? "en-US" : "zh-CN");
  }, [lang, setLang]);

  const t = useCallback(
    (key: TranslationKey, params?: Record<string, string | number>) => {
      const dict = translations[lang] as Record<string, string>;
      // Platform overrides: a `${key}.windows` entry replaces the base string
      // on Windows ("Recycle Bin" for Trash, "managed by Windows", …). Keys
      // without an override fall back to the base translation.
      const override = platform === "windows" ? dict[`${key}.windows`] : undefined;
      let str: string = override ?? dict[key] ?? key;
      if (params) {
        for (const [name, value] of Object.entries(params)) {
          str = str.replace(`{${name}}`, String(value));
        }
      }
      return str;
    },
    [lang, platform],
  );

  const value = useMemo<LanguageContextValue>(
    () => ({ lang, t, setLang, toggleLang }),
    [lang, t, setLang, toggleLang],
  );

  return createElement(
    LanguageContext.Provider,
    { value },
    children,
  );
}
