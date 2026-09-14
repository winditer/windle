import { useContext } from "react";
import { LanguageContext } from "@/i18n";
import type { Lang, TranslationKey } from "@/i18n/translations";

export function useTranslation() {
  const { lang, t, setLang, toggleLang } = useContext(LanguageContext);
  return { lang, t, setLang, toggleLang } as {
    lang: Lang;
    t: (key: TranslationKey, params?: Record<string, string | number>) => string;
    setLang: (lang: Lang) => void;
    toggleLang: () => void;
  };
}
