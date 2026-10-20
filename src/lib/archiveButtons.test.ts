import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import ArchiveButtons from "../components/ArchiveButtons";

const state = vi.hoisted(() => ({ obsidian: false, notion: false, pending: false }));
vi.mock("@tanstack/react-query", () => ({
  useQuery: () => ({ data: { obsidian: { configured: state.obsidian }, notion: { configured: state.notion } } }),
  useMutation: () => ({ isPending: state.pending, mutate: vi.fn() }),
}));
vi.mock("../toast", () => ({ reportError: vi.fn(), toast: { show: vi.fn() } }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

describe("ArchiveButtons", () => {
  it("only renders a separate button for each configured service", () => {
    for (const [obsidian, notion, count] of [[false, false, 0], [true, false, 1], [false, true, 1], [true, true, 2]] as const) {
      Object.assign(state, { obsidian, notion, pending: false });
      const html = renderToStaticMarkup(createElement(ArchiveButtons, { articleId: 42 }));
      expect(html.match(/<button/g) ?? []).toHaveLength(count);
      expect(html.includes("reader.saveToObsidian")).toBe(obsidian);
      expect(html.includes("reader.saveToNotion")).toBe(notion);
    }
  });

  it("disables saving buttons and exposes busy state while saving", () => {
    Object.assign(state, { obsidian: true, notion: true, pending: true });
    const html = renderToStaticMarkup(createElement(ArchiveButtons, { articleId: 42 }));
    expect(html.match(/aria-busy="true"/g)).toHaveLength(2);
    expect(html.match(/disabled=""/g)).toHaveLength(2);
  });
});
