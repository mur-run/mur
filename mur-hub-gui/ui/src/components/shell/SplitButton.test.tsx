import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SplitButton } from "./SplitButton";
import { OverflowMenu } from "./OverflowMenu";

describe("menus at rest", () => {
  it("split button renders the primary label and a closed menu trigger", () => {
    const html = renderToStaticMarkup(<SplitButton label="Run" onPrimary={() => {}} items={[]} menuLabel="More run options" />);
    expect(html).toContain(">Run<");
    expect(html).toContain('aria-haspopup="menu"');
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain('role="menu"');
  });
  it("overflow menu is an icon button", () => {
    expect(renderToStaticMarkup(<OverflowMenu items={[]} label="More" />)).toContain('aria-label="More"');
  });
});
