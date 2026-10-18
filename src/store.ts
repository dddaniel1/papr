// Global UI state. Server data (feeds, articles…) lives in React Query;
// this store holds view selection plus the appearance preferences the
// design's settings / tweaks controls drive.

import { create } from "zustand";
import i18n from "./i18n";
import * as api from "./api";
import type { ArticleQuery } from "./types";

/** Appearance is two independent axes: a colour `Palette` (the family — warm
 *  Paper, cool Frost, high-contrast) and a light/dark `Mode`. Their product is
 *  the 6 themes; the CSS keys off `data-palette` + `data-mode` on the root.
 *
 *  `Mode` also carries `"system"`, which isn't a theme of its own — it follows
 *  the OS light/dark preference and resolves to one of the two concrete
 *  `ResolvedMode`s (see `resolveMode`) that the CSS + native backing key off. */
export type Palette = "paper" | "frost" | "contrast";
export type Mode = "light" | "dark" | "system";
export type ResolvedMode = "light" | "dark";
export const PALETTES: Palette[] = ["paper", "frost", "contrast"];
export const MODES: Mode[] = ["light", "dark", "system"];

/** Whether the OS currently prefers a dark colour scheme. Drives `"system"`
 *  mode. `matchMedia` always exists in the Tauri webview; the guard is just
 *  defensive so a non-DOM import site can't throw. */
export function systemPrefersDark(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches
  );
}

/** Resolve the stored appearance preference to the concrete light/dark that the
 *  CSS `data-mode` and native window backing use. `"system"` follows the OS. */
export function resolveMode(mode: Mode): ResolvedMode {
  if (mode === "system") return systemPrefersDark() ? "dark" : "light";
  return mode;
}
export type Density = "compact" | "cozy" | "spacious";
export type ViewMode = "list" | "card";
export type StartupView = "all" | "unread" | "starred" | "last";
export type ReaderFont = "serif" | "sans" | "hyperlegible";

/** Reader title/body typeface options. `stack` feeds the `--reader-font` CSS
 *  variable; `adjust` nudges the body font-size — sans and Hyperlegible read
 *  visually larger than the serif at the same pixel size, so they shrink a
 *  touch to keep the optical size even across choices. */
export const READER_FONTS: Record<ReaderFont, { stack: string; adjust: string }> = {
  serif: { stack: "var(--serif)", adjust: "0px" },
  sans: { stack: "var(--ui)", adjust: "-1.5px" },
  hyperlegible: { stack: "'Atkinson Hyperlegible', var(--ui)", adjust: "-1.5px" },
};

/** Valid ranges for the reader appearance sliders — the single source of
 *  truth shared by the Settings sliders, persistence validation, and the
 *  `setReader` write guard, so all three stay in lockstep. */
export const READER_BOUNDS = {
  size: { min: 14, max: 22 },
  leading: { min: 130, max: 200 },
  width: { min: 520, max: 840 },
} as const;

/** Valid ranges for the draggable pane widths — shared by the resize handles,
 *  persistence validation, and the `setPanel` write guard so all stay in
 *  lockstep (mirrors `READER_BOUNDS`). The article-list column and the AI
 *  drawer can grow wide, but never so far they crowd out the reader; the
 *  sidebar stays a navigation rail. */
export const PANEL_BOUNDS = {
  sidebar: { min: 200, max: 420 },
  list: { min: 300, max: 560 },
  ai: { min: 280, max: 560 },
} as const;

/** Clamp `n` into `[min, max]`. */
function clamp(n: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, n));
}

/** How an article opens by default: reader view, auto-extracted full text,
 *  or the embedded web view. Individual feeds can override it (`Feed.openMode`). */
export type OpenMode = "reader" | "extracted" | "web";
export const OPEN_MODES: readonly OpenMode[] = ["reader", "extracted", "web"];

/** Behavioural preferences driven by the Settings panel. */
export interface Prefs {
  showSidebarCounts: boolean;
  showCardThumbs: boolean;
  reduceMotion: boolean;
  showReadingTime: boolean;
  markReadOnOpen: boolean;
  markReadOnScroll: boolean;
  defaultOpenMode: OpenMode;
  startupView: StartupView;
  hideReadOnStartup: boolean;
  /** Sidebar "unread only" mode — hide feeds with no unread articles. */
  sidebarUnreadOnly: boolean;
}

