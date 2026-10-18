import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import * as api from "../api";
import { LANGUAGES } from "../i18n";
import { useUi, PANEL_BOUNDS } from "../store";
import { usePlayer } from "../player";
import { useTranslationJobs } from "../translation";
import { useArticleActions } from "../hooks/articleActions";
import { renderMarkdown } from "../lib/markdown";
import { normalizeImageHtml, watchImage } from "../lib/imageLoading";
import ArticleImage from "./ArticleImage";
import { fullDate } from "../lib/feedMeta";
import { bilibiliPlayerUrl } from "../lib/bilibili";
import { isMac } from "../lib/platform";
import { reportError, toast } from "../toast";
import { tagColor } from "../lib/tagColors";
import type { ArticleDetail } from "../types";
import Icon from "./Icon";
import TagPicker from "./TagPicker";
import ResizeHandle from "./ResizeHandle";
import HighlightLayer from "./HighlightLayer";
import ContextMenu, { type MenuEntry } from "./ContextMenu";
import Lightbox from "./Lightbox";

interface Props {
  onToast: (msg: string) => void;
}

function youtubeId(url: string | null): string | null {
  if (!url) return null;
  const m =
    url.match(/[?&]v=([\w-]{11})/) || url.match(/youtu\.be\/([\w-]{11})/);
  return m ? m[1] : null;
}

/** Plain, entity-decoded text of an HTML body — for the reading-time estimate.
 *  A bare `replace(/<[^>]+>/g, " ")` tag-strip leaves HTML entities intact, so
 *  `Tom &amp; Jerry &mdash; done` would be counted as 5 words / 28 chars when
 *  the real text ("Tom & Jerry — done") is 4 words / 18 chars — inflating the
 *  estimate on entity-heavy articles. Parsing into an inert document decodes
 *  every entity (`&amp;` → `&`, `&mdash;` → `—`) and drops markup cleanly. */
function bodyPlainText(html: string): string {
  if (!html) return "";
  // DOMParser documents are inert — nothing here executes or loads.
  return new DOMParser().parseFromString(html, "text/html").body.textContent ?? "";
}

/** True when the body opens with its own visual media (image / video / iframe)
 *  before any real text. Such an article already leads with a strong visual, so
 *  prepending the list-thumbnail hero on top of it just shows a second, often
 *  unrelated image — the exact complaint in issue #97, where the body starts
 *  with a `<video>` cover while the feed's `media:thumbnail` is a different png.
 *  Walking in document order (media element before the first non-whitespace text
 *  node) is what the plain `body.includes(imageUrl)` guard can't catch, since
 *  the hero image and the body's lead media are distinct URLs here. */
function bodyLeadsWithMedia(html: string): boolean {
  if (!html) return false;
  const doc = new DOMParser().parseFromString(html, "text/html");
  const walker = doc.createTreeWalker(
    doc.body,
    NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT,
  );
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    if (node.nodeType === Node.TEXT_NODE) {
      if ((node.textContent ?? "").trim().length > 0) return false;
    } else if (node instanceof Element) {
      if (["IMG", "VIDEO", "IFRAME", "PICTURE"].includes(node.tagName)) return true;
    }
  }
  return false;
}

/** CJK ideographs + Japanese kana + Korean Hangul — scripts read by the
 *  character, not the whitespace-delimited word. */
const CJK_CHAR = /[぀-ヿ㐀-鿿가-힯豈-﫿]/u;
/** Global-flagged variant of `CJK_CHAR` for stripping every CJK glyph. */
const CJK_CHAR_GLOBAL = new RegExp(CJK_CHAR.source, "gu");

/** Estimate reading time in minutes for an article body's plain text.
 *
 *  A mixed-script estimate: CJK scripts have no word spacing, so they are
 *  counted by the character (~480 chars/min); latin-script text is counted by
 *  the whitespace-delimited word (~220 wpm). The two contributions are *summed*
 *  — the previous `Math.max(words/220, chars/480)` always lost for English
 *  (a 1000-word article spans ~5500 chars, so `chars/480` ≈ 11 dwarfed the
 *  true `words/220` ≈ 4.5), inflating every latin-script article ~2-3×. */
function estimateReadMinutes(text: string): number {
  let cjkChars = 0;
  for (const ch of text) {
    if (CJK_CHAR.test(ch)) cjkChars++;
  }
  // Words, with CJK characters stripped so they are not also counted as
  // single-character "words" by the latin path.
  const latinWords = text
    .replace(CJK_CHAR_GLOBAL, " ")
    .trim()
    .split(/\s+/)
    .filter(Boolean).length;
  const minutes = cjkChars / 480 + latinWords / 220;
  return Math.max(2, Math.round(minutes));
}

/** Decode a URL fragment, tolerating a malformed `%` escape. A real-world
 *  anchor can carry a literal percent (`#100%-growth`, `#section-50%`), which
 *  is not a valid escape sequence — `decodeURIComponent` throws `URIError` on
 *  it. The bare value still works as an `id` lookup, so fall back to it rather
 *  than letting the throw escape the click handler and kill the link. */
function decodeFragment(frag: string): string {
  try {
    return decodeURIComponent(frag);
  } catch {
    return frag;
  }
}

/** Pull the in-page fragment out of a link click, or null if it isn't one.
 *
 *  Two shapes count as in-page: a bare `#frag` href, and — because the body
 *  HTML is sanitized with the article's URL as the rewrite base — an absolute
 *  `https://site/article#frag` that resolves to the very article being read.
 *  `sourceUrl` is the article's own URL, used to recognise that second case.
 */
function inPageFragment(raw: string, sourceUrl: string | null): string | null {
  if (raw[0] === "#") return decodeFragment(raw.slice(1));
  if (!sourceUrl) return null;
  try {
    const u = new URL(raw);
    const b = new URL(sourceUrl);
    if (u.hash && u.origin === b.origin && u.pathname === b.pathname) {
      return decodeFragment(u.hash.slice(1));
    }
  } catch {
    /* not a parseable absolute URL — treat as external */
  }
  return null;
}

