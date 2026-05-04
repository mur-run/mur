import { describe, it, expect, vi } from "vitest";
import { handleShareReceived } from "./share";

describe("handleShareReceived", () => {
    it("inserts text into composer with badge", () => {
        const composer = { insert: vi.fn(), addBadge: vi.fn() };
        handleShareReceived(
            {
                source: "url_scheme",
                kind: { kind: "text", value: "hello" },
                metadata: {},
            },
            composer,
        );
        expect(composer.insert).toHaveBeenCalledWith("hello");
        expect(composer.addBadge).toHaveBeenCalledWith({
            source: "url_scheme",
            kindLabel: "text",
        });
    });

    it("inserts url with link treatment", () => {
        const composer = { insert: vi.fn(), addBadge: vi.fn() };
        handleShareReceived(
            {
                source: "hotkey",
                kind: { kind: "url", value: "https://x.com" },
                metadata: {},
            },
            composer,
        );
        expect(composer.insert).toHaveBeenCalledWith("https://x.com");
        expect(composer.addBadge).toHaveBeenCalledWith({
            source: "hotkey",
            kindLabel: "url",
        });
    });

    it("attaches image with file ref", () => {
        const composer = {
            insert: vi.fn(),
            addBadge: vi.fn(),
            attachFile: vi.fn(),
        };
        handleShareReceived(
            {
                source: "dock",
                kind: { kind: "image", value: "/tmp/a.png" },
                metadata: {},
            },
            composer,
        );
        expect(composer.attachFile).toHaveBeenCalledWith("/tmp/a.png", "image");
        expect(composer.addBadge).toHaveBeenCalledWith({
            source: "dock",
            kindLabel: "image",
        });
        // File refs do NOT also call insert — that would double-render
        // the path as both a chip and inline text.
        expect(composer.insert).not.toHaveBeenCalled();
    });

    it("attaches file with file ref", () => {
        const composer = {
            insert: vi.fn(),
            addBadge: vi.fn(),
            attachFile: vi.fn(),
        };
        handleShareReceived(
            {
                source: "services",
                kind: { kind: "file", value: "/tmp/notes.pdf" },
                metadata: {},
            },
            composer,
        );
        expect(composer.attachFile).toHaveBeenCalledWith(
            "/tmp/notes.pdf",
            "file",
        );
    });

    it("missing attachFile is a soft no-op for image/file payloads", () => {
        // Composer that only knows about text — share handler must not
        // throw when it sees an image; the badge alone is still useful.
        const composer = { insert: vi.fn(), addBadge: vi.fn() };
        expect(() =>
            handleShareReceived(
                {
                    source: "dock",
                    kind: { kind: "image", value: "/tmp/a.png" },
                    metadata: {},
                },
                composer,
            ),
        ).not.toThrow();
        expect(composer.addBadge).toHaveBeenCalled();
    });
});
