//! HTML sanitization and text extraction. Every piece of feed- or web-supplied
//! HTML passes through `sanitize` before it is ever stored or rendered.

use ammonia::{Builder, UrlRelative};
use ego_tree::iter::Edge;
use lol_html::{element, rewrite_str, RewriteStrSettings};
use scraper::node::Node;
use scraper::{Html, Selector};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::LazyLock;
use url::Url;

/// Sanitize untrusted HTML for safe rendering inside the reader webview.
/// Relative URLs are rewritten against `base` so feed images/links resolve.
pub fn sanitize(html: &str, base: Option<&str>) -> String {
    // Recover lazy-loaded image URLs before ammonia runs: feeds that lazy-load
    // (少数派/sspai, WeChat, many CMSes) put the real URL in `data-src`/`srcset`
    // and leave `src` empty or a placeholder, and ammonia's default whitelist
    // drops those attributes — so without this the `<img>` reaches the reader
    // with no usable `src` and silently shows nothing. See `promote_lazy_images`.
    let html = promote_lazy_images(html);

    let mut builder = Builder::default();
    builder
        .link_rel(Some("noopener noreferrer nofollow"))
        .add_generic_attributes(["loading"])
        // Allow inline HTML5 video. ammonia's default whitelist drops <video>
        // and <source> entirely, so a feed that embeds a clip with a <video>
        // tag reached the reader as nothing at all. Permit the element plus its
        // safe layout/playback attributes (and <source> for multiple formats);
        // `src`/`poster` are URL attributes, so they go through the same
        // relative-URL rewriting and scheme filtering as every other URL here,
        // which strips `javascript:` and friends. `autoplay` is deliberately
        // not whitelisted — feed-embedded video must be user-initiated.
        .add_tags(["video", "source"])
        .add_tag_attributes(
            "video",
            [
                "src",
                "poster",
                "width",
                "height",
                "preload",
                "loop",
                "muted",
                "playsinline",
            ],
        )
        .add_tag_attributes("source", ["src", "type", "media"])
        // Always expose player controls: a feed clip with no `controls` (and no
        // whitelisted `autoplay`) would otherwise be a frozen, unplayable frame.
        .set_tag_attribute_value("video", "controls", "")
        // Load every feed image without a `Referer`. Common image hosts —
        // notably Sina's `*.sinaimg.cn` CDN, which backs 喷嚏图卦 and many
        // Weibo-sourced feeds — hotlink-protect by Referer: a request carrying
        // the reader's own origin is 403'd, while one with no Referer is
        // served. Without this the image silently fails to load (and the
        // reader then hides the broken `<img>`), so the article looks
        // text-only. Forcing the attribute on every `<img>` also overrides any
        // weaker policy the feed shipped. Hosts that instead *require* a
        // Referer (e.g. `cdnfile.sspai.com`) are covered by the reader's
        // retry-through-backend path — see `commands::fetch_image`.
        .set_tag_attribute_value("img", "referrerpolicy", "no-referrer");

    let parsed_base = base.and_then(|b| Url::parse(b).ok());
    if let Some(b) = parsed_base {
        builder.url_relative(UrlRelative::RewriteWithBase(b));
    }
    builder.clean(&html).to_string()
}

