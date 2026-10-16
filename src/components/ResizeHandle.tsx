import { useCallback, useEffect, useRef } from "react";

interface Props {
  /** Pixel width of the pane this handle resizes, at drag start. Used for the
   *  keyboard steps, and as the fallback when `varName` is absent or unset. */
  width: number;
  /**
   * Name of the CSS custom property carrying the pane's *rendered* width, if
   * it differs from `width`. A window can clamp a pane narrower than its
   * stored width (see lib/layout.ts), and this handle's `left` already tracks
   * that property — so the drag must start from the same live value, or the
   * first pixel of movement would snap the pane out to its pre-clamp width.
   */
  varName?: string;
  /** `right`: dragging right grows the pane (sidebar / list, handle on the
   *  pane's right edge). `left`: dragging left grows the pane (the AI drawer,
   *  handle on its left edge). */
  side: "left" | "right";
  /** Lower clamp for the resulting width (px). */
  min: number;
  /** Upper clamp for the resulting width (px). */
  max: number;
  /** Called continuously during the drag with the new, clamped width. */
  onResize: (width: number) => void;
  /** Accessible label for the separator. */
  label: string;
}

/**
 * The pane's live width: the custom property when the caller named one (the
 * window may be clamping the pane narrower than its stored width), else the
 * prop.
 */
function liveWidth(varName: string | undefined, fallback: number): number {
  if (!varName) return fallback;
  const px = parseFloat(
    getComputedStyle(document.documentElement).getPropertyValue(varName),
  );
  return Number.isFinite(px) ? px : fallback;
}

/**
 * A thin draggable separator between two panes. Implemented with native
 * pointer events (no third-party resize library) to stay lightweight and match
 * the project's hand-rolled interaction code.
 *
 * The drag is tracked from the pointer's start X and the pane's start width, so
 * a slow first frame can't desync the handle from the cursor. Move/up are bound
 * on `window` so the drag survives the cursor outrunning the 7px hit area, and
 * the body `col-resize` cursor + a no-select guard make the gesture feel native.
 *
 * We deliberately do NOT use `setPointerCapture`: the handle's slot moves every
 * frame (its `left` tracks the live `--col-*` variable), and WebKit (the macOS
 * Tauri webview) releases the capture whenever the captured element changes
 * position — which dropped the drag and flickered the line mid-gesture. The
 * handle draws no line on hover/drag now (the `col-resize` cursor is the
 * affordance); only keyboard focus shows one, where capture loss can't apply.
 */
export default function ResizeHandle({ width, varName, side, min, max, onResize, label }: Props) {
  // Latest props in a ref so the move/up listeners (bound once per drag) always
  // read fresh values without re-binding mid-drag.
  const latest = useRef({ width, varName, side, min, max, onResize });
  latest.current = { width, varName, side, min, max, onResize };

  const cleanupDrag = useRef<(() => void) | null>(null);

  const onPointerDown = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    // Ignore secondary buttons so a right-click context menu can't start a drag.
    if (e.button !== 0) return;
    e.preventDefault();
    cleanupDrag.current?.();
    const startX = e.clientX;
    const { width, varName } = latest.current;
    const startW = liveWidth(varName, width);

    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    const move = (ev: PointerEvent) => {
      const { side, min, max, onResize } = latest.current;
      const dx = ev.clientX - startX;
      // `right` panes grow as the cursor moves right; `left` panes (drawer on
      // the right) grow as it moves left.
      const raw = side === "right" ? startW + dx : startW - dx;
      onResize(Math.min(max, Math.max(min, raw)));
    };
    const up = () => {
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      cleanupDrag.current = null;
    };
    cleanupDrag.current = up;
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  }, []);

  // Keyboard a11y: arrow keys nudge the boundary in 16px steps for users who
  // can't drag.
  const onKeyDown = useCallback((e: React.KeyboardEvent<HTMLDivElement>) => {
    const { width, varName, side, min, max, onResize } = latest.current;
    const step = e.shiftKey ? 48 : 16;
    let delta = 0;
    if (e.key === "ArrowLeft") delta = side === "right" ? -step : step;
    else if (e.key === "ArrowRight") delta = side === "right" ? step : -step;
    else return;
    e.preventDefault();
    const startW = liveWidth(varName, width);
    onResize(Math.min(max, Math.max(min, startW + delta)));
  }, []);

  // Belt-and-braces: if this handle unmounts mid-drag (e.g. focus mode hides
  // the panes), make sure the global cursor/select overrides are cleared.
  useEffect(() => {
    return () => {
      cleanupDrag.current?.();
    };
  }, []);

  return (
    <div
      className="resize-handle"
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
    />
  );
}
