//! macOS: what a **running** application is asked to open (parity row C10).
//!
//! On Windows and Linux a double-clicked `.plaudon` archive and a
//! `plaudon://` link both arrive as process arguments, which
//! [`plaudon_platform::activation::launch_request`] parses. macOS does not
//! work that way: Launch Services sends a running (or launching) application
//! an Apple event, which AppKit turns into `application:openURLs:` on the
//! `NSApplicationDelegate`. Nothing of that reaches `argv`.
//!
//! winit 0.30 owns the application delegate — its own documentation still
//! claims it "will not register an application delegate", but
//! `platform_impl/macos/event_loop.rs` calls `app.setDelegate(…)` with
//! `WinitApplicationDelegate`, which implements only
//! `applicationDidFinishLaunching:` and `applicationWillTerminate:`. eframe
//! 0.36 adds nothing on top. So the URL never arrives unless we put it there.
//!
//! Rather than replace that delegate — which would take over the two methods
//! the event loop depends on and leave us forwarding them by hand — this
//! module **adds one method to the class of whatever delegate is installed**
//! through the Objective-C runtime. winit keeps its delegate, its
//! notification registrations and its lifecycle; the class simply gains
//! `application:openURLs:`, whose implementation touches nothing but a queue
//! in this module.
//!
//! Timing is the other half of the problem: AppKit delivers the launch
//! Apple event *before* `applicationDidFinishLaunching:`, which is where
//! eframe first hands control back to us. The method is therefore installed
//! from an `NSApplicationWillFinishLaunchingNotification` observer — the
//! earliest point at which a delegate exists — registered before
//! `eframe::run_native`. Registering a notification observer does not touch
//! `NSApplication`, so winit still initialises it itself.
//!
//! Nothing here interprets a URL: the strings are queued verbatim and
//! [`crate::desktop::Desktop::drain`] hands them to
//! `plaudon_platform::activation::open_request`, which is the same parser the
//! argument vector goes through.

#![allow(
    unsafe_code,
    reason = "AppKit delivers an open-document or open-URL event only to the \
              application delegate, and the delegate belongs to winit; \
              reaching it is an Objective-C runtime call and cannot be \
              expressed in safe Rust"
)]

use std::cell::RefCell;
use std::sync::{Mutex, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{define_class, ffi, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification};
use objc2_foundation::{NSArray, NSNotification, NSNotificationCenter, NSObject, NSURL};

/// URLs the OS handed over since the last frame, in arrival order.
///
/// A `Mutex` rather than a channel: AppKit only ever pushes from the main
/// thread and the render loop only ever pops from it, so there is no
/// contention to speak of, and a `Vec` keeps the order the user clicked in.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The context to wake when something arrives.
///
/// Without it an open that lands while the window is idle would sit in the
/// queue until the next unrelated repaint, because this application paints
/// nothing when nothing happened.
static REPAINT: Mutex<Option<egui::Context>> = Mutex::new(None);

thread_local! {
    /// The observer, kept alive for the life of the process.
    ///
    /// `NSNotificationCenter` does not retain an observer registered with
    /// `addObserver:selector:name:object:`, so dropping this would leave a
    /// dangling registration.
    static OBSERVER: RefCell<Option<Retained<LaunchObserver>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `LaunchObserver` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PlaudonLaunchObserver"]
    #[derive(Debug)]
    struct LaunchObserver;

    impl LaunchObserver {
        /// `NSApplicationWillFinishLaunchingNotification`.
        ///
        /// A private selector name, so that it cannot collide with an AppKit
        /// method on `NSObject`.
        #[unsafe(method(plaudonApplicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            install_open_urls(MainThreadMarker::from(self));
        }
    }
);

impl LaunchObserver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm);
        unsafe { msg_send![this, init] }
    }
}

/// Start listening for documents and links the OS asks us to open.
///
/// Call once, from the main thread, before `eframe::run_native`. A second
/// call does nothing.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::debug!("open-document events need the main thread; none installed");
        return;
    };
    OBSERVER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return;
        }
        let observer = LaunchObserver::new(mtm);
        let object: &AnyObject = &observer;
        // SAFETY: the observer is kept alive below, the selector is the one
        // defined on its class above and takes exactly the notification
        // argument AppKit passes.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                object,
                sel!(plaudonApplicationWillFinishLaunching:),
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
            );
        }
        *slot = Some(observer);
    });
}

/// Repaint this context when an open arrives.
pub fn set_repaint_target(ctx: &egui::Context) {
    let mut target = REPAINT.lock().unwrap_or_else(PoisonError::into_inner);
    *target = Some(ctx.clone());
}

/// Everything the OS asked us to open since the last call.
#[must_use]
pub fn drain_pending() -> Vec<String> {
    let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
    std::mem::take(&mut pending)
}

/// Teach the live application delegate to hand us URLs.
fn install_open_urls(mtm: MainThreadMarker) {
    let application = NSApplication::sharedApplication(mtm);
    let Some(delegate) = application.delegate() else {
        tracing::debug!("no application delegate to extend; opens will not arrive");
        return;
    };
    let object: &AnyObject = AsRef::as_ref(&*delegate);
    let class = object.class();
    if class.instance_method(sel!(application:openURLs:)).is_some() {
        // Somebody else already answers for opens; adding ours would be
        // refused by the runtime anyway, and overriding theirs is not ours
        // to do.
        tracing::debug!("the application delegate already handles opens");
        return;
    }

    // `v@:@@`: returns void, takes the usual self and selector, then the
    // application and the array of URLs — the encoding of
    // `- (void)application:(NSApplication *)app openURLs:(NSArray<NSURL *> *)urls`.
    let types = c"v@:@@";
    // SAFETY: `open_urls` has exactly the signature that encoding describes,
    // and the selector is only ever sent by AppKit with those arguments.
    let added = unsafe {
        let implementation: Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(&AnyObject, Sel, &NSApplication, &NSArray<NSURL>),
            Imp,
        >(open_urls);
        ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            sel!(application:openURLs:),
            implementation,
            types.as_ptr(),
        )
    };
    if added.as_bool() {
        tracing::debug!("the application delegate now routes opens");
    } else {
        tracing::debug!("the application delegate refused the open handler");
    }
}

/// `application:openURLs:`, as the runtime will call it.
///
/// Deliberately does nothing but copy strings: it runs on AppKit's stack,
/// inside a delegate object this module does not own, and anything it
/// touched beyond this module's own queue would be reaching into winit's
/// state.
unsafe extern "C-unwind" fn open_urls(
    _this: &AnyObject,
    _cmd: Sel,
    _application: &NSApplication,
    urls: &NSArray<NSURL>,
) {
    let mut arrived = Vec::new();
    for index in 0..urls.count() {
        let url = urls.objectAtIndex(index);
        if let Some(string) = url.absoluteString() {
            arrived.push(string.to_string());
        }
    }
    if arrived.is_empty() {
        return;
    }
    {
        let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
        pending.extend(arrived);
    }
    let target = REPAINT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(ctx) = target.as_ref() {
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_is_drained_once_and_keeps_its_order() {
        {
            let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
            pending.push("plaudon://settings".to_owned());
            pending.push("file:///tmp/library.plaudon".to_owned());
        }
        assert_eq!(
            drain_pending(),
            vec![
                "plaudon://settings".to_owned(),
                "file:///tmp/library.plaudon".to_owned(),
            ]
        );
        assert!(drain_pending().is_empty(), "a drained queue stays drained");
    }
}
