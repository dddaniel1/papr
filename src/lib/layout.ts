// Window-size → layout decisions.
//
// Papr is a fixed three-pane desktop reader: a sidebar and an article list of
// pixel widths the user drags, and a reader that takes whatever is left. That
// composition is right for a wide window and wrong for a narrow one, and
// nothing ever re-checked the widths against the window — the stored values
// persist per machine, so dragging the panes wide on a large display and then
// opening a small window squeezed the reader to nothing.
//
// `effectiveColumns` is that re-check: it clamps the two fixed panes to what
// the window can actually spare, holding a floor under the reader. It never
// writes the stored preference, so widening the window brings the user's own
// widths straight back.
//
// `widthMode` / `heightMode` name the composition a viewport is in. The
// viewport hook mirrors them onto <html data-width-mode / data-height-mode>
// and styles.css restyles the chrome for each. They live here, as the single
// source of truth, which is why the CSS keys off those attributes instead of
// carrying a second copy of the breakpoints in media queries.

/** Minimum reader width while all three panes are visible. */
export const READER_FLOOR = 480;

/** Below this viewport width the sidebar folds to an icon rail. Sits where the
 *  stock three panes stop holding a readable measure (sidebar + list + the
 *  reader floor). */
export const COMPACT_MAX_WIDTH = 1120;

/** At or above this viewport width the window is classed as wide. The reader
 *  still respects the user's chosen line length. */
export const WIDE_MIN_WIDTH = 1440;

/** Below this viewport height the chrome's vertical rhythm tightens. */
export const SHORT_MAX_HEIGHT = 680;

/** Icon-rail width, in px. Mirrors `--rail-w` in styles.css, and wide enough
 *  to hold the macOS traffic lights when the sidebar collapses under them. */
export const RAIL_WIDTH = 72;

/** Below this width the list and reader take turns in the main pane. */
export const NARROW_MAX_WIDTH = RAIL_WIDTH + 300 + READER_FLOOR;

export type WidthMode = "narrow" | "compact" | "standard" | "wide";
export type HeightMode = "short" | "regular";

/** Which three-pane composition a viewport width calls for. */
export function widthMode(width: number): WidthMode {
  if (width > 0 && width < NARROW_MAX_WIDTH) return "narrow";
  if (width > 0 && width < COMPACT_MAX_WIDTH) return "compact";
  return width >= WIDE_MIN_WIDTH ? "wide" : "standard";
}

export function heightMode(height: number): HeightMode {
  return height > 0 && height < SHORT_MAX_HEIGHT ? "short" : "regular";
}

const clamp = (n: number, min: number, max: number) =>
  Math.min(max, Math.max(min, n));

export interface ColumnInput {
  /** Viewport width, in px. `0` means "not measurable" (e.g. a non-DOM
   *  environment) — the user's widths are then left alone. */
  width: number;
  /** The user's persisted sidebar width. */
  sidebar: number;
  /** The user's persisted list width. */
  list: number;
  sidebarMin: number;
  sidebarMax: number;
  listMin: number;
  listMax: number;
  readerFloor?: number;
  railWidth?: number;
}

export interface Columns {
  sidebar: number;
  list: number;
}

/**
 * The widths the two fixed panes should actually render at.
 *
 * In a compact window the sidebar is an icon rail, so its width is not the
 * user's to set there — it renders at `railWidth` regardless of the stored
 * value, and the stored value is what comes back when the window grows.
 */
export function effectiveColumns({
  width,
  sidebar,
  list,
  sidebarMin,
  sidebarMax,
  listMin,
  listMax,
  readerFloor = READER_FLOOR,
  railWidth = RAIL_WIDTH,
}: ColumnInput): Columns {
  if (width > 0 && width < NARROW_MAX_WIDTH) {
    return { sidebar: railWidth, list: Math.max(0, width - railWidth) };
  }

  const compact = width > 0 && width < COMPACT_MAX_WIDTH;
  const sidebarFloor = compact ? railWidth : sidebarMin;
  let s = compact ? railWidth : clamp(sidebar, sidebarMin, sidebarMax);
  let l = clamp(list, listMin, listMax);

  // Take back whatever the fixed panes are borrowing from the reader — from
  // the list first (it gives up width without losing navigation), then the
  // sidebar down to its own floor.
  if (width > 0) {
    let over = s + l - (width - readerFloor);
    const fromList = Math.max(0, Math.min(over, l - listMin));
    l -= fromList;
    over -= fromList;
    if (over > 0) s -= Math.min(over, s - sidebarFloor);
  }

  return { sidebar: s, list: l };
}
