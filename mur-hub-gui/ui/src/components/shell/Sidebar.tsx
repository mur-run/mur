import type { ReactNode } from "react";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import { NAV_ITEMS, type PageId } from "./nav";

// Monochrome (currentColor) glyphs — WKWebView ignores font-variant-emoji, so
// emoji codepoints render in color; inline SVG is the only reliable mono path.
// Pattern extended from DashboardApp's `Ico` helper.
function Ico({ children }: { children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width="16"
      height="16"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

const GLYPHS: Record<PageId, ReactNode> = {
  home: <path d="M3 11.5 12 4l9 7.5V20a1 1 0 0 1-1 1h-5v-6H9v6H4a1 1 0 0 1-1-1z" />,
  chats: <path d="M21 12a8 8 0 0 1-8 8H5l2.3-3.1A8 8 0 1 1 21 12Z" />,
  agents: (
    <>
      <circle cx="9" cy="8" r="3" />
      <path d="M2 20c0-3.3 3.1-6 7-6s7 2.7 7 6" />
      <circle cx="17" cy="8" r="2.5" />
      <path d="M16 14.2c2.9.5 5 2.6 5 5.8" />
    </>
  ),
  fleets: (
    <>
      <circle cx="12" cy="6" r="2.5" />
      <circle cx="5" cy="17" r="2.5" />
      <circle cx="19" cy="17" r="2.5" />
      <path d="M12 8.5v3M9.5 15.5 10.7 12M14.5 15.5 13.3 12" />
    </>
  ),
  skills: <path d="M12 2 2 7l10 5 10-5Zm0 15L2 12v5l10 5 10-5v-5Z" />,
  workflows: (
    <>
      <rect x="3" y="3" width="6" height="6" rx="1" />
      <rect x="15" y="15" width="6" height="6" rx="1" />
      <path d="M9 6h6a3 3 0 0 1 3 3v6" />
    </>
  ),
  mcp: (
    <>
      <rect x="4" y="4" width="16" height="16" rx="2" />
      <path d="M9 9h6v6H9z" />
    </>
  ),
  models: <path d="M4 7a8 8 0 0 1 16 0v10a8 8 0 0 1-16 0Zm0 0h16" />,
  plugins: (
    <path d="M12.2 2h-.4a2 2 0 0 0-2 2v.2a2 2 0 0 1-1 1.7l-.4.3a2 2 0 0 1-2 0l-.2-.1a2 2 0 0 0-2.7.7l-.2.4a2 2 0 0 0 .7 2.7l.2.1a2 2 0 0 1 1 1.7v.5a2 2 0 0 1-1 1.7l-.2.1a2 2 0 0 0-.7 2.7l.2.4a2 2 0 0 0 2.7.7l.2-.1a2 2 0 0 1 2 0l.4.3a2 2 0 0 1 1 1.7V20a2 2 0 0 0 2 2h.4a2 2 0 0 0 2-2v-.2a2 2 0 0 1 1-1.7l.4-.3a2 2 0 0 1 2 0l.2.1a2 2 0 0 0 2.7-.7l.2-.4a2 2 0 0 0-.7-2.7l-.2-.1a2 2 0 0 1-1-1.7v-.5a2 2 0 0 1 1-1.7l.2-.1a2 2 0 0 0 .7-2.7l-.2-.4a2 2 0 0 0-2.7-.7l-.2.1a2 2 0 0 1-2 0l-.4-.3a2 2 0 0 1-1-1.7V4a2 2 0 0 0-2-2Z" />
  ),
};

export interface SidebarProps {
  active: PageId;
  badge: number;
  onSelect: (id: PageId) => void;
}

export function Sidebar({ active, badge, onSelect }: SidebarProps) {
  const { t } = useT();
  const workspace = NAV_ITEMS.filter((i) => i.group === "workspace");
  const library = NAV_ITEMS.filter((i) => i.group === "library");

  const renderItem = (id: PageId, labelKey: string) => (
    <button
      key={id}
      type="button"
      className={`shell-sidebar-item${active === id ? " shell-sidebar-item--active" : ""}`}
      onClick={() => onSelect(id)}
      aria-current={active === id ? "page" : undefined}
    >
      <span className="shell-sidebar-item__icon">
        <Ico>{GLYPHS[id]}</Ico>
      </span>
      <span className="shell-sidebar-item__label">{t(labelKey as TranslationKey)}</span>
      {id === "home" && badge > 0 && (
        <span className="shell-sidebar-item__badge">{badge > 99 ? "99+" : badge}</span>
      )}
    </button>
  );

  return (
    <nav className="shell-sidebar" aria-label="Primary">
      <div className="shell-sidebar__group">
        <div className="shell-sidebar__group-label">{t("nav.groupWorkspace" as TranslationKey)}</div>
        {workspace.map((i) => renderItem(i.id, i.labelKey))}
      </div>
      <div className="shell-sidebar__group">
        <div className="shell-sidebar__group-label">{t("nav.groupLibrary" as TranslationKey)}</div>
        {library.map((i) => renderItem(i.id, i.labelKey))}
      </div>
    </nav>
  );
}