/// Promote a lazy-loaded image's real URL into `src` so it survives `sanitize`.
/// Lazy-loading feeds ship `<img src="" data-src="https://…">` (or a tiny
/// `data:` placeholder in `src`, or only a `srcset`); ammonia's default img
/// whitelist keeps `src` but drops `data-*`/`srcset`, which would leave an
/// `<img>` with nothing to load. Filling `src` here lets the recovered URL flow
/// through the rest of the pipeline unchanged — relative-URL rewriting, the
/// forced `no-referrer` policy, the webview load, and the `fetch_image` Referer
/// fallback that handles hosts like `cdnfile.sspai.com`. Runs before `clean`,
/// so ammonia still has the final say on safety; a rewrite failure falls back
/// to the original HTML.
fn promote_lazy_images(html: &str) -> String {
    let pictures = Rc::new(RefCell::new(Vec::<Option<String>>::new()));
    let picture_state = pictures.clone();
    let source_state = pictures.clone();
    let picture = element!("picture", move |el: &mut lol_html::html_content::Element| {
        picture_state.borrow_mut().push(None);
        let state = picture_state.clone();
        if let Some(handlers) = el.end_tag_handlers() {
            handlers.push(Box::new(move |_| {
                state.borrow_mut().pop();
                Ok(())
            }));
        }
        Ok(())
    });
    let source = element!("picture source", move |el| {
        if let Some(candidate) = source_state.borrow_mut().last_mut() {
            if candidate.is_none() {
                *candidate = ["data-srcset", "srcset"]
                    .iter()
                    .filter_map(|a| el.get_attribute(a))
                    .find_map(|s| image_from_srcset(&s));
            }
        }
        Ok(())
    });
    let image = element!("img", move |el| {
        let has_real_src = el
            .get_attribute("src")
            .and_then(|s| image_candidate(&s))
            .is_some();
        if !has_real_src {
            let recovered = [
                "data-src",
                "data-original",
                "data-actualsrc",
                "data-lazy-src",
            ]
            .iter()
            .filter_map(|a| el.get_attribute(a))
            .find_map(|s| image_candidate(&s))
            .or_else(|| {
                ["data-srcset", "srcset"]
                    .iter()
                    .filter_map(|a| el.get_attribute(a))
                    .find_map(|s| image_from_srcset(&s))
            })
            .or_else(|| pictures.borrow().last().cloned().flatten());
            if let Some(url) = recovered {
                let _ = el.set_attribute("src", &url);
            }
        }
        Ok(())
    });
    rewrite_str(
        html,
        RewriteStrSettings {
            element_content_handlers: vec![picture, source, image],
            ..Default::default()
        },
    )
    .unwrap_or_else(|_| html.to_string())
}

/// Only recover plausible web images; ammonia still validates the final URL.
fn image_candidate(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('#') {
        return None;
    }
    let parsed = Url::parse(value)
        .or_else(|_| Url::parse("https://relative.invalid/").unwrap().join(value))
        .ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    // Restrict placeholder detection to common filenames, not arbitrary URL substrings.
    let filename = parsed
        .path()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let stem = filename.split('.').next().unwrap_or("");
    if [
        "placeholder",
        "image-placeholder",
        "placeholder-image",
        "spacer",
        "blank",
        "transparent",
        "pixel",
        "1x1",
        "loading",
        "lazy",
    ]
    .contains(&stem)
    {
        return None;
    }
    Some(value.to_string())
}

fn image_from_srcset(srcset: &str) -> Option<String> {
    let mut tokens = srcset.split_ascii_whitespace();
    while let Some(token) = tokens.next() {
        // Commas within a URL (including data URLs) belong to that URL. A trailing
        // comma or the comma after a descriptor separates candidates.
        if let Some(url) = image_candidate(token.trim_end_matches(',')) {
            return Some(url);
        }
        if !token.ends_with(',') {
            for descriptor in tokens.by_ref() {
                if descriptor.ends_with(',') {
                    break;
                }
            }
        }
    }
    None
}

/// Tags whose text content is dropped wholesale (it isn't human-readable copy).
const SKIP_TAGS: &[&str] = &["script", "style", "template", "noscript"];

/// Block-level tags: their edges are word boundaries, so text on either side
/// must not be allowed to run together (`</h1><p>` → "TitleBody").
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "caption",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

