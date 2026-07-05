import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { LanguageProvider } from "../../i18n";
import { AgentPicker } from "./AgentPicker";

// No jsdom in this test env; LanguageProvider reads localStorage on mount.
(globalThis as { localStorage?: Storage }).localStorage ??= {
  getItem: () => null,
  setItem: () => {},
  removeItem: () => {},
  clear: () => {},
  key: () => null,
  length: 0,
} as Storage;

function withProvider(el: React.ReactElement) {
  return renderToStaticMarkup(<LanguageProvider>{el}</LanguageProvider>);
}

describe("AgentPicker", () => {
  it("renders one <option> per agent", () => {
    const html = withProvider(
      <AgentPicker agents={[{ name: "alice" }, { name: "bob" }]} value="alice" onChange={() => {}} />,
    );
    expect((html.match(/<option/g) ?? []).length).toBe(2);
    expect(html).toContain(">alice<");
    expect(html).toContain(">bob<");
  });

  it("marks the current value as selected", () => {
    const html = withProvider(
      <AgentPicker agents={[{ name: "alice" }, { name: "bob" }]} value="bob" onChange={() => {}} />,
    );
    expect(html).toContain('value="bob"');
  });

  it("disables the select when there are no agents", () => {
    const html = withProvider(<AgentPicker agents={[]} value="" onChange={() => {}} />);
    expect(html).toContain("disabled=\"\"");
  });
});
