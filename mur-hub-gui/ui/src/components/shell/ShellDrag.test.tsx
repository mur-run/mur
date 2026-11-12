import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("./platform", () => ({ isMac: () => true }));
vi.mock("./Sidebar", () => ({ Sidebar: () => null }));
import { Shell } from "./Shell";

describe("Shell window drag region", () => {
  it("gives the content column a draggable title strip on macOS overlay title bar", () => {
    const html = renderToStaticMarkup(
      <Shell page="home" onNavigate={() => {}} badge={0} onSettings={() => {}} onSearch={() => {}}>
        <p>page</p>
      </Shell>,
    );
    expect(html).toMatch(/<div class="shell__titlebar" data-tauri-drag-region="[^"]*"><\/div>/);
  });
});
