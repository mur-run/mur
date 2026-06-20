import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { PetFace } from "./PetFace";

// The whole point of PetFace is that the six styles look different offline and
// every expression reads distinctly. Guard both so a future palette/shape edit
// can't silently collapse them back into "one color ball".

const EXPRS = [
  "idle", "smile", "wave", "think", "wow", "sparkle",
  "cry", "sleep", "peek", "talk_open", "talk_close", "error",
];
const PRESETS = [
  "default-blob", "chiikawa", "sanrio-pastel", "sumikko",
  "vtuber-soft", "shimeji-retro", "family-photo",
];

describe("PetFace", () => {
  it("renders a distinct SVG for every style preset", () => {
    const seen = new Set<string>();
    for (const p of PRESETS) {
      const svg = renderToStaticMarkup(<PetFace presetId={p} family="chibi" expression="idle" />);
      expect(svg).toContain("<svg");
      seen.add(svg);
    }
    expect(seen.size).toBe(PRESETS.length);
  });

  it("renders a distinct face for each of the 12 expressions", () => {
    const seen = new Set<string>();
    for (const e of EXPRS) {
      seen.add(renderToStaticMarkup(<PetFace presetId="chiikawa" family="chibi" expression={e} />));
    }
    expect(seen.size).toBe(EXPRS.length);
  });

  it("falls back to a family default for an unknown preset", () => {
    const svg = renderToStaticMarkup(
      <PetFace presetId="totally-unknown" family="pixel" expression="idle" />,
    );
    expect(svg).toContain("<svg");
  });

  it("falls back to default-blob for unknown preset and unknown family", () => {
    const svg = renderToStaticMarkup(
      <PetFace presetId="nope" family="nope" expression="idle" />,
    );
    expect(svg).toContain("<svg");
  });
});
