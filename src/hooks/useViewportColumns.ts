import { useLayoutEffect } from "react";
import { PANEL_BOUNDS, useUi } from "../store";
import { effectiveColumns, heightMode, widthMode } from "../lib/layout";

/**
 * Mirrors the responsive layout onto the document root, imperatively.
 *
 * Writes the two clamped pane widths to `--col-sidebar` / `--col-list` (which
 * the `.window` grid, its resize-seam gradient and the drag handles all read)
 * and the composition name to `data-width-mode` / `data-height-mode`, which
 * styles.css restyles the chrome from. See src/lib/layout.ts for the "why".
 *
 * Both halves deliberately live outside React state: a window drag-resize
 * fires far faster than the compositor can use it, and this app already fights
 * that jank (the resize-seam work in styles.css) — re-rendering all three
 * panes every frame to move a CSS variable would undo that effort. So the
 * resize listener writes straight to the DOM and nothing re-renders.
 * `useLayoutEffect` keeps the first write ahead of the first paint, so a
 * launch into a narrow window never flashes the wide composition.
 */
export function useViewportColumns() {
  useLayoutEffect(() => {
    const root = document.documentElement;

    const apply = () => {
      const width = window.innerWidth;
      const { sidebarWidth, listWidth } = useUi.getState();
      const cols = effectiveColumns({
        width,
        sidebar: sidebarWidth,
        list: listWidth,
        sidebarMin: PANEL_BOUNDS.sidebar.min,
        sidebarMax: PANEL_BOUNDS.sidebar.max,
        listMin: PANEL_BOUNDS.list.min,
        listMax: PANEL_BOUNDS.list.max,
      });
      root.style.setProperty("--col-sidebar", `${cols.sidebar}px`);
      root.style.setProperty("--col-list", `${cols.list}px`);
      root.dataset.widthMode = widthMode(width);
      root.dataset.heightMode = heightMode(window.innerHeight);
    };

    apply();

    // One write per frame: the resize event can outrun the compositor.
    let frame = 0;
    const onResize = () => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        apply();
      });
    };

    // Dragging a pane or moving the Reader-width slider changes the stored
    // widths without touching the window, so re-clamp on those too.
    const unsub = useUi.subscribe((s, prev) => {
      if (s.sidebarWidth !== prev.sidebarWidth || s.listWidth !== prev.listWidth)
        apply();
    });

    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      if (frame) cancelAnimationFrame(frame);
      unsub();
    };
  }, []);
}
