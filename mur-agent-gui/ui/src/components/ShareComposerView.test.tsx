// Integration coverage for the M-c3.5.1 share handler + M-c3.5.2
// ShareBadge together. Stubs a `ComposerHandle` whose mutators
// drive a `ShareDraft` state, runs `handleShareReceived` against
// it, and snapshots the rendered HTML.
//
// Avoids Playwright (the plan's original choice) so the harness
// stays in vitest — same posture as every other "Tauri-shaped
// surface, lib only" milestone in this PR series. Real
// browser-driven snapshots land alongside the production wiring
// follow-up.

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import {
    handleShareReceived,
    type ComposerHandle,
    type SharePayload,
} from "../lib/share";
import {
    ShareComposerView,
    type ShareDraft,
} from "./ShareComposerView";

function draftDrivenComposer(): { composer: ComposerHandle; draft: ShareDraft } {
    const draft: ShareDraft = { badge: null, body: "" };
    const composer: ComposerHandle = {
        insert: (text) => {
            draft.body = draft.body + text;
        },
        addBadge: (b) => {
            draft.badge = { ...b };
        },
        attachFile: (path, kindLabel) => {
            draft.attachment = { path, kindLabel };
        },
    };
    return { composer, draft };
}

describe("ShareComposerView + handleShareReceived", () => {
    it("renders share badge + body for a URL-scheme text share", () => {
        const { composer, draft } = draftDrivenComposer();
        const payload: SharePayload = {
            source: "url_scheme",
            kind: { kind: "text", value: "hello from URL scheme" },
            metadata: {},
        };
        handleShareReceived(payload, composer);

        const html = renderToStaticMarkup(<ShareComposerView draft={draft} />);
        expect(html).toContain("Shared via URL scheme");
        expect(html).toContain("hello from URL scheme");
        // No attachment chip for plain text.
        expect(html).not.toContain("share-attachment");
    });

    it("renders the dock-drop label for image attachments", () => {
        const { composer, draft } = draftDrivenComposer();
        const payload: SharePayload = {
            source: "dock",
            kind: { kind: "image", value: "/tmp/screenshot.png" },
            metadata: {},
        };
        handleShareReceived(payload, composer);

        const html = renderToStaticMarkup(<ShareComposerView draft={draft} />);
        expect(html).toContain("Shared by dropping on dock");
        // Attachment chip rendered with the path.
        expect(html).toContain("share-attachment");
        expect(html).toContain("/tmp/screenshot.png");
        // No body insert for image shares.
        const bodyMatch = html.match(/data-testid="share-body"[^>]*>([^<]*)</);
        expect(bodyMatch?.[1]).toBe("");
    });

    it("renders no badge when the draft is empty", () => {
        // Sanity check — ShareComposerView used outside the
        // share-handler path renders just the body.
        const empty: ShareDraft = { badge: null, body: "regular message" };
        const html = renderToStaticMarkup(<ShareComposerView draft={empty} />);
        expect(html).not.toContain("share-badge");
        expect(html).toContain("regular message");
    });
});
