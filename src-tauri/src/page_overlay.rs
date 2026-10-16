//! Linux/GTK-only shim that makes the in-app original-page child webview
//! (page_view.rs) genuinely *overlay* the reading area, matching macOS/Windows.
//!
//! Why this exists
//! ---------------
//! On Linux, Tauri/wry parents a child webview to the window's **vertical
//! `gtk::Box`** — `tauri-runtime-wry::create_webview` calls
//! `webview_builder.build_gtk(window.default_vbox())` — so the page stacks as a
//! second pane instead of floating over the reader, and `Webview::set_position/
//! set_size` are no-ops unless wry recognizes the child container. This is a long-standing upstream gap (tauri-apps/tauri#10420,
//! still open; fixes #15704 and wry#1745 unmerged as of tauri 2.12.x). We work
//! around it with public API only: build a `gtk::Layout` filling the window,
//! move the main webview into it, and parent the page-view webview to the same
//! Layout, positioning it with `Layout::move_` (which survives GTK relayout
//! passes, unlike a bare size-allocate). See the approach sketched upstream in
//! tauri#10420.
//!
//! Threading: GTK objects are `!Send` and main-thread-only. `Webview::with_webview`
//! yields the platform handle on the GTK main thread but demands a `Send`
//! closure, so the Layout is kept in a **thread-local** read back on the main
//! thread rather than captured.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use tauri::{Webview, WebviewWindow};

thread_local! {
    /// The `GtkLayout` that hosts the main webview and, on top of it, any
    /// page-view child webviews. Created once by [`install`].
    static PAGE_LAYER: RefCell<Option<gtk::Layout>> = const { RefCell::new(None) };
}

/// Restructure the main window so a page-view child can overlay the content:
/// wrap the window content in a `GtkLayout` and move the main webview into it.
/// Idempotent; a no-op after the first successful run.
pub fn install(window: &WebviewWindow) {
    // `with_webview` only fails if the webview runtime is gone — nothing to do
    // about that here, so ignore the Result.
    let _ = window.with_webview(|platform| {
        let already = PAGE_LAYER.with(|c| c.borrow().is_some());
        if already {
            return;
        }
        let wv = platform.inner();

        let layout = gtk::Layout::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
        layout.set_hexpand(true);
        layout.set_vexpand(true);

        // Reparent the webview out of tao's GtkBox and into the Layout. Keep the
        // Layout as the box's sole child so tao's window layout is untouched.
        if let Some(parent) = wv.parent() {
            if let Ok(container) = parent.clone().downcast::<gtk::Container>() {
                container.remove(&wv);
                if let Ok(bx) = container.clone().downcast::<gtk::Box>() {
                    bx.pack_start(&layout, true, true, 0);
                } else {
                    container.add(&layout);
                }
            }
        }
        layout.put(&wv, 0, 0);

        // GtkLayout keeps child size requests out of the window minimum. A
        // GtkFixed would lock that minimum to the largest previous allocation.
        // Resize the main webview ourselves; guard requests against re-entrancy.
        let last = Rc::new(Cell::new((0i32, 0i32)));
        let wv_fill = wv.clone();
        layout.connect_size_allocate(move |_, alloc| {
            let (w, h) = (alloc.width(), alloc.height());
            if w <= 0 || h <= 0 {
                return;
            }
            if last.get() != (w, h) {
                last.set((w, h));
                wv_fill.set_size_request(w, h);
            }
            wv_fill.size_allocate(&gtk::Allocation::new(0, 0, w, h));
        });

        layout.show_all();
        PAGE_LAYER.with(|c| *c.borrow_mut() = Some(layout));
    });
}

#[cfg(test)]
mod tests {
    use gtk::prelude::*;

    #[test]
    #[ignore = "requires a GTK display; run with --ignored --test-threads=1"]
    fn child_bounds_do_not_raise_window_minimum() {
        gtk::init().expect("GTK display");
        let layout = gtk::Layout::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
        let main = gtk::DrawingArea::new();
        let page = gtk::DrawingArea::new();
        layout.put(&main, 0, 0);
        layout.put(&page, 400, 50);
        layout.show_all();

        for (width, height) in [(1280, 820), (800, 440), (1600, 1000), (600, 260)] {
            main.set_size_request(width, height);
            page.set_size_request(1000, 900);
            let (minimum, _) = layout.preferred_size();
            assert_eq!((minimum.width, minimum.height), (0, 0));
        }
    }
}

/// Move a freshly-created page-view webview out of the window's GtkBox and into
/// the overlay Layout, at `(x, y)` sized `w × h` (logical px). Must run after the
/// webview exists (i.e. after `Window::add_child` returned).
pub fn attach_child(view: &Webview, x: f64, y: f64, w: f64, h: f64) {
    let _ = view.with_webview(move |platform| {
        let wv = platform.inner();
        PAGE_LAYER.with(|c| {
            let layer = c.borrow();
            let Some(layout) = layer.as_ref() else {
                return;
            };
            if let Some(parent) = wv.parent() {
                if let Ok(container) = parent.clone().downcast::<gtk::Container>() {
                    container.remove(&wv);
                }
            }
            layout.put(&wv, x as i32, y as i32);
            wv.set_size_request(w as i32, h as i32);
            wv.show_all();
        });
    });
}

/// Reposition/resize an already-attached page-view webview via its Layout parent.
/// `move_` + `set_size_request` persist across GTK relayouts, unlike the bare
/// `size_allocate` wry's `set_bounds` performs on Linux today.
pub fn set_child_bounds(view: &Webview, x: f64, y: f64, w: f64, h: f64) {
    let _ = view.with_webview(move |platform| {
        let wv = platform.inner();
        if let Some(layout) = wv.parent().and_then(|p| p.downcast::<gtk::Layout>().ok()) {
            layout.move_(&wv, x as i32, y as i32);
        }
        if w > 0.0 && h > 0.0 {
            wv.set_size_request(w as i32, h as i32);
        }
    });
}
