import type { KeyboardEvent, ReactNode } from "react";
import { StatusPill, type StatusKind } from "./Status";

export interface DetailTabDef<T extends string> {
  id: T;
  label: string;
}

export function nextTab<T extends string>(tabs: DetailTabDef<T>[], active: T, delta: 1 | -1): T {
  const i = Math.max(0, tabs.findIndex((t) => t.id === active));
  return tabs[(i + delta + tabs.length) % tabs.length].id;
}

export interface DetailPageProps<T extends string> {
  avatar: ReactNode;
  title: string;
  status: StatusKind;
  meta?: ReactNode;
  actions?: ReactNode;
  tabs: DetailTabDef<T>[];
  activeTab: T;
  onTab: (id: T) => void;
  /** Rendered at the top of the body (needs-you strip, load errors). */
  banners?: ReactNode;
  children: ReactNode;
}

/** Header + ARIA tab bar + body (spec §4.2). The body remounts per tab so the
 *  cross-fade in detail-page.css runs on every switch. */
export function DetailPage<T extends string>(p: DetailPageProps<T>) {
  function onTabsKey(e: KeyboardEvent<HTMLDivElement>) {
    if (e.key === "ArrowRight") p.onTab(nextTab(p.tabs, p.activeTab, 1));
    else if (e.key === "ArrowLeft") p.onTab(nextTab(p.tabs, p.activeTab, -1));
  }
  return (
    <article className="detail-page">
      <header className="detail-page__head">
        <span className="detail-page__avatar">{p.avatar}</span>
        <div className="detail-page__ident">
          <h1 className="detail-page__title">
            {p.title} <StatusPill kind={p.status} />
          </h1>
          {p.meta && <div className="detail-page__meta">{p.meta}</div>}
        </div>
        {p.actions && <div className="detail-page__actions">{p.actions}</div>}
      </header>
      <div className="detail-page__tabs" role="tablist" onKeyDown={onTabsKey}>
        {p.tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            id={`tab-${t.id}`}
            aria-selected={t.id === p.activeTab}
            aria-controls={`panel-${t.id}`}
            tabIndex={t.id === p.activeTab ? 0 : -1}
            className={`detail-page__tab${t.id === p.activeTab ? " detail-page__tab--on" : ""}`}
            onClick={() => p.onTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>
      <div key={p.activeTab} className="detail-page__body" role="tabpanel" id={`panel-${p.activeTab}`} aria-labelledby={`tab-${p.activeTab}`}>
        {p.banners}
        {p.children}
      </div>
    </article>
  );
}