/// Strip all markup from HTML, yielding collapsed plain text. Used for the
/// FTS body index, list snippets, and AI prompt context.
///
/// Parsing into a DOM (rather than letting ammonia concatenate text nodes)
/// gets two things right that a plain tag-strip does not: HTML entities are
/// decoded (`&amp;` → `&`), and a space is emitted at every block boundary so
/// adjacent paragraphs/headings keep their words apart — while inline tags
/// (`un<b>der</b>line`) still join seamlessly. The traversal is iterative, so
/// pathologically deep markup can't overflow the stack.
pub fn html_to_text(html: &str) -> String {
    let frag = Html::parse_fragment(html);
    let mut out = String::new();
    // Depth of the current script/style/etc. subtree — text is dropped while
    // this is non-zero.
    let mut skip = 0u32;
    for edge in frag.tree.root().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Element(el) => {
                    let name = el.name();
                    if SKIP_TAGS.contains(&name) {
                        skip += 1;
                    } else if skip == 0 && BLOCK_TAGS.contains(&name) {
                        out.push(' ');
                    }
                }
                Node::Text(t) if skip == 0 => out.push_str(t),
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(el) = node.value() {
                    let name = el.name();
                    if SKIP_TAGS.contains(&name) {
                        skip = skip.saturating_sub(1);
                    } else if skip == 0 && BLOCK_TAGS.contains(&name) {
                        out.push(' ');
                    }
                }
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

static IMG_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("img").expect("img is a valid selector"));

/// The first usable image URL embedded in a block of (already-sanitized) HTML.
/// Used as a card-thumbnail fallback when the feed ships no media thumbnail —
/// many feeds put the lead image only as an `<img>` in the entry body. Because
/// `sanitize` has already rewritten relative URLs against the feed base, a
/// non-absolute `src` left here is unresolvable, and a `data:` blob is an
/// inline pixel rather than a real thumbnail; both are skipped.
pub fn first_image(html: &str) -> Option<String> {
    let frag = Html::parse_fragment(html);
    frag.select(&IMG_SELECTOR).find_map(|el| {
        let src = el.value().attr("src")?.trim();
        (src.starts_with("http://") || src.starts_with("https://")).then(|| src.to_string())
    })
}

/// Gallery images from sanitized article HTML, in reading order without duplicates.
pub fn image_urls(html: &str) -> Vec<String> {
    let frag = Html::parse_fragment(html);
    let mut seen = std::collections::HashSet::new();
    frag.select(&IMG_SELECTOR)
        .filter(|el| {
            !["width", "height"].iter().any(|attr| {
                el.value().attr(attr).and_then(|s| s.parse::<u32>().ok()).is_some_and(|n| n <= 2)
            })
        })
        .filter_map(|el| {
            let src = el.value().attr("src")?.trim();
            let url = Url::parse(src).ok()?;
            (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
                && seen.insert(src.to_string())).then(|| src.to_string())
        })
        .collect()
}

