import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

const platform = vi.hoisted(() => ({ mac: true }));
vi.mock("./platform", () => ({ isMac: () => platform.mac }));
vi.mock("./Sidebar", () => ({ Sidebar: () => null }));
import { Shell } from "./Shell";

function render(): string {
  return renderToStaticMarkup(
    <Shell page="home" onNavigate={() => {}} badge={0} onSettings={() => {}} onSearch={() => {}}>
      <p>page</p>
    </Shell>,
  );
}

describe("Shell window drag region", () => {
  afterEach(() => {
    platform.mac = true;
  });

  it("gives the content column a draggable title strip on macOS overlay title bar", () => {
    platform.mac = true;
    const html = render();
    expect(html).toContain('class="shell__titlebar"');
    expect(html).toContain("data-tauri-drag-region");
  });

  it("renders no title strip on Windows/Linux, which keep native decorations", () => {
    platform.mac = false;
    const html = render();
    expect(html).not.toContain("shell__titlebar");
  });
});
