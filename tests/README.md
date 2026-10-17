# UI regression checks

Run `pnpm dev`, open `http://localhost:1430/tests/ui.html`, and run
`await window.paprTest.runChecks()` in the browser console. Reload before rerunning.
The page replaces Tauri IPC with in-memory fixtures: no real subscriptions,
settings, AI calls or sync credentials are used. The normal production build
includes only the app entry point.

The checks cover onboarding, keyboard-focusable settings, failed setting saves,
refresh outcomes, pagination retry, reopening an in-flight summary, and streamed
AI questions and briefings. Resize this page to inspect the same UI at desktop
and minimum window sizes. Live provider and OS integration still need a desktop
smoke test.
