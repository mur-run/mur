// Vitest doesn't ship a DOM by default and we don't want to pull in
// @testing-library/react + jsdom just to assert that a label renders
// — `renderToStaticMarkup` produces deterministic HTML that we can
// substring-check against, which is exactly what these tests need.

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ShareBadge } from "./ShareBadge";

describe("ShareBadge", () => {
    it("renders the URL-scheme label", () => {
        const html = renderToStaticMarkup(
            <ShareBadge
                source="url_scheme"
                kindLabel="text"
                detail="muragent-coach://share?text=..."
            />,
        );
        expect(html).toContain("Shared via URL scheme");
        expect(html).toContain("text");
    });

    it("renders the hotkey label", () => {
        const html = renderToStaticMarkup(
            <ShareBadge
                source="hotkey"
                kindLabel="text"
                detail="Cmd+Shift+M+C"
            />,
        );
        expect(html).toContain("Shared via hotkey");
    });

    it("renders the Services label", () => {
        const html = renderToStaticMarkup(
            <ShareBadge source="services" kindLabel="image" />,
        );
        expect(html).toContain("Shared via Services menu");
    });

    it("renders the dock-drop label", () => {
        const html = renderToStaticMarkup(
            <ShareBadge source="dock" kindLabel="file" />,
        );
        expect(html).toContain("Shared by dropping on dock");
    });

    it("falls back to a generic label for unknown sources", () => {
        // Defensive: if the Rust side ever emits a new channel we
        // haven't mapped yet, the badge still renders something
        // sensible instead of "undefined".
        const html = renderToStaticMarkup(
            <ShareBadge source="future_channel" kindLabel="text" />,
        );
        expect(html).toContain("Shared via future_channel");
    });

    it("expandable accordion shows raw source detail", () => {
        const html = renderToStaticMarkup(
            <ShareBadge
                source="hotkey"
                kindLabel="text"
                detail="Cmd+Shift+M+C"
            />,
        );
        expect(html).toContain("<details");
        expect(html).toContain("Where this came from");
        expect(html).toContain("Cmd+Shift+M+C");
    });

    it("omits the accordion when no detail is supplied", () => {
        // No detail → no accordion. Avoids rendering an empty
        // collapsible that's confusing to click on.
        const html = renderToStaticMarkup(
            <ShareBadge source="hotkey" kindLabel="text" />,
        );
        expect(html).not.toContain("<details");
        expect(html).not.toContain("Where this came from");
    });

    it("uses an amber border + background for trust nudging", () => {
        // The visual treatment matches the B0 `<untrusted_share>`
        // wrapping; tests pin the Tailwind classes so a future
        // theme refactor can't silently downgrade the warning.
        const html = renderToStaticMarkup(
            <ShareBadge source="url_scheme" kindLabel="text" />,
        );
        expect(html).toContain("border-amber-400");
        expect(html).toContain("bg-amber-50/40");
    });
});
