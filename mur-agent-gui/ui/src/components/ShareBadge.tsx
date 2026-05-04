// Track C3 / M-c3.5.2 — visual treatment for shared content.
//
// Rendered next to a composer entry whenever a Track C3 channel
// (URL scheme / hotkey / Services menu / dock-drop) injected the
// content. The amber border-l + soft amber background nudges the
// user that this came from outside the agent's normal trust
// boundary — same visual language the B0 hook applies in the
// `<untrusted_share>` tag.
//
// The expandable `<details>` accordion shows raw provenance
// ("where this came from") so a user can audit in the rare case
// they're suspicious about a payload — e.g. a URL that doesn't
// match what they thought they were sharing.

import type { ReactElement } from "react";

const CHANNEL_LABELS: Record<string, string> = {
    url_scheme: "Shared via URL scheme",
    hotkey: "Shared via hotkey",
    services: "Shared via Services menu",
    dock: "Shared by dropping on dock",
};

export interface ShareBadgeProps {
    source: string;
    kindLabel: string;
    detail?: string;
}

export function ShareBadge({
    source,
    kindLabel,
    detail,
}: ShareBadgeProps): ReactElement {
    const label = CHANNEL_LABELS[source] ?? `Shared via ${source}`;
    return (
        <div
            data-testid="share-badge"
            data-source={source}
            data-kind={kindLabel}
            className="border-l-4 border-amber-400 bg-amber-50/40 px-3 py-2 text-sm"
        >
            <div className="font-medium text-amber-900">{label}</div>
            <div className="text-xs uppercase tracking-wide text-amber-700">
                {kindLabel}
            </div>
            {detail !== undefined && (
                <details className="mt-1 text-xs text-amber-800">
                    <summary className="cursor-pointer select-none">
                        Where this came from
                    </summary>
                    <pre className="mt-1 whitespace-pre-wrap break-all font-mono">
                        {detail}
                    </pre>
                </details>
            )}
        </div>
    );
}
