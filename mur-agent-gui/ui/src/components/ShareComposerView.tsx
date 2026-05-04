// Track C3 / M-c3.5.3 — composer wrapper that renders the
// `ShareBadge` whenever a Track C3 share landed in the active draft.
//
// Production composer (in `tabs/Chat.tsx` or wherever the chat tab
// finally lives) will mount this internally; for now the component
// is a thin presentational wrapper so the integration test can
// snapshot the badge + body together without spinning up Playwright.

import type { ReactElement } from "react";
import { ShareBadge } from "./ShareBadge";

export interface ShareDraft {
    /// `null` when no Track C3 share is attached to the current draft.
    badge: { source: string; kindLabel: string; detail?: string } | null;
    body: string;
    /// Optional file ref for image / file shares. Rendered as a chip
    /// next to the body so users can confirm what they're about to send.
    attachment?: { path: string; kindLabel: string };
}

export interface ShareComposerViewProps {
    draft: ShareDraft;
}

export function ShareComposerView({
    draft,
}: ShareComposerViewProps): ReactElement {
    return (
        <div data-testid="share-composer" className="flex flex-col gap-2">
            {draft.badge && (
                <ShareBadge
                    source={draft.badge.source}
                    kindLabel={draft.badge.kindLabel}
                    detail={draft.badge.detail}
                />
            )}
            {draft.attachment && (
                <div
                    data-testid="share-attachment"
                    data-path={draft.attachment.path}
                    className="rounded border border-stone-300 bg-stone-50 px-2 py-1 text-xs"
                >
                    📎 {draft.attachment.path} ({draft.attachment.kindLabel})
                </div>
            )}
            <div data-testid="share-body" className="whitespace-pre-wrap">
                {draft.body}
            </div>
        </div>
    );
}
