import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SourceList } from "./SourceList";

const noop = () => {};
const rows = [
  { id: "aura", name: "AURA", subtitle: "Engineer", status: "running" as const, needsYou: 1, avatar: "A", facets: ["Engineer"] },
  { id: "scout", name: "Scout", status: "idle" as const, avatar: "S", facets: ["Research"] },
];

describe("SourceList markup", () => {
  it("marks the selected row and renders the needs-you badge", () => {
    const html = renderToStaticMarkup(
      <SourceList title="Agents" count={2} rows={rows} facets={[{ id: "Engineer", label: "Engineer", count: 1 }]}
        allLabel="All" activeFacet={null} onFacet={noop} filter="" onFilter={noop} filterPlaceholder="Filter"
        selectedId="aura" onSelect={noop} onCreate={noop} createLabel="New" emptyState={<p>none</p>} />,
    );
    expect(html).toContain('id="row-aura"');
    expect(html).toContain("source-row--on");
    expect(html).toContain('class="needs-you"');
    expect(html).toContain("status-dot--idle");
  });
  it("shows the empty state when the filter matches nothing", () => {
    const html = renderToStaticMarkup(
      <SourceList title="Agents" count={2} rows={rows} facets={[]} allLabel="All" activeFacet={null} onFacet={noop}
        filter="zzz" onFilter={noop} filterPlaceholder="Filter" selectedId={null} onSelect={noop} onCreate={noop}
        createLabel="New" emptyState={<p>none</p>} />,
    );
    expect(html).toContain("<p>none</p>");
  });
});
