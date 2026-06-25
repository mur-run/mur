import { createContext, useContext, useState, useEffect, type ReactNode } from "react";
import { en } from "./en";
import { zhTW } from "./zh-TW";
import type { Lang, TranslationKey, Table } from "./types";

const TABLES: Record<string, Table> = { en, "zh-TW": zhTW };
const STORAGE_KEY = "mur.hub.lang";

export function translate(
  lang: string,
  key: TranslationKey,
  vars?: Record<string, string | number>,
): string {
  const table = TABLES[lang] ?? en;
  let s = (table[key] ?? en[key]) as string;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      s = s.replace(new RegExp(`\\{${k}\\}`, "g"), String(v));
    }
  }
  return s;
}

function detectDefault(): Lang {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === "en" || stored === "zh-TW") return stored;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh-TW" : "en";
}

interface Ctx {
  lang: Lang;
  setLang: (l: Lang) => void;
  t: (k: TranslationKey, vars?: Record<string, string | number>) => string;
}

const LanguageContext = createContext<Ctx | null>(null);

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detectDefault);
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, lang);
    document.documentElement.lang = lang; // WebKit uses this to pick the
    // correct Han glyph variant (TC vs SC/JP) and CJK line-breaking rules.
  }, [lang]);
  const setLang = (l: Lang) => setLangState(l);
  const t = (k: TranslationKey, vars?: Record<string, string | number>) =>
    translate(lang, k, vars);
  return (
    <LanguageContext.Provider value={{ lang, setLang, t }}>
      {children}
    </LanguageContext.Provider>
  );
}

export function useT() {
  const ctx = useContext(LanguageContext);
  if (!ctx) throw new Error("useT must be used within <LanguageProvider>");
  return ctx;
}