const ls = {
  get: (k: string, fallback: string) => localStorage.getItem(k) ?? fallback,
  /** A persisted enum value, validated against `allowed` — a corrupt or
   *  stale value falls back instead of flowing through an unchecked cast. */
  oneOf: <T extends string>(k: string, allowed: readonly T[], fallback: T): T => {
    const v = localStorage.getItem(k);
    return v != null && (allowed as readonly string[]).includes(v)
      ? (v as T)
      : fallback;
  },
  /** A persisted number, clamped to `[min, max]`. localStorage is
   *  webview-writable and may hold a corrupt non-numeric value (NaN would
   *  reach a CSS variable like `--reader-size: NaNpx` and break the layout)
   *  or a stale out-of-range value from an older build with different
   *  slider limits — both would distort the reader. NaN falls back; an
   *  in-band-but-out-of-range value is clamped into range. */
  num: (k: string, fallback: number, min: number, max: number) => {
    const v = localStorage.getItem(k);
    if (v == null) return fallback;
    const n = Number(v);
    if (!Number.isFinite(n)) return fallback;
    return clamp(n, min, max);
  },
  bool: (k: string, fallback: boolean) => {
    const v = localStorage.getItem(k);
    return v == null ? fallback : v === "1";
  },
  set: (k: string, v: string | number | boolean) =>
    localStorage.setItem(k, typeof v === "boolean" ? (v ? "1" : "0") : String(v)),
};

interface UiState {
  /** The active sidebar selection driving the article list. */
  query: ArticleQuery;
  /** Human-readable label of the current selection (list header). */
  queryLabel: string;
  /** Currently open article, or null. */
  selectedArticleId: number | null;
  /** Hide already-read articles in the list. */
  unreadOnly: boolean;
  /** Sort the list oldest-first instead of newest-first. */
  sortOldest: boolean;
  /** Offset of the first page the middle-pane list loads — its paging anchor.
   *  Normally 0 (newest first). Opening an article from search that lives far
   *  down the list jumps the anchor to that article's page so the list loads
   *  *only* that page and pages outward from there, instead of every page above
   *  it. Part of the list's React Query key, so `currentList` reads the same
   *  window the user sees. Reset to 0 on any list-context change (feed / filter
   *  / sort). */
  listAnchor: number;

  // appearance preferences
  palette: Palette;
  mode: Mode;
  density: Density;
  viewMode: ViewMode;
  readerFont: ReaderFont;
  readerSize: number;
  readerLeading: number;
  readerWidth: number;

  // draggable pane widths (px)
  sidebarWidth: number;
  listWidth: number;
  aiWidth: number;

  // behavioural preferences
  prefs: Prefs;

  // transient view modes
  focusMode: boolean;
  aiOpen: boolean;
  sidebarOpen: boolean;
  collapsedFolders: Record<number, boolean>;
  /** A covering modal (subscribe / settings / explore …) is open. The reader's
   *  original-page view is a native child webview that floats above the whole
   *  DOM — including modals — so it must be torn down while one is up, or it
   *  occludes the dialog (issue #54). The reader effect watches this flag. */
  modalOpen: boolean;
  /** A floating context menu is open. Same hazard as `modalOpen`: a context
   *  menu raised over the reading area would be occluded by the native
   *  original-page webview that floats above the DOM (issue #74), so the
   *  reader suspends that view while a menu is up. */
  menuOpen: boolean;

  select: (query: ArticleQuery, label: string) => void;
  openArticle: (id: number | null) => void;
  toggleUnreadOnly: () => void;
  toggleSort: () => void;
  setListAnchor: (offset: number) => void;
  setFolderCollapsed: (id: number, collapsed: boolean) => void;
  toggleFolderCollapsed: (id: number) => void;
  toggleAllFolders: (folderIds: number[]) => boolean;

  setPalette: (p: Palette) => void;
  setMode: (m: Mode) => void;
  setDensity: (d: Density) => void;
  setViewMode: (v: ViewMode) => void;
  setReaderFont: (v: ReaderFont) => void;
  setReader: (p: Partial<Pick<UiState, "readerSize" | "readerLeading" | "readerWidth">>) => void;
  setPanel: (p: Partial<Pick<UiState, "sidebarWidth" | "listWidth" | "aiWidth">>) => void;

  setPref: (patch: Partial<Prefs>) => void;

  setFocusMode: (v: boolean) => void;
  setAiOpen: (v: boolean) => void;
  setSidebarOpen: (v: boolean) => void;
  toggleSidebar: () => void;
  setModalOpen: (v: boolean) => void;
  setMenuOpen: (v: boolean) => void;
}

const PREF_KEYS: (keyof Prefs)[] = [
  "showSidebarCounts",
  "showCardThumbs",
  "reduceMotion",
  "showReadingTime",
  "markReadOnOpen",
  "markReadOnScroll",
  "defaultOpenMode",
  "startupView",
  "hideReadOnStartup",
  "sidebarUnreadOnly",
];

