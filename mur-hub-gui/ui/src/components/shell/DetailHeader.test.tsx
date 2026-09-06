import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { DetailHeader } from "./DetailHeader";

describe("DetailHeader markup", () => {
  it("renders avatar, title, meta and actions", () => {
    const html = renderToStaticMarkup(
      <DetailHeader avatar="A" title="AURA" meta={<span>m1</span>} actions={<button type="button">act</button>} />,
    );
    expect(html).toContain("detail-page__head");
    expect(html).toContain("detail-page__title");
    expect(html).toContain("AURA");
    expect(html).toContain("detail-page__meta");
    expect(html).toContain("m1");
    expect(html).toContain("detail-page__actions");
    expect(html).toContain("act");
  });
  it("omits the meta and actions wrappers when absent", () => {
    const html = renderToStaticMarkup(<DetailHeader avatar="A" title="AURA" />);
    expect(html).not.toContain("detail-page__meta");
    expect(html).not.toContain("detail-page__actions");
    expect(html).not.toContain("status-pill");
  });
});
