//! Linux/GTK-only shim that makes the in-app original-page child webview
//! (page_view.rs) genuinely *overlay* the reading area, matching macOS/Windows.
//!
//! Why this exists
//! ---------------
//! On Linux, Tauri/wry parents a child webview to the window's **vertical
//! `gtk::Box`** — `tauri-runtime-wry::create_webview` calls
//! `webview_builder.build_gtk(window.default_vbox())` — so the page stacks as a
//! second pane instead of floating over the reader, and `Webview::set_position/
//! set_size` are no-ops (wry only repositions a child whose parent is a
//! `gtk::Fixed`). This is a long-standing upstream gap (tauri-apps/tauri#10420,
//! still open; fixes #15704 and wry#1745 unmerged as of tauri 2.12.x). We work
//! around it with public API only: build a `gtk::Fixed` filling the window,
//! move the main webview into it, and parent the page-view webview to the same
//! Fixed, positioning it with `Fixed::move_` (which survives GTK relayout
//! passes, unlike a bare size-allocate). See the approach sketched upstream in
//! tauri#10420.
//!
//! Threading: GTK objects are `!Send` and main-thread-only. `Webview::with_webview`
//! yields the platform handle on the GTK main thread but demands a `Send`
//! closure, so the Fixed is kept in a **thread-local** read back on the main
//! thread rather than captured.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use tauri::{Webview, WebviewWindow};

thread_local! {
    /// The `GtkFixed` that hosts the main webview and, on top of it, any
    /// page-view child webviews. Created once by [`install`].
    static PAGE_LAYER: RefCell<Option<gtk::Fixed>> = const { RefCell::new(None) };
}

/// Restructure the main window so a page-view child can overlay the content:
/// wrap the window content in a `GtkFixed` and move the main webview into it.
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

        let fixed = gtk::Fixed::new();
        fixed.set_hexpand(true);
        fixed.set_vexpand(true);

        // Reparent the webview out of tao's GtkBox and into the Fixed. Keep the
        // Fixed as the box's sole child so tao's window layout is untouched.
        if let Some(parent) = wv.parent() {
            if let Ok(container) = parent.clone().downcast::<gtk::Container>() {
                container.remove(&wv);
                if let Ok(bx) = container.clone().downcast::<gtk::Box>() {
                    bx.pack_start(&fixed, true, true, 0);
                } else {
                    container.add(&fixed);
                }
            }
        }
        fixed.put(&wv, 0, 0);

        // A GtkFixed never resizes its children, so the main webview must track
        // the Fixed's allocation itself. Guard against re-entrancy: a no-op
        // size_request on every allocate would loop forever.
        let last = Rc::new(Cell::new((0i32, 0i32)));
        let wv_fill = wv.clone();
        fixed.connect_size_allocate(move |_, alloc| {
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

        fixed.show_all();
        PAGE_LAYER.with(|c| *c.borrow_mut() = Some(fixed));
    });
}

/// Move a freshly-created page-view webview out of the window's GtkBox and into
/// the overlay Fixed, at `(x, y)` sized `w × h` (logical px). Must run after the
/// webview exists (i.e. after `Window::add_child` returned).
pub fn attach_child(view: &Webview, x: f64, y: f64, w: f64, h: f64) {
    let _ = view.with_webview(move |platform| {
        let wv = platform.inner();
        PAGE_LAYER.with(|c| {
            let layer = c.borrow();
            let Some(fixed) = layer.as_ref() else {
                return;
            };
            if let Some(parent) = wv.parent() {
                if let Ok(container) = parent.clone().downcast::<gtk::Container>() {
                    container.remove(&wv);
                }
            }
            fixed.put(&wv, x as i32, y as i32);
            wv.set_size_request(w as i32, h as i32);
            wv.show_all();
        });
    });
}

/// Reposition/resize an already-attached page-view webview via its Fixed parent.
/// `move_` + `set_size_request` persist across GTK relayouts, unlike the bare
/// `size_allocate` wry's `set_bounds` performs on Linux today.
pub fn set_child_bounds(view: &Webview, x: f64, y: f64, w: f64, h: f64) {
    let _ = view.with_webview(move |platform| {
        let wv = platform.inner();
        if let Some(fixed) = wv.parent().and_then(|p| p.downcast::<gtk::Fixed>().ok()) {
            fixed.move_(&wv, x as i32, y as i32);
        }
        if w > 0.0 && h > 0.0 {
            wv.set_size_request(w as i32, h as i32);
        }
    });
}
