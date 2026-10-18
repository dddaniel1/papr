# UI regression checks

Scroll regression: open `/tests/imageScroll.html`, then run
`await window.runImageScrollCheck()`. A React scroll-state update must preserve
the recovered image node, its data URL, and decoded pixels.


Image recovery checks: open `http://localhost:1430/tests/images.html` with the
same dev server, then run `await window.imageChecks.runChecks()` in the console.
This uses mocked image IPC to cover lazy/relative URLs, failed-image actions,
manual retry, request deduplication, and ignoring late results after cleanup.
Reload before rerunning. Native image downloads are covered separately by Rust
HTTP fixture tests; actual source compatibility needs a desktop smoke test.


Run `pnpm dev`, open `http://localhost:1430/tests/ui.html`, and run
`await window.paprTest.runChecks()` in the browser console. Reload before rerunning.
The page replaces Tauri IPC with in-memory fixtures: no real subscriptions,
settings, AI calls or sync credentials are used. The normal production build
includes only the app entry point.

The checks cover onboarding, keyboard-focusable settings, failed setting saves,
refresh outcomes, pagination retry, highlight Markdown export (including cancel
and failure), RSSHub Bilibili player rendering and cleanup, reopening an in-flight
summary, and streamed AI questions and briefings. Resize this page to inspect the
same UI at desktop
and minimum window sizes. Live provider and OS integration still need a desktop
smoke test.
