import type { ReactNode } from "react";
import { StatusPill, type StatusKind } from "./Status";

export interface DetailHeaderProps {
  avatar: ReactNode;
  title: string;
  /** Omit for objects without a runtime state (Library items). */
  status?: StatusKind;
  meta?: ReactNode;
  actions?: ReactNode;
}

/** The identity strip every detail shares (spec 3(a) §5): DetailPage renders
 *  it above its tabs, ChatPane above the conversation. */
export function DetailHeader(p: DetailHeaderProps) {
  return (
    <header className="detail-page__head">
      <span className="detail-page__avatar">{p.avatar}</span>
      <div className="detail-page__ident">
        <h1 className="detail-page__title">
          {p.title} {p.status && <StatusPill kind={p.status} />}
        </h1>
        {p.meta && <div className="detail-page__meta">{p.meta}</div>}
      </div>
      {p.actions && <div className="detail-page__actions">{p.actions}</div>}
    </header>
  );
}
