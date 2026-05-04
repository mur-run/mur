// Track C3 / M-c3.5.1 — Tauri share-event listener.
//
// The Rust side emits a `share:received` event from `DefaultIngestor`
// (mur-agent-gui/src-tauri/src/send/mod.rs) once a shared payload has
// been routed through the multimodal pipeline. The composer UI calls
// `startShareListener` at mount; each event resolves to a
// `SharePayload` that we hand to `handleShareReceived`, which pushes
// the body into the active composer and tags it with a `ShareBadge`
// so the user knows where the content came from.
//
// `ComposerHandle` is intentionally minimal — the real composer is
// owned by the chat tab (M-c3.5.2 wires it up). Tests pass `vi.fn()`
// stubs so we can assert on the calls without rendering anything.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// Wire-shape mirrors `mur_agent_gui_lib::send::ShareKind` (see
// `src-tauri/src/send/mod.rs`). Tagged-union `kind`/`value` matches
// `#[serde(rename_all = "snake_case", tag = "kind", content = "value")]`.
export type ShareKind =
    | { kind: "text"; value: string }
    | { kind: "url"; value: string }
    | { kind: "image"; value: string }
    | { kind: "file"; value: string };

export interface SharePayload {
    source: string;
    kind: ShareKind;
    metadata: Record<string, unknown>;
}

// The composer surface the share handler talks to. Defined as an
// interface (not a class) so the chat tab and unit tests can each
// supply their own implementation without a shared base type.
export interface ComposerHandle {
    insert(text: string): void;
    addBadge(b: { source: string; kindLabel: string }): void;
    attachFile?(path: string, kindLabel: string): void;
}

// Pure routing logic — extracted so it can be unit-tested without
// touching `@tauri-apps/api/event`. `startShareListener` is the thin
// wrapper that wires this to the actual event bus.
export function handleShareReceived(p: SharePayload, composer: ComposerHandle) {
    const kindLabel = p.kind.kind;
    composer.addBadge({ source: p.source, kindLabel });
    switch (p.kind.kind) {
        case "text":
        case "url":
            composer.insert(p.kind.value);
            break;
        case "image":
        case "file":
            composer.attachFile?.(p.kind.value, kindLabel);
            break;
    }
}

// Subscribe to `share:received`. Caller must invoke the returned
// `UnlistenFn` on unmount to avoid duplicate handlers when the
// composer remounts (e.g. tab switch).
export async function startShareListener(
    composer: ComposerHandle,
): Promise<UnlistenFn> {
    return listen<SharePayload>("share:received", (e) =>
        handleShareReceived(e.payload, composer),
    );
}
