// Unit tests for the window-size layout helpers.
// `layout.ts` is pure (the viewport width is an argument), so it is fully
// exercisable in node without a DOM.

import { describe, it, expect } from "vitest";
import {
  COMPACT_MAX_WIDTH,
  NARROW_MAX_WIDTH,
  RAIL_WIDTH,
  READER_FLOOR,
  SHORT_MAX_HEIGHT,
  WIDE_MIN_WIDTH,
  effectiveColumns,
  heightMode,
  widthMode,
} from "./layout";

// The stock pane widths every install starts with (store.ts + styles.css).
const DEFAULT_PANES = {
  sidebar: 248,
  list: 388,
  sidebarMin: 200,
  sidebarMax: 420,
  listMin: 300,
  listMax: 560,
};

const cols = (width: number, panes = DEFAULT_PANES) =>
  effectiveColumns({ width, ...panes });

describe("widthMode", () => {
  it("switches to list/detail when three panes cannot fit", () => {
    expect(widthMode(NARROW_MAX_WIDTH - 1)).toBe("narrow");
    expect(widthMode(NARROW_MAX_WIDTH)).toBe("compact");
  });
  it("is compact below the rail breakpoint", () => {
    expect(widthMode(COMPACT_MAX_WIDTH - 1)).toBe("compact");
  });

  it("is standard from the rail breakpoint up to the wide one", () => {
    expect(widthMode(COMPACT_MAX_WIDTH)).toBe("standard");
    expect(widthMode(WIDE_MIN_WIDTH - 1)).toBe("standard");
  });

  it("is wide from the wide breakpoint up", () => {
    expect(widthMode(WIDE_MIN_WIDTH)).toBe("wide");
  });

  it("treats an unmeasurable viewport as standard, never compact", () => {
    expect(widthMode(0)).toBe("standard");
  });
});

describe("heightMode", () => {
  it("tightens below the short breakpoint", () => {
    expect(heightMode(SHORT_MAX_HEIGHT - 1)).toBe("short");
    expect(heightMode(SHORT_MAX_HEIGHT)).toBe("regular");
  });

  it("treats an unmeasurable viewport as regular", () => {
    expect(heightMode(0)).toBe("regular");
  });
});

describe("effectiveColumns", () => {
  it("gives the main pane the remaining width in a narrow window", () => {
    for (const width of [320, 600, NARROW_MAX_WIDTH - 1]) {
      expect(cols(width)).toEqual({ sidebar: RAIL_WIDTH, list: width - RAIL_WIDTH });
    }
  });

  it("protects the reader at every supported three-pane width", () => {
    for (let width = NARROW_MAX_WIDTH; width <= 2560; width += 8) {
      const { sidebar, list } = cols(width, { ...DEFAULT_PANES, sidebar: 420, list: 560 });
      expect(width - sidebar - list).toBeGreaterThanOrEqual(READER_FLOOR);
    }
  });
  it("leaves the user's widths alone when the window can spare them", () => {
    // 1280 wide: 248 + 388 = 636, leaving the reader 644 — well clear of the
    // floor, so nothing is taken back.
    expect(cols(1280)).toEqual({ sidebar: 248, list: 388 });
  });

  it("holds the reader floor by shrinking the list first", () => {
    // 1200 wide with both panes maxed (420 + 560 = 980): the list gives up the
    // 260px it can spare and the sidebar keeps its width.
    const c = cols(1200, { ...DEFAULT_PANES, sidebar: 420, list: 560 });
    expect(c.sidebar).toBe(420);
    expect(c.list).toBe(300);
    expect(1200 - c.sidebar - c.list).toBe(READER_FLOOR);
  });

  it("never shrinks the list below its own minimum", () => {
    // Pane widths maxed (420 + 560 = 980) in a 1120 window: the list can only
    // return 260 of the 280px needed, so the rest comes off the sidebar.
    const c = cols(1120, { ...DEFAULT_PANES, sidebar: 420, list: 560 });
    expect(c.list).toBe(DEFAULT_PANES.listMin);
    expect(c.sidebar).toBeLessThan(420);
    expect(1120 - c.sidebar - c.list).toBe(READER_FLOOR);
  });

  it("folds the sidebar to the rail in a compact window", () => {
    const c = cols(900, { ...DEFAULT_PANES, sidebar: 420 });
    expect(c.sidebar).toBe(RAIL_WIDTH);
  });

  it("keeps the reader floor in a compact window with a wide list", () => {
    // Smallest three-pane width; narrower windows use list/detail navigation.
    const c = cols(860, { ...DEFAULT_PANES, list: 560 });
    expect(c.sidebar).toBe(RAIL_WIDTH);
    expect(860 - c.sidebar - c.list).toBeGreaterThanOrEqual(READER_FLOOR);
  });

  it("restores the stored width once the window grows again", () => {
    const stored = { ...DEFAULT_PANES, sidebar: 420, list: 560 };
    expect(cols(900, stored).list).toBeLessThan(560);
    expect(cols(1600, stored)).toEqual({ sidebar: 420, list: 560 });
  });

  it("does not collapse a narrow layout when the viewport is unmeasurable", () => {
    expect(cols(0, { ...DEFAULT_PANES, sidebar: 420, list: 560 })).toEqual({
      sidebar: 420,
      list: 560,
    });
  });

  it("clamps stored widths that fall outside the allowed range", () => {
    expect(cols(2000, { ...DEFAULT_PANES, sidebar: 9999, list: 1 })).toEqual({
      sidebar: DEFAULT_PANES.sidebarMax,
      list: DEFAULT_PANES.listMin,
    });
  });
});