/** Mirror the active palette + mode into the backend settings table so the Rust
 *  side can paint the native window in the matching colour *before* the webview
 *  loads on the next launch — without this, a dark user sees a brief light flash
 *  on startup before the JS initializes. Fire-and-forget; failures are harmless
 *  (the next launch falls back to the hardcoded Paper Light until the setting
 *  is saved again). */
function mirrorAppearance(palette: Palette, mode: Mode) {
  api.setSetting("palette", palette).catch(() => {});
  api.setSetting("mode", mode).catch(() => {});
}

/** Mirror the active `readerDarkShade` into backend settings table for the
 *  same reason — pre-painting the native window background on launch. */
function mirrorDarkShade() {
  const shade = localStorage.getItem("readerDarkShade") ?? "stone";
  api.setSetting("reader_dark_shade", shade).catch(() => {});
}

function loadPrefs(): Prefs {
  return {
    showSidebarCounts: ls.bool("pref.showSidebarCounts", true),
    showCardThumbs: ls.bool("pref.showCardThumbs", true),
    reduceMotion: ls.bool("pref.reduceMotion", false),
    showReadingTime: ls.bool("pref.showReadingTime", true),
    markReadOnOpen: ls.bool("pref.markReadOnOpen", true),
    markReadOnScroll: ls.bool("pref.markReadOnScroll", true),
    defaultOpenMode: ls.oneOf("pref.defaultOpenMode", OPEN_MODES, "reader"),
    startupView: ls.oneOf(
      "pref.startupView",
      ["all", "unread", "starred", "last"],
      "unread",
    ),
    hideReadOnStartup: ls.bool("pref.hideReadOnStartup", false),
    sidebarUnreadOnly: ls.bool("pref.sidebarUnreadOnly", false),
  };
}

