import type { ReactNode } from "react";
import type { SettingsSectionId } from "./settingsSections";

/** 24-unit stroke paths for the section nav (rendered through `Ico`). */
export const SETTINGS_GLYPHS: Record<SettingsSectionId, ReactNode> = {
  general: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9L17 7M7 17l-2.1 2.1" />
    </>
  ),
  models: <path d="M4 7a8 8 0 0 1 16 0v10a8 8 0 0 1-16 0Zm0 0h16" />,
  updates: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 16V8M8.5 11.5 12 8l3.5 3.5" />
    </>
  ),
  data: (
    <>
      <path d="M3 8l9-4 9 4-9 4Z" />
      <path d="M3 8v9l9 4 9-4V8M12 12v9" />
    </>
  ),
  about: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5M12 8h.01" />
    </>
  ),
};
