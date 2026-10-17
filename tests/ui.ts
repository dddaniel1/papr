// Open /tests/ui.html with `pnpm dev`. All IPC uses disposable in-memory data.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { ArticleSummary, Feed } from "../src/types";

const feed: Feed = {
  id: 1, title: "Test feed", feedUrl: "https://example.test/feed", siteUrl: null,
  description: null, faviconUrl: null, folderId: null, sourceType: "rss",
  lastFetchedAt: null, fetchError: null, unreadCount: 120, refreshIntervalMin: null,
  autoTranslate: false, openMode: null,
};
const articles: ArticleSummary[] = Array.from({ length: 120 }, (_, i) => ({
  id: i + 1, feedId: 1, feedTitle: feed.title, sourceType: "rss", title: `Article ${i + 1}`,
  author: null, snippet: "A reading preview that stays readable after marking this article read.",
  imageUrl: null, url: null, publishedAt: "2026-10-17T00:00:00Z", isRead: false,
  isStarred: false, readLater: false,
}));
const state = {
  feeds: [] as Feed[], articles: [] as ArticleSummary[], settings: new Map<string, string>(),
  failSetting: "", failPage: false, refresh: "partial", summaryCalls: 0,
  pendingSummary: null as null | (() => void), askCalls: 0, digestCalls: 0,
};
mockWindows("main");
mockIPC(async (cmd, args: any = {}) => {
  switch (cmd) {
    case "get_setting": return state.settings.get(args.key) ?? null;
    case "set_setting":
      if (args.key === state.failSetting) throw new Error("Test save failure");
      state.settings.set(args.key, args.value); return;
    case "list_feeds": return state.feeds;
    case "list_folders": case "list_tags": case "list_rules": case "list_highlights": return [];
    case "smart_counts": return { unread: state.articles.length, starred: 0, readLater: 0 };
    case "list_articles":
      if (state.failPage && args.offset > 0) throw new Error("Test page failure");
      return state.articles.slice(args.offset, args.offset + args.limit);
    case "get_article": return { ...articles.find((a) => a.id === args.id), contentHtml: "<p>Full article body.</p>", extractedHtml: null, aiSummary: null, translatedHtml: null, translatedLang: null, enclosures: [], tags: [] };
    case "article_index": return args.id - 1;
    case "refresh_feeds":
      if (state.refresh === "skipped") args.onProgress.onmessage({ event: "skipped" });
      else if (state.refresh === "partial") args.onProgress.onmessage({ event: "feedDone", data: { feedId: 1, newArticles: 0, error: "Test feed failure" } });
      else if (state.refresh === "sync") args.onProgress.onmessage({ event: "syncFailed", data: { error: "Test sync failure" } });
      return 0;
    case "ai_summarize":
      state.summaryCalls++;
      return new Promise<void>((resolve) => {
        state.pendingSummary = () => {
          args.onToken.onmessage({ type: "delta", data: "Completed summary" });
          args.onToken.onmessage({ type: "done" });
          resolve();
        };
      });
    case "ai_ask": case "ai_digest":
      if (cmd === "ai_ask") state.askCalls++; else state.digestCalls++;
      args.onToken.onmessage({ type: "delta", data: cmd === "ai_ask" ? "Answer from your feeds" : "Your recent briefing" });
      args.onToken.onmessage({ type: "done" }); return;
    case "storage_stats": return { dbBytes: 1024, articleCount: state.articles.length, feedCount: state.feeds.length };
    case "freshrss_status": return { connected: false };
    case "plugin:app|version": return "test";
    case "plugin:autostart|is_enabled": return false;
    default: return null;
  }
}, { shouldMockEvents: true });
// Keep this page's preferences independent from the installed app.
localStorage.setItem("language", "en");
localStorage.setItem("pref.markReadOnOpen", "0");
localStorage.setItem("palette", "paper");
localStorage.setItem("mode", "light");
await import("../src/main");
const { useUi } = await import("../src/store");
const { emit } = await import("@tauri-apps/api/event");
const waitFor = async (predicate: () => boolean, name: string) => {
  const end = Date.now() + 5000;
  while (!predicate()) {
    if (Date.now() > end) throw new Error(`Timed out: ${name}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
};
const click = (selector: string) => {
  const el = document.querySelector<HTMLElement>(selector);
  if (!el) throw new Error(`Missing: ${selector}`);
  el.click();
};
const nav = async (text: string) => {
  const el = Array.from(document.querySelectorAll<HTMLButtonElement>(".settings-nav-item")).find((el) => el.textContent?.trim() === text);
  if (!el) throw new Error(`Missing settings section: ${text}`);
  el.click();
  await waitFor(() => document.querySelector(".settings-header h2")?.textContent === text, `settings section ${text}`);
};
const key = (key: string, extra = {}) => window.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, ...extra }));
const setInput = (selector: string, value: string) => {
  const el = document.querySelector<HTMLInputElement>(selector)!;
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
};
const checks: string[] = [];
async function runChecks() {
  await waitFor(() => document.body.textContent!.includes("Add your first feed"), "first-run guidance");
  click(".empty .empty-retry");
  await waitFor(() => !!document.querySelector(".modal"), "add feed action"); key("Escape");
  checks.push("Empty state opens subscription dialog");

  click('.sb-footer button[aria-label*="Settings"]');
  await waitFor(() => !!document.querySelector(".settings-window"), "settings");
  if (document.querySelectorAll("button.settings-nav-item").length !== 10) throw new Error("Settings navigation is not keyboard focusable");
  if (!document.querySelector(".settings-window")!.contains(document.activeElement)) throw new Error("Settings did not take focus");
  await nav("Notifications");
  await waitFor(() => document.querySelector<HTMLInputElement>(".s-toggle")?.disabled === false, "notification settings");
  state.failSetting = "notify_enabled";
  const toggle = document.querySelector<HTMLInputElement>(".s-toggle")!;
  const checked = toggle.checked; toggle.click();
  await waitFor(() => document.body.textContent!.includes("Test save failure"), "failed save feedback");
  if (toggle.checked !== checked) throw new Error("Failed save changed the control");
  state.failSetting = "";
  checks.push("Settings keyboard targets, initial focus and failed toggle save");

  await nav("Subscriptions");
  await waitFor(() => document.querySelector<HTMLInputElement>('input[aria-label="Instance"]')?.disabled === false, "RSSHub setting");
  state.failSetting = "rsshub_instance";
  setInput('input[aria-label="Instance"]', "https://example.test/rsshub");
  await new Promise((resolve) => setTimeout(resolve, 30));
  const instance = document.querySelector<HTMLInputElement>('input[aria-label="Instance"]')!;
  instance.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
  await waitFor(() => instance.value === "" && !instance.disabled, "RSSHub rollback");
  state.failSetting = "";
  checks.push("Failed RSSHub save restores the saved address");

  await nav("General");
  await waitFor(() => !!document.querySelector<HTMLInputElement>(".s-slider"), "refresh interval");
  state.failSetting = "refresh_interval_min";
  setInput(".s-slider", "60");
  document.querySelector(".s-slider")!.dispatchEvent(new Event("pointerup", { bubbles: true }));
  await waitFor(() => document.querySelector<HTMLInputElement>(".s-slider")?.value === "30", "slider rollback");
  state.failSetting = "";
  checks.push("Failed interval save restores the slider");
  key("Escape");

  for (const [value, expected] of [["partial", "feed(s) failed"], ["skipped", "already running"], ["sync", "sync failed"]]) {
    state.refresh = value;
    click('.sb-footer button[aria-label*="Refresh"]');
    await waitFor(() => document.querySelector(".toast")?.textContent?.includes(expected) ?? false, value);
  }
  checks.push("Refresh distinguishes partial failure, busy and sync failure");

  state.feeds = [feed]; state.articles = articles; state.failPage = true;
  await emit("feeds-updated");
  await waitFor(() => !!document.querySelector(".art"), "articles");
  const scroll = document.querySelector(".list-scroll")!;
  scroll.scrollTop = scroll.scrollHeight;
  scroll.dispatchEvent(new Event("scroll"));
  await waitFor(() => document.body.textContent!.includes("Could not load more articles"), "pagination error");
  state.failPage = false;
  click(".list-page-status .empty-retry");
  await waitFor(() => !document.body.textContent!.includes("Could not load more articles"), "pagination recovery");
  checks.push("Pagination error is visible and retry recovers");

  useUi.getState().openArticle(1);
  await waitFor(() => !!document.querySelector(".reader-toolbar"), "reader");
  useUi.getState().setAiOpen(true);
  await waitFor(() => state.summaryCalls === 1, "summary request");
  useUi.getState().setAiOpen(false);
  await new Promise((resolve) => setTimeout(resolve, 30));
  useUi.getState().setAiOpen(true);
  await new Promise((resolve) => setTimeout(resolve, 30));
  state.pendingSummary!();
  await waitFor(() => document.querySelector(".ai-drawer")?.textContent?.includes("Completed summary") ?? false, "summary resume");
  if (state.summaryCalls !== 1) throw new Error("Reopening duplicated a summary request");
  checks.push("Summary survives close/reopen without duplicate generation");

  click('.sb-footer button[aria-label="AI assistant"]');
  await waitFor(() => !!document.querySelector(".ai-dialog"), "AI entry");
  setInput(".ai-dialog .modal-input", "What changed?");
  await new Promise((resolve) => setTimeout(resolve, 30));
  document.querySelector(".ai-dialog form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  await waitFor(() => document.body.textContent!.includes("Answer from your feeds"), "AI answer");
  document.querySelectorAll<HTMLButtonElement>(".ai-dialog .s-seg button")[1].click();
  await new Promise((resolve) => setTimeout(resolve, 30));
  document.querySelector(".ai-dialog form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  await waitFor(() => document.body.textContent!.includes("Your recent briefing"), "AI briefing");
  if (state.askCalls !== 1 || state.digestCalls !== 1) throw new Error("AI request counts differ");
  checks.push("AI question and briefing entries produce streamed results");
  key("Escape");
  await checkThemeContrast();
  return checks;
}

async function checkThemeContrast() {
  useUi.getState().setAiOpen(false);
  const row = document.querySelector<HTMLElement>(".art")!;
  const originalClass = row.className;
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 1;
  const ctx = canvas.getContext("2d")!;
  const luminance = () => [...ctx.getImageData(0, 0, 1, 1).data].slice(0, 3)
    .map((v) => { v /= 255; return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; })
    .reduce((sum, v, i) => sum + v * [0.2126, 0.7152, 0.0722][i], 0);
  try {
    for (const palette of ["paper", "frost", "contrast"] as const) {
      for (const mode of ["light", "dark"] as const) {
        useUi.getState().setPalette(palette); useUi.getState().setMode(mode);
        await new Promise((resolve) => setTimeout(resolve, 30));
        for (const active of [false, true]) {
          row.classList.add("read"); row.classList.toggle("active", active);
          // Let selection/background transitions settle before sampling.
          await new Promise((resolve) => setTimeout(resolve, 150));
          ctx.fillStyle = getComputedStyle(document.querySelector(".list")!).backgroundColor;
          ctx.fillRect(0, 0, 1, 1);
          ctx.fillStyle = getComputedStyle(row).backgroundColor;
          ctx.fillRect(0, 0, 1, 1);
          const background = luminance();
          for (const selector of [".art-title", ".art-snippet", ".art-feed", ".art-time"]) {
            ctx.fillStyle = getComputedStyle(row.querySelector(selector)!).color;
            ctx.fillRect(0, 0, 1, 1);
            const foreground = luminance();
            const ratio = (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05);
            if (ratio < 4.5) throw new Error(`${palette}/${mode}/${selector}/active=${active}: contrast ${ratio.toFixed(2)}`);
          }
        }
      }
    }
    checks.push("Read article text meets 4.5:1 in all six themes, including selection");
  } finally {
    row.className = originalClass;
    useUi.getState().setPalette("paper"); useUi.getState().setMode("light");
  }
}
Object.assign(window, { paprTest: { state, checks, runChecks, checkThemeContrast, nav, key } });