const loadCollapsedFolders = (): Record<number, boolean> => {
  try {
    const raw = localStorage.getItem("collapsedFolders");
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
};

export const useUi = create<UiState>((set, get) => ({
  query: { kind: "unread" },
  queryLabel: i18n.t("smart.unread"),
  selectedArticleId: null,
  unreadOnly: false,
  sortOldest: false,
  listAnchor: 0,

  palette: ls.oneOf("palette", PALETTES, "paper"),
  mode: ls.oneOf("mode", MODES, "system"),
  density: ls.oneOf<Density>("density", ["compact", "cozy", "spacious"], "cozy"),
  viewMode: ls.oneOf<ViewMode>("viewMode", ["list", "card"], "card"),
  readerFont: ls.oneOf<ReaderFont>(
    "readerFont",
    ["serif", "sans", "hyperlegible"],
    "serif",
  ),
  readerSize: ls.num("readerSize", 16, READER_BOUNDS.size.min, READER_BOUNDS.size.max),
  readerLeading: ls.num(
    "readerLeading",
    165,
    READER_BOUNDS.leading.min,
    READER_BOUNDS.leading.max,
  ),
  readerWidth: ls.num("readerWidth", 680, READER_BOUNDS.width.min, READER_BOUNDS.width.max),

  sidebarWidth: ls.num("sidebarWidth", 248, PANEL_BOUNDS.sidebar.min, PANEL_BOUNDS.sidebar.max),
  listWidth: ls.num("listWidth", 388, PANEL_BOUNDS.list.min, PANEL_BOUNDS.list.max),
  aiWidth: ls.num("aiWidth", 360, PANEL_BOUNDS.ai.min, PANEL_BOUNDS.ai.max),

  prefs: loadPrefs(),

  focusMode: false,
  aiOpen: false,
  sidebarOpen: false,
  collapsedFolders: loadCollapsedFolders(),
  modalOpen: false,
  menuOpen: false,

  select: (query, label) => {
    // Remember the selection so the "open on startup: last view" preference
    // can restore it next launch.
    ls.set("lastView", JSON.stringify({ query, label }));
    // Reset the paging anchor: a new selection always opens at the newest page.
    set({ query, queryLabel: label, selectedArticleId: null, listAnchor: 0 });
  },
  openArticle: (id) => set({ selectedArticleId: id }),
  // Toggling a filter/sort rebuilds the list, so re-anchor to the newest page.
  toggleUnreadOnly: () => set((s) => ({ unreadOnly: !s.unreadOnly, listAnchor: 0 })),
  toggleSort: () => set((s) => ({ sortOldest: !s.sortOldest, listAnchor: 0 })),
  setListAnchor: (listAnchor) => set({ listAnchor }),
  setFolderCollapsed: (id, collapsed) =>
    set((s) => {
      const next = { ...s.collapsedFolders, [id]: collapsed };
      localStorage.setItem("collapsedFolders", JSON.stringify(next));
      return { collapsedFolders: next };
    }),
  toggleFolderCollapsed: (id) =>
    set((s) => {
      const next = { ...s.collapsedFolders, [id]: !s.collapsedFolders[id] };
      localStorage.setItem("collapsedFolders", JSON.stringify(next));
      return { collapsedFolders: next };
    }),
  toggleAllFolders: (folderIds) => {
    const s = get();
    const allCollapsed =
      folderIds.length > 0 && folderIds.every((id) => s.collapsedFolders[id]);
    const nextCollapsed = !allCollapsed;
    const next: Record<number, boolean> = {};
    for (const id of folderIds) {
      next[id] = nextCollapsed;
    }
    localStorage.setItem("collapsedFolders", JSON.stringify(next));
    set({ collapsedFolders: next });
    return nextCollapsed;
  },

  setPalette: (palette) => {
    ls.set("palette", palette);
    mirrorAppearance(palette, get().mode);
    set({ palette });
  },
  setMode: (mode) => {
    ls.set("mode", mode);
    mirrorAppearance(get().palette, mode);
    set({ mode });
  },
  setDensity: (density) => {
    ls.set("density", density);
    set({ density });
  },
  setViewMode: (viewMode) => {
    ls.set("viewMode", viewMode);
    set({ viewMode });
  },
  setReaderFont: (readerFont) => {
    ls.set("readerFont", readerFont);
    set({ readerFont });
  },
  setReader: (p) => {
    // Clamp through the bounds so a programmatic call shares the write guard,
    // pushing an out-of-range value into the persisted store or a CSS var.
    const next: Partial<Pick<UiState, "readerSize" | "readerLeading" | "readerWidth">> = {};
    if (p.readerSize != null) {
      next.readerSize = clamp(p.readerSize, READER_BOUNDS.size.min, READER_BOUNDS.size.max);
      ls.set("readerSize", next.readerSize);
    }
    if (p.readerLeading != null) {
      next.readerLeading = clamp(
        p.readerLeading,
        READER_BOUNDS.leading.min,
        READER_BOUNDS.leading.max,
      );
      ls.set("readerLeading", next.readerLeading);
    }
    if (p.readerWidth != null) {
      next.readerWidth = clamp(p.readerWidth, READER_BOUNDS.width.min, READER_BOUNDS.width.max);
      ls.set("readerWidth", next.readerWidth);
    }
    set(next);
  },
  setPanel: (p) => {
    // Clamp on write so neither a drag past the handle's guard nor a stale
    // persisted value (from an older build with different limits) can push an
    // out-of-range width into the store or its `--col-*` CSS variable.
    const next: Partial<Pick<UiState, "sidebarWidth" | "listWidth" | "aiWidth">> = {};
    if (p.sidebarWidth != null) {
      next.sidebarWidth = clamp(p.sidebarWidth, PANEL_BOUNDS.sidebar.min, PANEL_BOUNDS.sidebar.max);
      ls.set("sidebarWidth", next.sidebarWidth);
    }
    if (p.listWidth != null) {
      next.listWidth = clamp(p.listWidth, PANEL_BOUNDS.list.min, PANEL_BOUNDS.list.max);
      ls.set("listWidth", next.listWidth);
    }
    if (p.aiWidth != null) {
      next.aiWidth = clamp(p.aiWidth, PANEL_BOUNDS.ai.min, PANEL_BOUNDS.ai.max);
      ls.set("aiWidth", next.aiWidth);
    }
    set(next);
  },

  setPref: (patch) => {
    for (const k of PREF_KEYS) {
      if (patch[k] !== undefined) ls.set(`pref.${k}`, patch[k] as string | boolean);
    }
    set((s) => ({ prefs: { ...s.prefs, ...patch } }));
  },

  setFocusMode: (focusMode) => set({ focusMode }),
  setAiOpen: (aiOpen) => set({ aiOpen }),
  setSidebarOpen: (sidebarOpen) => set({ sidebarOpen }),
  toggleSidebar: () => set((s) => ({ sidebarOpen: !s.sidebarOpen })),
  setModalOpen: (modalOpen) => set({ modalOpen }),
  setMenuOpen: (menuOpen) => set({ menuOpen }),
}));

// Seed the backend's appearance copy on startup so an existing install — whose
// theme has lived only in localStorage until now — still gets the native
// launch background themed correctly from the next launch onward.
mirrorAppearance(useUi.getState().palette, useUi.getState().mode);
mirrorDarkShade();