/** Build a click handler for links inside injected HTML (article body, AI
 *  summary). In-page anchor links (footnotes, tables of contents) scroll to
 *  their target within the reader; everything else opens in the external
 *  browser — a bare <a> click would otherwise navigate the Tauri webview away
 *  from the app entirely (or, for a fragment link, to a bogus `app://…#frag`). */
function makeLinkClickHandler(sourceUrl: string | null) {
  return (e: React.MouseEvent) => {
    const link = (e.target as HTMLElement).closest("a");
    if (!link) return;
    const raw = link.getAttribute("href");
    if (!raw) return;
    e.preventDefault();

    const hash = inPageFragment(raw, sourceUrl);
    if (hash != null) {
      if (hash === "") return; // bare `#` — no element to reach
      const root = link.closest(".article-body, .ai-prose");
      // getElementById can't be scoped to the body, so match by id or the
      // legacy `<a name>` form within the rendered content.
      const target = root?.querySelector(
        `[id="${CSS.escape(hash)}"], a[name="${CSS.escape(hash)}"]`,
      );
      target?.scrollIntoView({ behavior: "smooth", block: "start" });
      return;
    }

    openUrl(link.href).catch(() => {});
  };
}

export default function Reader({ onToast }: Props) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions(toast.error);
  const id = useUi((s) => s.selectedArticleId);
  const focusMode = useUi((s) => s.focusMode);
  const setFocusMode = useUi((s) => s.setFocusMode);
  const aiOpen = useUi((s) => s.aiOpen);
  const setAiOpen = useUi((s) => s.setAiOpen);
  const markReadOnOpen = useUi((s) => s.prefs.markReadOnOpen);
  const markReadOnScroll = useUi((s) => s.prefs.markReadOnScroll);
  const showReadingTime = useUi((s) => s.prefs.showReadingTime);
  const defaultOpenMode = useUi((s) => s.prefs.defaultOpenMode);

  const [scrolled, setScrolled] = useState(false);
  // Which body to show when an extraction exists follows the default open
  // mode: "reader" (the default) shows the feed's own content and extraction
  // is opt-in via the toolbar button; "extracted" shows the full text.
  const [showExtracted, setShowExtracted] = useState(
    defaultOpenMode === "extracted",
  );
  const [showTranslation, setShowTranslation] = useState(false);
  // Reading vs. the article's original web page, shown in an in-app iframe.
  // Sites that set X-Frame-Options / CSP frame-ancestors refuse to load this
  // way — the in-frame hint points those back to "open in browser".
  const [viewMode, setViewMode] = useState<"reader" | "web">("reader");
  const [tagPick, setTagPick] = useState<{ x: number; y: number } | null>(null);
  const [ctxMenu, setCtxMenu] = useState<{
    x: number;
    y: number;
    // Set when the right-click landed on an article image / over a text
    // selection, so the menu can offer image- and copy-specific actions.
    imageUrl?: string;
    selection?: string;
  } | null>(null);
  // Full-screen image viewer: the article's image srcs + the one to open on
  // (issue #87). Null when closed.
  const [lightbox, setLightbox] = useState<{ srcs: string[]; index: number } | null>(
    null,
  );
  const scrollRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  // Host element the native original-page child webview is positioned over.
  const pageHostRef = useRef<HTMLDivElement>(null);
  // Article id we already auto-marked read via scroll, so a flurry of scroll
  // events near the foot doesn't fire `setRead` repeatedly before the
  // optimistic cache patch lands.
  const scrollMarkedRef = useRef<number | null>(null);
  const playTrack = usePlayer((s) => s.play);
  const playingSrc = usePlayer((s) => (s.playing ? s.track?.src : null));

  const article = useQuery({
    queryKey: ["article", id],
    queryFn: () => api.getArticle(id as number),
    enabled: id != null,
  });
  const a: ArticleDetail | undefined = article.data;

  // Feed list, so the article's source feed can be checked for its per-feed
  // auto-translate flag. Shared cache key with the sidebar — no extra fetch.
  const feeds = useQuery({ queryKey: ["feeds"], queryFn: api.listFeeds });
  const autoTranslateFeed = !!(
    a && feeds.data?.find((f) => f.id === a.feedId)?.autoTranslate
  );
  // Effective open mode (issue #110): the feed's own setting, falling back to
  // the global default. `undefined` while the article or feed list is still
  // loading, so the open-mode/auto-extract effects below don't fire before the
  // feed's own setting is known.
  const feedOpenMode =
    a && feeds.data
      ? feeds.data.find((f) => f.id === a.feedId)?.openMode ?? null
      : undefined;
  const openMode =
    feedOpenMode === undefined ? undefined : feedOpenMode ?? defaultOpenMode;

  const readMinutes = useMemo(() => {
    return estimateReadMinutes(bodyPlainText(a?.extractedHtml || a?.contentHtml || ""));
    // Recompute when the body changes — including after full-text extraction
    // replaces the short feed snippet, which keeps the same article id.
  }, [a?.extractedHtml, a?.contentHtml]);

  // Reset scroll + extraction view on article change.
  useEffect(() => {
    setShowExtracted(useUi.getState().prefs.defaultOpenMode === "extracted");
    setShowTranslation(false);
    setViewMode("reader");
    setScrolled(false);
    setTagPick(null);
    scrollMarkedRef.current = null;
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
  }, [id]);

  // Apply the effective open mode once the article (and the feed list) is
  // available — declared after the reset above so it wins the same commit.
  // Applied once per article, so the toolbar toggles keep working afterwards
  // and a feeds refetch never yanks the view back.
  const openModeAppliedRef = useRef<number | null>(null);
  useEffect(() => {
    if (!a || openMode === undefined || openModeAppliedRef.current === a.id)
      return;
    openModeAppliedRef.current = a.id;
    if (openMode === "web" && a.url) setViewMode("web");
    else setShowExtracted(openMode === "extracted");
  }, [a, openMode]);

  // Drive the native original-page child webview (page_view.rs) while in web
  // mode. It floats above the DOM, so we measure the host rect and keep the
  // webview aligned to it across window/sidebar resizes. (Switching articles
  // resets viewMode to "reader" above, so a.url is stable within a session.)
  const articleUrl = a?.url ?? null;
  // Anything that floats over the reading area must suspend the page view: the
  // child webview floats above the whole DOM, so it would otherwise occlude a
  // covering modal (subscribe / settings / explore — issue #54), a context
  // menu raised over the reader (issue #74), the tag picker, or the AI drawer
  // that slides over the reader's right edge. We *hide* the webview rather than
  // tear it down, so dismissing the overlay reveals the already-loaded page
  // instantly instead of reloading it (the bounds keep syncing while hidden,
  // below).
  const modalOpen = useUi((s) => s.modalOpen);
  const menuOpen = useUi((s) => s.menuOpen);
  const overlayOpen = modalOpen || menuOpen || aiOpen || tagPick != null;
  // Read the latest value inside the lifecycle effect without making it a
  // dependency — overlays toggle visibility (below), never the webview's life.
  const overlayOpenRef = useRef(overlayOpen);
  overlayOpenRef.current = overlayOpen;

  // Webview lifecycle: create on entering web mode (or when the article's URL
  // changes), destroy on leaving. Bounds track the host across resizes even
  // while hidden, so re-showing never lands the page at a stale rect.
  useEffect(() => {
    const host = pageHostRef.current;
    if (viewMode !== "web" || !articleUrl || !host) return;

    const bounds = () => {
      const r = host.getBoundingClientRect();
      return { x: r.left, y: r.top, width: r.width, height: r.height };
    };
    let open = false;
    let cancelled = false;
    api.openPageView(articleUrl, bounds()).then(
      () => {
        open = true;
        // If teardown ran while this open was still in flight, the cleanup's
        // closePageView fired against a not-yet-created webview (a no-op) and
        // would leave this one orphaned above the DOM — close it now.
        if (cancelled) {
          api.closePageView().catch(() => {});
          return;
        }
        // Created visible by default; if an overlay is already up (e.g. the AI
        // drawer was open when web mode was toggled on), hide it at once.
        if (overlayOpenRef.current) api.setPageViewVisible(false).catch(() => {});
      },
      () => {},
    );

    const sync = () => {
      if (open && !cancelled) api.setPageViewBounds(bounds()).catch(() => {});
    };
    const ro = new ResizeObserver(sync);
    ro.observe(host);
    window.addEventListener("resize", sync);

    return () => {
      cancelled = true;
      ro.disconnect();
      window.removeEventListener("resize", sync);
      api.closePageView().catch(() => {});
    };
  }, [viewMode, articleUrl]);

  // Visibility: toggle the webview as overlays come and go, without reloading.
  // No-op (backend-side) when no webview is open.
  useEffect(() => {
    if (viewMode !== "web" || !articleUrl) return;
    api.setPageViewVisible(!overlayOpen).catch(() => {});
  }, [overlayOpen, viewMode, articleUrl]);

  // Mark as read once when an unread article is opened (if the user opted in).
  useEffect(() => {
    if (a && !a.isRead && markReadOnOpen) actions.setRead(a.id, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [a?.id]);

  // The extracted article id travels as the mutation variable, not via the
  // `a` closure: extraction is async and the user can switch articles before
  // it resolves. Keying onSuccess off the live `a` would invalidate the wrong
  // article (the extracted text never shows on return) and toast "full text
  // extracted" while reading an unrelated, un-extracted article.
  const extract = useMutation({
    mutationFn: (articleId: number) => api.extractFulltext(articleId),
    onSuccess: (_data, articleId) => {
      qc.invalidateQueries({ queryKey: ["article", articleId] });
      // Only the article still on screen should flip into the extracted view
      // and surface the toast.
      if (useUi.getState().selectedArticleId === articleId) {
        setShowExtracted(true);
        onToast(t("reader.fullTextExtracted"));
      }
    },
    onError: (e) => reportError(e),
  });

  // The default translation target + engine, switched inline from the toolbar
  // The default target language + engine come from Settings. The toolbar can
  // override them per translation, but only temporarily: the override is not
  // written back and resets to the default when switching articles. The article's
  // cached `translatedLang` (and any running job's `lang`) is compared against the
  // effective `targetLang` to decide whether a translation is current for it.
  const translateSetting = useQuery({
    queryKey: ["setting", "translate_target_lang"],
    queryFn: () => api.getSetting("translate_target_lang"),
  });
  const defaultLang = translateSetting.data || i18n.language;
  const engineSetting = useQuery({
    queryKey: ["setting", "translate_engine"],
    queryFn: () => api.getSetting("translate_engine"),
  });
  const defaultEngine = engineSetting.data || "llm";
  // `null` = follow the default; a string = a temporary per-article override.
  const [tmpLang, setTmpLang] = useState<string | null>(null);
  const [tmpEngine, setTmpEngine] = useState<string | null>(null);
  const targetLang = tmpLang ?? defaultLang;
  const engine = tmpEngine ?? defaultEngine;
  // Drop the temporary overrides when the article changes so each article opens
  // on the configured defaults.
  useEffect(() => {
    setTmpLang(null);
    setTmpEngine(null);
  }, [id]);

  // Background translation jobs run independently of this view, so several
  // articles can translate at once and switching away never interrupts one.
  const startTranslate = useTranslationJobs((s) => s.translate);
  const job = useTranslationJobs((s) => (id != null ? s.jobs[id] : undefined));

  const hasExtracted = !!a?.extractedHtml;
  const canTranslate = !!(a?.extractedHtml || a?.contentHtml);
  const baseBody =
    (showExtracted && a?.extractedHtml ? a.extractedHtml : a?.contentHtml) || "";
  const jobForTarget = job && job.lang === targetLang ? job : undefined;
  const translating = jobForTarget?.status === "translating";
  const cachedValid = !!a?.translatedHtml && a.translatedLang === targetLang;
  const translatedBody =
    jobForTarget?.html || (cachedValid ? a?.translatedHtml ?? "" : "");
  const hasTranslation = !!translatedBody;
  const showToggle = hasTranslation || translating;
  const body = showTranslation
    ? translatedBody ||
      (translating ? `<p><em>${t("reader.translating")}</em></p>` : baseBody)
    : baseBody;
  const displayBody = useMemo(() => normalizeImageHtml(body, a?.url), [body, a?.url]);
  const emptyBody = t("reader.noContent");
  // Stable identity prevents scroll renders from replacing recovered image/highlight nodes.
  const bodyMarkup = useMemo(() => ({
    __html: displayBody || `<p><em>${emptyBody}</em></p>`,
  }), [displayBody, emptyBody]);
  // Whether the body already opens with its own image/video — if so, the hero
  // thumbnail is suppressed to avoid a redundant top image (issue #97).
  const leadsWithMedia = useMemo(() => bodyLeadsWithMedia(baseBody), [baseBody]);

  useEffect(() => {
    const root = bodyRef.current;
    if (!root || viewMode !== "reader") return;
    const cleanups = Array.from(root.querySelectorAll("img"), (img) =>
      watchImage(img, a?.url, {
        failed: t("reader.imageLoadFailed"),
        retry: t("common.retry"),
        open: t("reader.openOriginalImage"),
      }, (url) => { openUrl(url).catch(reportError); }),
    );
    return () => cleanups.forEach((cleanup) => cleanup());
  }, [displayBody, a?.id, a?.url, viewMode, t]);

  // When a translation finishes, refetch the article so its persisted
  // `translatedHtml` lands in the cache — the toggle then keeps working after
  // the in-memory job is gone (e.g. reopening the article in a later session).
  useEffect(() => {
    if (id == null || !job) return;
    if (job.status === "done") {
      qc.invalidateQueries({ queryKey: ["article", id] });
    } else if (job.status === "error") {
      // The translation failed (a toast already surfaced why) — drop back to the
      // original so the view isn't stuck on an empty "translating…" state.
      setShowTranslation(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, job?.status]);

  // With an "extracted" open mode in effect, a summary-only feed item is
  // upgraded to the full page the moment it's opened, so the reader never
  // shows a two-line stub. Skipped when the feed already carries the whole
  // article, when there is no source URL to fetch, or once attempted for this
  // article — so a failed fetch isn't retried on every re-render.
  const autoExtractedRef = useRef<number | null>(null);
  useEffect(() => {
    if (openMode !== "extracted" || !a || !a.url || a.extractedHtml) return;
    if (autoExtractedRef.current === a.id || extract.isPending) return;
    // Measure the *decoded* text, not the raw markup. A bare `<[^>]+>` tag
    // strip leaves HTML entities intact, so an entity-heavy stub
    // (`&nbsp;`-padded copy, `&mdash;`/`&amp;` runs) is over-counted — a
    // genuinely short snippet can clear the 800-char bar and wrongly look
    // "complete", leaving the reader showing the very stub auto-extract is
    // meant to replace. `bodyPlainText` decodes entities and drops markup
    // cleanly, the same measurement the reading-time estimate already uses.
    // An explicit per-feed "extracted" skips the bar entirely — the user asked
    // for the page's own text even when the feed body looks complete.
    if (feedOpenMode !== "extracted") {
      const plain = bodyPlainText(a.contentHtml || "").trim();
      if (plain.length >= 800) return; // feed already delivers the full text
    }
    autoExtractedRef.current = a.id;
    extract.mutate(a.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [a?.id, a?.extractedHtml, openMode, feedOpenMode]);

  // Per-feed auto-translate: when the article's source feed opts in, translate
  // it into the configured target the moment it opens. Fires once per article
  // (guarded by `autoTranslatedRef`), only when there's a body to translate and
  // a usable cached translation in the target language isn't already present —
  // so a feed left untouched still shows its original text, and reopening a
  // cached article doesn't re-spend an API call. The toolbar toggle still lets
  // the reader flip back to the original at any time.
  const autoTranslatedRef = useRef<number | null>(null);
  useEffect(() => {
    if (!autoTranslateFeed || !a || !canTranslate) return;
    if (autoTranslatedRef.current === a.id) return;
    autoTranslatedRef.current = a.id;
    // A fresh cached translation for the target language needs no new job;
    // just surface it. Otherwise start a background translation.
    if (!(a.translatedHtml && a.translatedLang === targetLang)) {
      startTranslate(a.id, targetLang, engine);
    }
    setShowTranslation(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [a?.id, autoTranslateFeed, canTranslate, targetLang, engine]);

  // Mark the current article read once its foot is reached. Also fires for an
  // article short enough to need no scrolling at all (`scrollHeight` already
  // within `clientHeight`) — that case produces no `scroll` event, so without
  // a render-time check a fully-visible short article would never be marked
  // read despite "mark read on scroll" being on.
  const markReadIfAtFoot = useCallback(() => {
    const el = scrollRef.current;
    if (!el || !markReadOnScroll || !a || a.isRead) return;
    if (scrollMarkedRef.current === a.id) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 120) {
      scrollMarkedRef.current = a.id;
      actions.setRead(a.id, true);
    }
  }, [markReadOnScroll, a, actions]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    setScrolled(el.scrollTop > 8);
    markReadIfAtFoot();
  };

  // A short article that fits the viewport never fires `scroll`, so check the
  // foot condition once the body has laid out (article switch, extract toggle,
  // extraction finishing). The check is deferred briefly so body images have a
  // chance to load — measuring `scrollHeight` before they do could read a
  // too-small height and mark a genuinely long article read prematurely. The
  // `scrollMarkedRef` guard keeps it idempotent.
  useEffect(() => {
    const timer = window.setTimeout(markReadIfAtFoot, 400);
    return () => window.clearTimeout(timer);
  }, [markReadIfAtFoot, showExtracted, a?.extractedHtml, a?.contentHtml]);


  const copyLink = () => {
    if (!a?.url) return;
    navigator.clipboard.writeText(a.url).then(() => onToast(t("reader.linkCopied")), () => {});
  };
  const copyText = (text: string, toastKey: string) => {
    navigator.clipboard.writeText(text).then(() => onToast(t(toastKey)), () => {});
  };
  // Save a feed image to disk. Fetch and save both run in Rust (`save_image`):
  // the request's Referer can walk the same hotlink-protection fallbacks that
  // let these images render at all (see fetch_image), and the native save
  // dialog appears there too, so a large image never round-trips the webview.
  const saveImage = async (url: string) => {
    try {
      const saved = await api.saveImage(url, a?.url);
      if (saved) toast.show(t("reader.imageSaved"));
    } catch {
      toast.error(t("reader.imageSaveFailed"));
    }
  };
  const share = () => {
    if (!a?.url) return;
    if (navigator.share) {
      navigator.share({ title: a.title, url: a.url }).catch((e) => {
        // A user-cancelled share rejects with AbortError — only fall back to
        // copying the link on a genuine failure (e.g. share unsupported).
        if ((e as Error)?.name !== "AbortError") copyLink();
      });
    } else {
      copyLink();
    }
  };

  // Article-body clicks: an image opens the full-screen viewer (issue #87), with
  // the article's other images available for ← / → navigation; anything else
  // falls through to the link handler (in-page anchors, external links).
  const linkClick = makeLinkClickHandler(a?.url ?? null);
  const handleBodyClick = (e: React.MouseEvent) => {
    const img = (e.target as HTMLElement).closest("img") as HTMLImageElement | null;
    const root = bodyRef.current;
    if (img && root?.contains(img)) {
      // Use the src the DOM actually renders — a proxied data: URL when the
      // original hotlink-protected host needed a Referer — and skip hidden /
      // broken images so the gallery matches what the reader shows.
      const imgs = Array.from(
        root.querySelectorAll<HTMLImageElement>("img"),
      ).filter(
        (im) => im.style.display !== "none" && (im.currentSrc || im.getAttribute("src")),
      );
      const index = imgs.indexOf(img);
      if (index >= 0) {
        e.preventDefault();
        setLightbox({ srcs: imgs.map((im) => im.currentSrc || im.src), index });
        return;
      }
    }
    linkClick(e);
  };

  const backButton = (
    <button
      className="tb-btn reader-back"
      title={t("reader.backToList")}
      aria-label={t("reader.backToList")}
      onClick={() => {
        useUi.getState().openArticle(null);
        useUi.getState().setFocusMode(false);
        useUi.getState().setAiOpen(false);
      }}
    >
      <span className="reader-back-icon"><Icon name="chevron-right" size={16} /></span>
    </button>
  );

  if (id == null) {
    const kbd = {
      fontFamily: "var(--mono)",
      fontSize: 10,
      padding: "1px 5px",
      border: "1px solid var(--hair)",
      borderRadius: 3,
    };
    return (
      <div className="reader" role="main">
        {isMac && <div className="reader-toolbar" data-tauri-drag-region />}
        <div className="empty" style={{ flex: 1 }}>
          <div className="glyph">
            <Icon name="rss" size={22} />
          </div>
          <div>{t("reader.emptySelectArticle")}</div>
          <div style={{ fontSize: 11.5, color: "var(--muted-2)" }}>
            {t("reader.emptyHintPrefix")} <kbd style={kbd}>J</kbd> /{" "}
            <kbd style={kbd}>K</kbd> {t("reader.emptyHintSuffix")}
          </div>
        </div>
      </div>
    );
  }

  // An article is selected but its detail isn't loaded yet — still fetching
  // or the fetch failed. Surface that explicitly instead of falling through
  // to the "select an article" empty state, which would be misleading.
  if (!a) {
    return (
      <div className="reader" role="main">
        <div className="reader-toolbar reader-loading-toolbar" {...(isMac && { "data-tauri-drag-region": true })}>
          {backButton}
        </div>
        {article.isError ? (
          <div className="empty" style={{ flex: 1 }}>
            <div className="glyph">
              <Icon name="alert" size={22} />
            </div>
            <div>{t("reader.loadError")}</div>
            <button
              className="empty-retry"
              onClick={() => article.refetch()}
              disabled={article.isFetching}
            >
              <Icon name="refresh" size={12} />
              {t("common.retry")}
            </button>
          </div>
        ) : (
          <div className="reader-scroll">
            <div className="article reader-content" aria-hidden="true">
              <div className="sk-line" style={{ width: "28%" }} />
              <div
                className="sk-line"
                style={{ width: "82%", height: 24, marginBottom: 18 }}
              />
              <div
                className="sk-line"
                style={{ width: "44%", marginBottom: 30 }}
              />
              {Array.from({ length: 9 }).map((_, i) => (
                <div
                  key={i}
                  className="sk-line"
                  style={{ width: i % 3 === 2 ? "58%" : "100%", height: 12 }}
                />
              ))}
            </div>
          </div>
        )}
      </div>
    );
  }

  // `hasExtracted`, `canTranslate`, `body`, `displayBody` and the translation
  // state are computed above (before the early returns) so the image-proxy and
  // recovery effects can depend on them.

  // Translate into `lang` with `eng` and show the result. Defaults come from
  // Settings; the toolbar passes temporary overrides here (not persisted). Always
  // starts a fresh job (the store skips only a duplicate in-flight one).
  const run = (lang: string, eng: string) => {
    if (!canTranslate) return;
    startTranslate(a.id, lang, eng);
    setShowTranslation(true);
  };

  const ytId = a.sourceType === "youtube" ? youtubeId(a.url) : null;
  // Keep YouTube's privacy-enhanced host; Bilibili RSSHub items remain ordinary RSS.
  const playerUrl = ytId
    ? `https://www.youtube-nocookie.com/embed/${ytId}`
    : bilibiliPlayerUrl(a.url);

  return (
    <div className="reader" role="main">
      <div
        className={`reader-toolbar ${scrolled ? "scrolled" : ""}`}
        {...(isMac && { "data-tauri-drag-region": true })}
      >
        {backButton}
        <button
          className={`tb-btn ${a.isStarred ? "on" : ""}`}
          onClick={() => actions.setStarred(a.id, !a.isStarred)}
          title={t("reader.tbStar")}
          aria-label={t("reader.tbStar")}
          aria-pressed={a.isStarred}
        >
          <Icon name={a.isStarred ? "star-fill" : "star"} size={16} />
        </button>
        <button
          className={`tb-btn ${a.readLater ? "on" : ""}`}
          onClick={() => actions.setReadLater(a.id, !a.readLater)}
          title={t("reader.tbReadLater")}
          aria-label={t("reader.tbReadLater")}
          aria-pressed={a.readLater}
        >
          <Icon name={a.readLater ? "bookmark-fill" : "bookmark"} size={16} />
        </button>
        <button
          className={`tb-btn ${a.tags.length > 0 ? "on" : ""}`}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setTagPick((p) => (p ? null : { x: r.left, y: r.bottom + 6 }));
          }}
          title={t("reader.tbTags")}
          aria-label={t("reader.tbTags")}
          aria-haspopup="menu"
          aria-expanded={tagPick != null}
        >
          <Icon name="tag" size={16} />
        </button>
        <button
          className={`tb-btn ${hasExtracted && showExtracted ? "on" : ""} ${
            extract.isPending ? "spinning" : ""
          }`}
          onClick={() =>
            hasExtracted ? setShowExtracted((v) => !v) : extract.mutate(a.id)
          }
          // Extraction needs the source URL; without one (and nothing
          // extracted yet) the button can only error, so disable it.
          disabled={extract.isPending || (!hasExtracted && !a.url)}
          title={hasExtracted ? t("reader.tbToggleFullText") : t("reader.tbExtractFullText")}
          aria-label={hasExtracted ? t("reader.tbToggleFullText") : t("reader.tbExtractFullText")}
          aria-pressed={hasExtracted ? showExtracted : undefined}
          aria-busy={extract.isPending}
        >
          <Icon name="text" size={16} />
        </button>
        <button
          className="tb-btn"
          title={t("reader.tbShare")}
          aria-label={t("reader.tbShare")}
          onClick={share}
          disabled={!a.url}
        >
          <Icon name="share" size={16} />
        </button>
        <button
          className={`tb-btn ${showTranslation ? "on" : ""}`}
          title={t("reader.tbTranslate")}
          aria-label={t("reader.tbTranslate")}
          aria-pressed={showTranslation}
          disabled={!canTranslate}
          onClick={() =>
            // One click: translate with the default language + engine, or flip
            // back to the original. Switch engine/language inline on the toggle.
            showTranslation ? setShowTranslation(false) : run(targetLang, engine)
          }
        >
          <Icon name="globe" size={16} />
        </button>
        <HighlightLayer
          // Keyed by article id so the export action / popovers reset cleanly
          // when the reader switches articles.
          key={a.id}
          article={a}
          bodyRef={bodyRef}
          bodyVersion={displayBody}
        />
        <div className="tb-btn spacer" />
        {a.url && (
          <button
            className={`tb-btn ${viewMode === "web" ? "on" : ""}`}
            title={t("reader.tbWebView")}
            aria-label={t("reader.tbWebView")}
            aria-pressed={viewMode === "web"}
            onClick={() => setViewMode((v) => (v === "web" ? "reader" : "web"))}
          >
            <Icon name="eye" size={16} />
          </button>
        )}
        {a.url && (
          <button
            className="tb-btn"
            title={t("reader.tbOpenInBrowser")}
            aria-label={t("reader.tbOpenInBrowser")}
            onClick={() => openUrl(a.url!).catch(() => {})}
          >
            <Icon name="open" size={16} />
          </button>
        )}
      </div>

      {viewMode === "web" && a.url ? (
        <div className="reader-webview">
          <div className="reader-webview-bar">
            <span className="reader-webview-url">{a.url}</span>
            <button
              className="reader-webview-open"
              onClick={() => openUrl(a.url!).catch(() => {})}
            >
              {t("reader.tbOpenInBrowser")}
            </button>
          </div>
          {/* The native child webview (page_view.rs) floats over this host —
              it's not in the DOM, so this div only reserves the space. The
              effect below measures it and positions the webview to match. */}
          <div className="reader-webview-host" ref={pageHostRef} />
        </div>
      ) : (
      <div
        className="reader-scroll"
        ref={scrollRef}
        onScroll={onScroll}
        onContextMenu={(e) => {
          e.preventDefault();
          // Capture what the click landed on so the menu can add image- and
          // selection-specific actions (the native menu is suppressed app-wide;
          // see main.tsx).
          const img = (e.target as HTMLElement).closest("img") as HTMLImageElement | null;
          const sel = window.getSelection();
          const selection =
            sel && !sel.isCollapsed ? sel.toString().trim() : "";
          setCtxMenu({
            x: e.clientX,
            y: e.clientY,
            // data-papr-src holds the real address when the image was
            // recovered through the backend and src is an inline data: URL.
            imageUrl: img?.dataset.paprSrc || img?.currentSrc || img?.getAttribute("src") || undefined,
            selection: selection || undefined,
          });
        }}
      >
        <article className="article reader-content" key={a.id}>
          <button
            type="button"
            className="article-feed"
            title={t("reader.viewAllFromFeed")}
            onClick={() =>
              useUi.getState().select({ kind: "feed", value: a.feedId }, a.feedTitle)
            }
          >
            <Icon name="rss" size={13} />
            {a.feedTitle}
          </button>
          <h1 className="article-title">{a.title}</h1>
          <div className="article-meta">
            {a.author && <span className="author">{a.author}</span>}
            {a.author && a.publishedAt && <span>·</span>}
            {a.publishedAt && <span>{fullDate(a.publishedAt)}</span>}
            {showReadingTime && (
              <>
                <span>·</span>
                <span>{t("reader.readMinutes", { count: readMinutes })}</span>
              </>
            )}
            {extract.isPending && (
              <>
                <span>·</span>
                <span>{t("reader.extractingFullText")}</span>
              </>
            )}
          </div>

          {a.tags.length > 0 && (
            <div className="article-tags">
              {a.tags.map((tag) => (
                <button
                  key={tag.id}
                  className="article-tag"
                  style={{ "--tag-c": tagColor(tag.color) } as React.CSSProperties}
                  onClick={() =>
                    useUi.getState().select({ kind: "tag", value: tag.id }, tag.name)
                  }
                >
                  <span className="tag-dot" />
                  {tag.name}
                </button>
              ))}
            </div>
          )}

          {playerUrl ? (
            <iframe
              key={playerUrl}
              style={{ width: "100%", aspectRatio: "16 / 9" }}
              src={playerUrl}
              title={a.title}
              referrerPolicy="strict-origin-when-cross-origin"
              allowFullScreen
            />
          ) : (
            a.imageUrl &&
            // Skip the hero when the body already embeds the same image, so
            // feeds that repeat their lead image don't show it twice.
            !body.includes(a.imageUrl) &&
            // Also skip it when the body opens with its own image/video — the
            // article already leads with a visual, so a separate thumbnail on
            // top would just be a redundant (often mismatched) image (#97).
            !leadsWithMedia && (
              <ArticleImage
                key={a.id}
                className="reader-hero"
                src={a.imageUrl}
                pageUrl={a.url}
                alt=""
              />
            )
          )}

          {a.enclosures
            .filter((e) => e.mimeType?.startsWith("audio"))
            .map((e, i) => {
              const isPlaying = playingSrc === e.url;
              return (
                <button
                  className={`episode ${isPlaying ? "playing" : ""}`}
                  key={`a${i}`}
                  onClick={() =>
                    playTrack({
                      articleId: a.id,
                      title: a.title,
                      feedTitle: a.feedTitle,
                      src: e.url,
                    })
                  }
                >
                  <span className="episode-play">
                    <Icon name={isPlaying ? "pause" : "play"} size={15} />
                  </span>
                  <span className="episode-text">
                    {isPlaying
                      ? t("reader.episodePlaying")
                      : t("reader.episodePlay")}
                  </span>
                </button>
              );
            })}
          {a.enclosures
            .filter((e) => e.mimeType?.startsWith("video"))
            .map((e, i) => (
              <div className="enclosure" key={`v${i}`}>
                <video controls src={e.url} />
              </div>
            ))}

          {showToggle && (
            <div className="tr-toggle" role="group" aria-label={t("reader.tbTranslate")}>
              <button
                className={!showTranslation ? "on" : ""}
                aria-pressed={!showTranslation}
                onClick={() => setShowTranslation(false)}
              >
                {t("reader.original")}
              </button>
              <button
                className={showTranslation ? "on" : ""}
                aria-pressed={showTranslation}
                onClick={() => setShowTranslation(true)}
              >
                {t("reader.translation")}
              </button>
              {/* Temporary per-article switchers — change engine or language for
                  this translation only, without touching the configured defaults;
                  switching re-translates with the new choice straight away. */}
              <select
                className="s-select tr-sel"
                value={engine}
                aria-label={t("reader.translateEngine")}
                onChange={(e) => {
                  setTmpEngine(e.target.value);
                  run(targetLang, e.target.value);
                }}
              >
                <option value="llm">{t("reader.translateEngineLlm")}</option>
                <option value="google">Google</option>
                <option value="deepl">DeepL</option>
                <option value="bing">Bing</option>
              </select>
              <select
                className="s-select tr-sel"
                value={targetLang}
                aria-label={t("reader.translateTitle")}
                onChange={(e) => {
                  setTmpLang(e.target.value);
                  run(e.target.value, engine);
                }}
              >
                {LANGUAGES.map((l) => (
                  <option key={l.code} value={l.code}>
                    {l.label}
                  </option>
                ))}
              </select>
              {translating && (
                <span className="tr-progress">
                  {t("reader.translating")}
                  {jobForTarget && jobForTarget.total > 0 &&
                    ` ${jobForTarget.done}/${jobForTarget.total}`}
                </span>
              )}
            </div>
          )}

          <div
            className="article-body"
            ref={bodyRef}
            onClick={handleBodyClick}
            dangerouslySetInnerHTML={bodyMarkup}
          />
        </article>
      </div>
      )}

      {lightbox && (
        <Lightbox
          srcs={lightbox.srcs}
          index={lightbox.index}
          onClose={() => setLightbox(null)}
        />
      )}

      <AIDrawer
        // Keyed by article id so switching articles remounts the drawer:
        // its `text` state then re-initialises from the new article's
        // summary, rather than carrying the previous one's across.
        key={a.id}
        open={aiOpen}
        article={a}
        onClose={() => setAiOpen(false)}
      />

      {tagPick && (
        <TagPicker
          articleId={a.id}
          attached={a.tags.map((tg) => tg.id)}
          x={tagPick.x}
          y={tagPick.y}
          onClose={() => setTagPick(null)}
        />
      )}

      {ctxMenu && (
        <ContextMenu
          x={ctxMenu.x}
          y={ctxMenu.y}
          items={[
            ...(ctxMenu.selection
              ? [
                  {
                    icon: "copy" as const,
                    label: t("reader.ctxCopy"),
                    onClick: () => copyText(ctxMenu.selection!, "reader.textCopied"),
                  },
                ]
              : []),
            ...(ctxMenu.imageUrl
              ? [
                  {
                    icon: "arrow-down" as const,
                    label: t("reader.ctxSaveImage"),
                    onClick: () => saveImage(ctxMenu.imageUrl!),
                  },
                  {
                    icon: "copy" as const,
                    label: t("reader.ctxCopyImageAddress"),
                    onClick: () =>
                      copyText(ctxMenu.imageUrl!, "reader.imageAddressCopied"),
                  },
                ]
              : []),
            ...(ctxMenu.selection || ctxMenu.imageUrl
              ? [{ separator: true as const }]
              : []),
            {
              icon: aiOpen ? "sparkle-fill" : "sparkle",
              label: t("reader.tbAiSummary"),
              onClick: () => setAiOpen(!aiOpen),
            },
            ...(canTranslate
              ? [
                  {
                    icon: "globe",
                    label: showTranslation
                      ? t("reader.tbShowOriginal")
                      : t("reader.tbTranslate"),
                    onClick: () =>
                      showTranslation
                        ? setShowTranslation(false)
                        : run(targetLang, engine),
                  },
                ]
              : []),
            { separator: true },
            ...(a.url
              ? [{ icon: "copy", label: t("reader.tbCopyLink"), onClick: copyLink }]
              : []),
            { separator: true },
            {
              icon: focusMode ? "eye-off" : "focus",
              label: t("reader.tbFocusMode"),
              onClick: () => setFocusMode(!focusMode),
            },
          ] as MenuEntry[]}
          onClose={() => setCtxMenu(null)}
        />
      )}
    </div>
  );
}

function AIDrawer({
  open,
  article,
  onClose,
}: {
  open: boolean;
  article: ArticleDetail;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const aiWidth = useUi((s) => s.aiWidth);
  // Initialised from the article's stored summary (if any). The parent keys
  // this component by article id, so a switch remounts it and re-runs this
  // initialiser — no separate "reset on article change" effect is needed.
  const [text, setText] = useState<string | null>(article.aiSummary);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [retry, setRetry] = useState(0);
  const runRef = useRef(0);
  useEffect(() => () => { ++runRef.current; }, []);

  useEffect(() => {
    if (!open || busy || text || failed) return;
    const run = ++runRef.current;
    let sawErrorEvent = false;
    setBusy(true);
    setText("");
    // Hiding the drawer keeps this request alive: reopening resumes the same
    // summary without another provider request or a blank intermediate state.
    api.aiSummarize(article.id, (ev) => {
      if (runRef.current !== run) return;
      if (ev.type === "delta") setText((s) => (s ?? "") + ev.data);
      else if (ev.type === "error") {
        sawErrorEvent = true;
        setFailed(true);
        toast.error(ev.data);
      }
    })
      .then(() => {
        qc.invalidateQueries({ queryKey: ["article", article.id] });
      })
      .catch((e) => {
        if (runRef.current === run && !sawErrorEvent) {
          setFailed(true);
          reportError(e);
        }
      })
      .finally(() => { if (runRef.current === run) setBusy(false); });
    // Busy/text are guards, not triggers: a stream delta must not start a run.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, article.id, retry]);

  const loading = busy && !text;
  const onRetry = () => {
    setText("");
    setFailed(false);
    setRetry((n) => n + 1);
  };
  // Parse + sanitize the summary only when the text changes, not on every
  // AIDrawer re-render (e.g. each open/close toggle).
  const html = useMemo(() => (text ? renderMarkdown(text) : ""), [text]);

  return (
    <div
      className={`ai-drawer ${open ? "open" : ""}`}
      // A labelled complementary landmark so screen-reader users can jump
      // straight to the summary.
      role="complementary"
      aria-label={t("reader.aiSummaryTitle")}
      // When closed the drawer is only moved off-screen — `inert` keeps its
      // close button and content out of the tab order and the a11y tree.
      inert={!open}
    >
      {/* Left-edge handle: dragging left widens the drawer. Hidden from the
          a11y tree while the drawer is closed (the whole drawer is `inert`). */}
      <div className="resize-handle-slot resize-handle-slot--inline">
        <ResizeHandle
          width={aiWidth}
          side="left"
          min={PANEL_BOUNDS.ai.min}
          max={PANEL_BOUNDS.ai.max}
          onResize={(w) => useUi.getState().setPanel({ aiWidth: w })}
          label={t("reader.resizeAi")}
        />
      </div>
      <div className="ai-head">
        <span className="accent-ico">
          <Icon name="sparkle-fill" size={15} />
        </span>
        <h3>{t("reader.aiSummaryTitle")}</h3>
        <button
          className="tb-btn close"
          onClick={onClose}
          title={t("common.close")}
          aria-label={t("common.close")}
        >
          <Icon name="x" size={14} />
        </button>
      </div>
      <div className="ai-body" aria-live="polite" aria-busy={busy}>
        {loading && (
          <div className="ai-loading">
            <span className="ai-dot" />
            <span className="ai-dot" />
            <span className="ai-dot" />
            <span style={{ marginLeft: 4 }}>{t("reader.aiReadingFullText")}</span>
          </div>
        )}
        {failed && !busy && (
          <div className="ai-error">
            <Icon name="alert" size={18} />
            <span>{t("reader.aiError")}</span>
            <button className="empty-retry" onClick={onRetry}>
              <Icon name="refresh" size={12} />
              {t("common.retry")}
            </button>
          </div>
        )}
        {text && !failed && (
          <>
            <div
              className="ai-prose"
              onClick={makeLinkClickHandler(article.url)}
              dangerouslySetInnerHTML={{ __html: html }}
            />
            <div
              style={{
                fontSize: 11,
                color: "var(--muted-2)",
                marginTop: 24,
                lineHeight: 1.5,
              }}
            >
              {t("reader.aiDisclaimer")}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
