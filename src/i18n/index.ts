import {
  createElement,
  createContext,
  useCallback,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { useAppStore } from "@/stores/appStore";
import { translations, type Lang, type TranslationKey } from "./translations";

const STORAGE_KEY = "windle-lang";

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

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(getInitialLang);
  const platform = useAppStore((state) => state.platform);

  const setLang = useCallback((next: Lang) => {
    setLangState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Ignore write errors.
    }
  }, []);

  const toggleLang = useCallback(() => {
    setLangState((prev) => {
      const next: Lang = prev === "zh-CN" ? "en-US" : "zh-CN";
      try {
        localStorage.setItem(STORAGE_KEY, next);
      } catch {
        // Ignore write errors.
      }
      return next;
    });
  }, []);

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
