import { en } from "./en";

export type TranslationKey = keyof typeof en;
export type Lang = "en" | "zh-TW";
export type Table = Record<TranslationKey, string>;