/// HTML-escape a string for safe interpolation into element text or an
/// attribute value. Escapes the five characters that can break out of either
/// context (`& < > " '`).
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- html_to_text behaviours, each pinned with a regression test. ---

    #[test]
    fn block_boundaries_keep_words_apart() {
        // `</h1><p>` must not collapse to "TitleBody".
        assert_eq!(html_to_text("<h1>Title</h1><p>Body</p>"), "Title Body");
    }

    #[test]
    fn inline_tags_do_not_break_words() {
        // `un<b>der</b>line` is one word — inline edges add no space.
        assert_eq!(html_to_text("un<b>der</b>line"), "underline");
    }

    #[test]
    fn html_entities_are_decoded() {
        assert_eq!(html_to_text("<p>Tom &amp; Jerry</p>"), "Tom & Jerry");
        assert_eq!(html_to_text("caf&#233;"), "café");
        assert_eq!(html_to_text("a &lt;tag&gt; b"), "a <tag> b");
    }

    #[test]
    fn script_and_style_content_is_dropped() {
        assert_eq!(
            html_to_text("<p>hi</p><script>alert(1)</script><style>x{}</style>"),
            "hi"
        );
    }

    #[test]
    fn whitespace_is_collapsed() {
        assert_eq!(
            html_to_text("<p>  lots   of\n\n  space </p>"),
            "lots of space"
        );
    }

    #[test]
    fn empty_and_plain_input() {
        assert_eq!(html_to_text(""), "");
        assert_eq!(html_to_text("just text"), "just text");
    }

    #[test]
    fn deeply_nested_markup_does_not_overflow() {
        // The traversal is iterative; 5000 nested divs must not blow the stack.
        let deep = format!("{}word{}", "<div>".repeat(5000), "</div>".repeat(5000));
        assert_eq!(html_to_text(&deep), "word");
    }

    // --- sanitize: the XSS boundary for all feed-supplied HTML. ---

    #[test]
    fn sanitize_strips_scripts_and_event_handlers() {
        let out = sanitize("<p onclick=\"steal()\">hi</p><script>evil()</script>", None);
        assert!(!out.contains("script"), "script tag survived: {out}");
        assert!(!out.contains("onclick"), "event handler survived: {out}");
        assert!(out.contains("hi"));
    }

    #[test]
    fn sanitize_adds_rel_to_links() {
        let out = sanitize("<a href=\"https://example.com\">x</a>", None);
        assert!(out.contains("noopener"), "link rel missing: {out}");
    }

    // --- video: inline HTML5 video survives sanitize (issue #39). ---

    #[test]
    fn sanitize_keeps_video_and_source() {
        let out = sanitize(
            r#"<video poster="https://ex.com/p.jpg"><source src="https://ex.com/v.mp4" type="video/mp4"></video>"#,
            None,
        );
        assert!(out.contains("<video"), "video tag dropped: {out}");
        assert!(out.contains("<source"), "source tag dropped: {out}");
        assert!(out.contains(r#"src="https://ex.com/v.mp4""#), "source src dropped: {out}");
        assert!(out.contains(r#"type="video/mp4""#), "source type dropped: {out}");
        assert!(out.contains(r#"poster="https://ex.com/p.jpg""#), "poster dropped: {out}");
    }

    #[test]
    fn sanitize_forces_video_controls() {
        // A clip with no controls (and no autoplay) is an unplayable frozen
        // frame — controls are forced on so the reader can always play it.
        let out = sanitize(r#"<video src="https://ex.com/v.mp4"></video>"#, None);
        assert!(out.contains("controls"), "controls not forced: {out}");
    }

    #[test]
    fn sanitize_strips_video_autoplay_and_handlers() {
        // autoplay isn't whitelisted (feed video must be user-initiated), and
        // event handlers are stripped like everywhere else.
        let out = sanitize(
            r#"<video src="https://ex.com/v.mp4" autoplay onerror="evil()"></video>"#,
            None,
        );
        assert!(!out.contains("autoplay"), "autoplay survived: {out}");
        assert!(!out.contains("onerror"), "event handler survived: {out}");
    }

    #[test]
    fn sanitize_rewrites_relative_video_src_against_base() {
        let out = sanitize(
            r#"<video src="/clip.mp4"></video>"#,
            Some("https://example.com/post/"),
        );
        assert!(
            out.contains("https://example.com/clip.mp4"),
            "relative video src not rewritten: {out}"
        );
    }

    #[test]
    fn sanitize_drops_javascript_video_src() {
        let out = sanitize(r#"<video src="javascript:evil()"></video>"#, None);
        assert!(!out.contains("javascript:"), "javascript: src survived: {out}");
    }

    #[test]
    fn gallery_images_keep_reading_order_and_skip_duplicates_and_pixels() {
        assert_eq!(image_urls(r#"<img src="https://ex.com/a.jpg">
            <img src="https://ex.com/a.jpg"><img src="javascript:alert(1)">
            <img src="/relative.jpg"><img src="data:image/png;base64,AAAA">
            <img width="1" src="https://ex.com/track.gif">
            <img height="2" src="https://ex.com/pixel.gif">
            <img src="https://ex.com/b.jpg?x=1&amp;y=2">"#),
            vec!["https://ex.com/a.jpg", "https://ex.com/b.jpg?x=1&y=2"]);
        assert!(image_urls("<p>No images</p>").is_empty());
    }

    // --- first_image: card-thumbnail fallback from body HTML. ---

    #[test]
    fn first_image_returns_the_first_absolute_img() {
        let html =
            r#"<p>intro</p><img src="https://ex.com/a.png"><img src="https://ex.com/b.png">"#;
        assert_eq!(first_image(html).as_deref(), Some("https://ex.com/a.png"));
    }

    #[test]
    fn first_image_skips_unusable_sources() {
        // A leftover relative src can't resolve (sanitize would have made real
        // ones absolute); a data: blob is an inline pixel, not a thumbnail.
        assert_eq!(first_image(r#"<img src="/local.png">"#), None);
        assert_eq!(
            first_image(r#"<img src="data:image/png;base64,AAAA">"#),
            None
        );
        assert_eq!(first_image("<p>no images here</p>"), None);
        assert_eq!(first_image(""), None);
    }

    #[test]
    fn first_image_falls_through_relative_to_next_absolute() {
        let html = r#"<img src="/rel.png"><img src="https://ex.com/real.jpg">"#;
        assert_eq!(
            first_image(html).as_deref(),
            Some("https://ex.com/real.jpg")
        );
    }

    #[test]
    fn sanitize_marks_images_no_referrer() {
        // Hotlink-protected hosts (e.g. *.sinaimg.cn behind 喷嚏图卦) 403 a
        // request that carries the reader's origin as Referer; `no-referrer`
        // is what makes the image load.
        let out = sanitize(r#"<img src="https://wx1.sinaimg.cn/large/a.jpg">"#, None);
        assert!(
            out.contains(r#"referrerpolicy="no-referrer""#),
            "img missing no-referrer policy: {out}"
        );
    }

    #[test]
    fn sanitize_overrides_weaker_image_referrer_policy() {
        // A feed shipping its own (Referer-leaking) policy must not win.
        let out = sanitize(
            r#"<img src="https://wx1.sinaimg.cn/large/a.jpg" referrerpolicy="origin">"#,
            None,
        );
        assert!(out.contains(r#"referrerpolicy="no-referrer""#), "{out}");
        assert!(!out.contains(r#"referrerpolicy="origin""#), "{out}");
    }

    #[test]
    fn sanitize_rewrites_relative_urls_against_base() {
        let out = sanitize("<img src=\"/pic.png\">", Some("https://example.com/post/"));
        assert!(
            out.contains("https://example.com/pic.png"),
            "relative URL not rewritten: {out}"
        );
    }

    // --- promote_lazy_images: recover lazy-loaded <img> URLs before sanitize. ---

    #[test]
    fn promotes_data_src_to_src() {
        // sspai and many CMSes ship the real URL in data-src with src empty;
        // ammonia would otherwise drop data-src, leaving an unloadable <img>.
        let out = sanitize(
            r#"<img src="" data-src="https://cdnfile.sspai.com/a.jpg">"#,
            None,
        );
        assert!(
            out.contains(r#"src="https://cdnfile.sspai.com/a.jpg""#),
            "data-src not promoted: {out}"
        );
    }

    #[test]
    fn promotes_srcset_first_candidate_when_no_src() {
        let out = sanitize(
            r#"<img srcset="https://ex.com/a.jpg 1x, https://ex.com/b.jpg 2x">"#,
            None,
        );
        assert!(
            out.contains(r#"src="https://ex.com/a.jpg""#),
            "srcset not promoted: {out}"
        );
    }

    #[test]
    fn promotes_over_data_placeholder_src() {
        // A 1px data: placeholder in src must not block promotion.
        let out = sanitize(
            r#"<img src="data:image/gif;base64,AAAA" data-original="https://ex.com/real.jpg">"#,
            None,
        );
        assert!(out.contains(r#"src="https://ex.com/real.jpg""#), "{out}");
        assert!(
            !out.contains("data:image/gif"),
            "placeholder survived: {out}"
        );
    }

    #[test]
    fn real_src_is_not_overwritten_by_data_src() {
        let out = sanitize(
            r#"<img src="https://ex.com/real.jpg" data-src="https://ex.com/other.jpg">"#,
            None,
        );
        assert!(out.contains(r#"src="https://ex.com/real.jpg""#), "{out}");
        assert!(
            !out.contains("other.jpg"),
            "data-src wrongly overrode src: {out}"
        );
    }

    #[test]
    fn lazy_candidates_replace_only_recognized_http_placeholders() {
        for placeholder in [
            "https://ex.com/assets/placeholder.gif?v=1",
            "/images/spacer.png",
            "//ex.com/blank.gif",
        ] {
            let html =
                format!(r#"<img src="{placeholder}" data-src="" data-original="../real.jpg">"#);
            let out = sanitize(&html, Some("https://example.com/posts/story/"));
            assert_eq!(
                first_image(&out).as_deref(),
                Some("https://example.com/posts/real.jpg"),
                "{out}"
            );
        }
        for real in [
            "https://ex.com/loading-a-truck.jpg",
            "/photos/placeholder-art.jpg",
        ] {
            let out = sanitize(
                &format!(r#"<img src="{real}" data-original="https://ex.com/other.jpg">"#),
                Some("https://example.com/"),
            );
            assert!(!out.contains("other.jpg"), "{out}");
        }
    }

    #[test]
    fn srcset_skips_unusable_candidates_and_preserves_url_commas() {
        for attribute in ["data-srcset", "srcset"] {
            let out = sanitize(
                &format!(
                    r#"<img data-src="javascript:evil()" {attribute}="data:image/gif;base64,AAAA 1x, /placeholder.gif 2x, //cdn.example.com/image,large.jpg 3x">"#
                ),
                Some("https://example.com/post/"),
            );
            assert_eq!(
                first_image(&out).as_deref(),
                Some("https://cdn.example.com/image,large.jpg"),
                "{out}"
            );
            assert!(!out.contains("javascript:"), "{out}");
        }
    }

    #[test]
    fn picture_sources_supply_missing_images_without_leaking_to_other_images() {
        let out = sanitize(
            r#"
            <picture><source srcset="javascript:evil() 1x"><source data-srcset="/first.webp 1x"><img src="/blank.gif" onerror="evil()"></picture>
            <img><video><source src="/movie.mp4"></video>
            <picture><source srcset="/second.webp 1x"><img src="/real.jpg"></picture>
            <picture><source srcset="/third.webp 1x"><img></picture>"#,
            Some("https://example.com/posts/"),
        );
        let doc = Html::parse_fragment(&out);
        let sources: Vec<_> = doc
            .select(&IMG_SELECTOR)
            .map(|el| el.value().attr("src").map(str::to_string))
            .collect();
        assert_eq!(
            sources,
            vec![
                Some("https://example.com/first.webp".into()),
                None,
                Some("https://example.com/real.jpg".into()),
                Some("https://example.com/third.webp".into())
            ]
        );
        assert!(
            out.contains(r#"src="https://example.com/movie.mp4""#),
            "{out}"
        );
        assert!(
            !out.contains("onerror") && !out.contains("javascript:"),
            "{out}"
        );
    }

    #[test]
    fn recovered_images_cannot_bypass_sanitization() {
        let out = sanitize(
            r#"<img src="javascript:evil()" data-src="JAVASCRIPT:evil()" data-original=" /safe.jpg?x=1&amp;y=2 " onload="evil()"><img data-src="data:text/html,evil"><img data-src="vbscript:evil()">"#,
            Some("https://example.com/"),
        );
        assert_eq!(
            first_image(&out).as_deref(),
            Some("https://example.com/safe.jpg?x=1&y=2"),
            "{out}"
        );
        assert!(
            !out.contains("onload")
                && !out.contains("javascript:")
                && !out.contains("JAVASCRIPT:")
                && !out.contains("vbscript:")
                && !out.contains("data:text"),
            "{out}"
        );
    }

    #[test]
    fn promoted_image_still_gets_no_referrer() {
        // The recovered src must still flow through the no-referrer policy that
        // hotlink-protected hosts depend on.
        let out = sanitize(r#"<img data-src="https://cdnfile.sspai.com/a.jpg">"#, None);
        assert!(out.contains(r#"referrerpolicy="no-referrer""#), "{out}");
        assert!(out.contains("cdnfile.sspai.com/a.jpg"), "{out}");
    }
}
