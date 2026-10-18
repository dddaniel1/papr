import { fetchImage } from "../api";
import { imageDataUrl } from "./imageBytes";

const pending = new Map<string, Promise<string>>();

/** Share only in-flight work; retry always makes a fresh request. */
export function recoverImage(src: string, pageUrl?: string | null): Promise<string> {
  const key = JSON.stringify([src, pageUrl ?? null]);
  const existing = pending.get(key);
  if (existing) return existing;
  const request = fetchImage(src, pageUrl)
    .then((bytes) => imageDataUrl(src, bytes))
    .finally(() => pending.delete(key));
  pending.set(key, request);
  return request;
}

export function imageUrl(src: string, pageUrl?: string | null): string | null {
  if (!src.trim()) return null;
  try {
    const url = new URL(src, pageUrl || undefined);
    return /^(https?:|data:)$/.test(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}

export function needsImageProxy(src: string): boolean {
  try {
    const url = new URL(src);
    if (["cdnfile.sspai.com", "rssfile.sspai.com"].includes(url.hostname)) return true;
    const path = url.pathname.toLowerCase();
    if (path.endsWith(".heic") || path.endsWith(".heif")) return true;
    if (url.search.toLowerCase().includes(".heic") || url.search.toLowerCase().includes(".heif")) return true;
    return false;
  } catch {
    return false;
  }
}

interface ImageLabels {
  failed: string;
  retry: string;
  open: string;
}

/** One recovery path for injected article HTML and React-owned thumbnails. */
export function watchImage(
  img: HTMLImageElement,
  pageUrl: string | null | undefined,
  labels: ImageLabels,
  open: (url: string) => void,
): () => void {
  const original = img.dataset.paprSrc || img.currentSrc || img.getAttribute("src") || "";
  const src = imageUrl(original, pageUrl);
  let alive = true;
  let recovering = false;
  let attempted = false;
  let hint: HTMLSpanElement | undefined;
  const display = img.style.display;
  img.referrerPolicy = "no-referrer";

  const fail = () => {
    if (!alive || hint) return;
    img.style.display = "none";
    hint = document.createElement("span");
    hint.className = "image-failure";
    hint.setAttribute("role", "group");
    hint.setAttribute("aria-label", labels.failed);
    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    icon.setAttribute("viewBox", "0 0 24 24");
    icon.setAttribute("aria-hidden", "true");
    icon.classList.add("image-failure-icon");
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", "M9 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-4M3 16l5-5 5 5 3-3 5 5M15 3l6 6m0-6-6 6");
    icon.append(path);
    const message = document.createElement("span");
    message.className = "image-failure-message";
    message.textContent = labels.failed;
    hint.append(icon, message);
    if (src && /^https?:/.test(src)) {
      const retry = document.createElement("button");
      retry.type = "button";
      retry.className = "image-failure-retry";
      retry.textContent = labels.retry;
      retry.onclick = (event) => {
        event.preventDefault();
        event.stopPropagation();
        hint?.remove();
        hint = undefined;
        attempted = false;
        void recover();
      };
      const link = document.createElement("button");
      link.type = "button";
      link.className = "image-failure-open";
      link.textContent = labels.open;
      link.onclick = (event) => {
        event.preventDefault();
        event.stopPropagation();
        open(src);
      };
      hint.append(retry, link);
    }
    img.after(hint);
  };
  const recover = async () => {
    if (!alive || recovering) return;
    if (attempted || !src || !/^https?:/.test(src)) {
      fail();
      return;
    }
    attempted = true;
    recovering = true;
    try {
      const data = await recoverImage(src, pageUrl);
      if (!alive) return;
      img.dataset.paprSrc = src;
      img.removeAttribute("srcset");
      // A picture's sources otherwise override the recovered img.src.
      if (img.parentElement?.tagName === "PICTURE") {
        img.parentElement.querySelectorAll("source").forEach((source) => source.removeAttribute("srcset"));
      }
      img.style.display = display;
      img.src = data;
    } catch {
      fail();
    } finally {
      recovering = false;
    }
  };
  const onError = () => void recover();
  img.addEventListener("error", onError);
  const check = () => {
    if (!attempted && img.complete && img.naturalWidth === 0) void recover();
  };
  if (src && needsImageProxy(src)) void recover();
  else check();
  const timers = [250, 1000, 2500].map((delay) => window.setTimeout(check, delay));
  return () => {
    alive = false;
    timers.forEach(window.clearTimeout);
    img.removeEventListener("error", onError);
    img.style.display = display;
    hint?.remove();
  };
}

/** Compatibility for saved HTML that still contains lazy attributes or relative URLs. */
export function normalizeImageHtml(html: string, pageUrl?: string | null): string {
  if (!html) return html;
  const doc = new DOMParser().parseFromString(html, "text/html");
  doc.querySelectorAll("img").forEach((img) => {
    const lazy = ["data-src", "data-original", "data-actualsrc", "data-lazy-src"]
      .map((name) => img.getAttribute(name)?.trim())
      .find((value) => value && /^https?:/.test(imageUrl(value, pageUrl) || ""));
    const raw = img.getAttribute("src")?.trim();
    const source = img.parentElement?.tagName === "PICTURE"
      ? img.parentElement.querySelector("source") : null;
    const srcset = img.getAttribute("data-srcset") || img.getAttribute("srcset")
      || source?.getAttribute("data-srcset") || source?.getAttribute("srcset");
    // Saved feeds commonly keep a single candidate or a conventional URL/descriptor list.
    const placeholder = !raw || raw.startsWith("data:")
      || /(?:^|\/)(?:placeholder|image-placeholder|placeholder-image|spacer|blank|transparent|pixel|1x1|loading|lazy)(?:\.[^/?#]*)?(?:[?#].*)?$/i.test(raw);
    const candidate = placeholder
      ? lazy || srcset?.trim().split(/\s+/)[0]?.replace(/,$/, "") || raw : raw;
    const resolved = candidate && imageUrl(candidate, pageUrl);
    if (resolved) img.setAttribute("src", resolved);
    const promotedLazy = placeholder && !!lazy;
    if (promotedLazy || img.hasAttribute("data-srcset")) img.removeAttribute("srcset");
    if (promotedLazy && img.parentElement?.tagName === "PICTURE") {
      img.parentElement.querySelectorAll("source").forEach((item) => item.removeAttribute("srcset"));
    }
    if (!promotedLazy && srcset && !srcset.trim().startsWith("data:")) {
      const normalized = Array.from(srcset.matchAll(/(\S+)(?:\s+([\d.]+[wx]))?\s*(?:,|$)/g), ([, url, descriptor]) => {
        const absolute = imageUrl(url.replace(/,$/, ""), pageUrl);
        return absolute ? [absolute, descriptor].filter(Boolean).join(" ") : "";
      }).filter(Boolean).join(", ");
      if (normalized) (source || img).setAttribute("srcset", normalized);
    }
    img.setAttribute("referrerpolicy", "no-referrer");
  });
  return doc.body.innerHTML;
}
