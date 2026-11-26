//! Platform integration (build plan, 7.2): the Dock badge and user
//! notifications on macOS. Elsewhere these only log.
//!
//! Notifications use `UNUserNotificationCenter`, which only works from a
//! bundled `.app` (it needs a bundle identifier). Under `cargo run` the
//! app logs instead.

use futures::StreamExt as _;
use gpui_kit::App;
use uuid::Uuid;

/// Start listening for notification clicks. Call once at startup.
pub fn init(cx: &mut App) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
    imp::init(tx);
    imp::hide_title_bar_in_full_screen(crate::windows::TABBING_ID);
    cx.spawn(async move |cx| {
        while let Some(identifier) = rx.next().await {
            let Some((project, session)) = parse_identifier(&identifier) else {
                continue;
            };
            cx.update(|cx| crate::windows::reveal_session(project, session, cx));
        }
    })
    .detach();
}

/// The Dock badge: the number of sessions needing input; 0 clears it.
pub fn set_badge_count(count: usize) {
    imp::set_badge(if count == 0 {
        None
    } else {
        Some(count.to_string())
    });
}

/// Keep project windows out of macOS's own window list (the Window menu's
/// and the Dock menu's): the app lists them itself (`menus`), by project,
/// from its own record of open windows. macOS's list kept stale or
/// duplicate names: the app rebuilds its menu bar (and so the window list's
/// menu) whenever projects change, and AppKit's bookkeeping went wrong,
/// showing one project's name twice. Safe to call any time.
pub fn keep_project_windows_out_of_window_list() {
    imp::keep_out_of_window_list(crate::windows::TABBING_ID);
}

/// System Settings ▸ Desktop & Dock ▸ "Prefer tabs when opening documents".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum TabbingMode {
    Always,
    /// The default: new windows become tabs while the app is full screen.
    InFullScreen,
    Never,
}

pub fn tabbing_mode() -> TabbingMode {
    imp::tabbing_mode()
}

/// Post a notification. Clicking it focuses the project's window and
/// selects the session.
pub fn notify(title: &str, body: &str, project_id: Uuid, session_id: &str) {
    let identifier = format!("anveg|{project_id}|{session_id}|{}", Uuid::new_v4());
    imp::notify(title, body, &identifier);
}

fn parse_identifier(identifier: &str) -> Option<(Uuid, String)> {
    let mut parts = identifier.split('|');
    if parts.next()? != "anveg" {
        return None;
    }
    let project = Uuid::parse_str(parts.next()?).ok()?;
    let session = parts.next()?.to_owned();
    Some((project, session))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use std::ptr::NonNull;
    use std::sync::{Mutex, OnceLock};

    use futures::channel::mpsc::UnboundedSender;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AllocAnyThread as _, MainThreadMarker, define_class, msg_send};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{
        NSBundle, NSError, NSNotification, NSNotificationCenter, NSOperationQueue, NSString,
    };
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };

    static CLICKS: OnceLock<Mutex<Option<UnboundedSender<String>>>> = OnceLock::new();
    static AUTHORIZATION_REQUESTED: OnceLock<()> = OnceLock::new();

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "AnvegNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            /// Show banners even while the app is in front (a session in
            /// another window may need input).
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                handler: &block2::DynBlock<dyn Fn()>,
            ) {
                let identifier = response.notification().request().identifier().to_string();
                if let Some(lock) = CLICKS.get()
                    && let Ok(guard) = lock.lock()
                    && let Some(tx) = guard.as_ref()
                {
                    let _ = tx.unbounded_send(identifier);
                }
                handler.call(());
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Whether this process is a bundled app (notifications need that).
    fn is_bundled() -> bool {
        NSBundle::mainBundle().bundleIdentifier().is_some()
    }

    fn center() -> Option<Retained<UNUserNotificationCenter>> {
        is_bundled().then(UNUserNotificationCenter::currentNotificationCenter)
    }

    pub fn init(tx: UnboundedSender<String>) {
        let _ = CLICKS.set(Mutex::new(Some(tx)));
        let Some(center) = center() else {
            tracing::info!("not running from an app bundle; notifications will be logged instead");
            return;
        };
        let delegate = Delegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly; keep ours for the app's life.
        std::mem::forget(delegate);
    }

    fn request_authorization(center: &UNUserNotificationCenter) {
        if AUTHORIZATION_REQUESTED.set(()).is_err() {
            return;
        }
        let handler =
            block2::RcBlock::new(|granted: objc2::runtime::Bool, _error: *mut NSError| {
                tracing::info!(granted = granted.as_bool(), "notification permission");
            });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert
                | UNAuthorizationOptions::Sound
                | UNAuthorizationOptions::Badge,
            &handler,
        );
    }

    pub fn notify(title: &str, body: &str, identifier: &str) {
        let Some(center) = center() else {
            tracing::info!(title, body, "notification (not bundled, so only logged)");
            return;
        };
        request_authorization(&center);
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(identifier),
            &content,
            None,
        );
        let done = block2::RcBlock::new(|error: *mut NSError| {
            if !error.is_null() {
                tracing::warn!("could not post a notification");
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&done));
    }

    /// `AppleWindowTabbingMode` in the global defaults: "always",
    /// "fullscreen" (also when unset) or "manual".
    pub fn tabbing_mode() -> super::TabbingMode {
        let defaults = objc2_foundation::NSUserDefaults::standardUserDefaults();
        let value = defaults
            .stringForKey(&NSString::from_str("AppleWindowTabbingMode"))
            .map(|s| s.to_string());
        match value.as_deref() {
            Some("always") => super::TabbingMode::Always,
            Some("manual") => super::TabbingMode::Never,
            _ => super::TabbingMode::InFullScreen,
        }
    }

    /// In full screen macOS slides the window's title bar (the window
    /// buttons, and the window tab bar) down over the top of the window
    /// whenever the pointer reaches the menu bar. With the app's own title
    /// bar that covered its top rows, so in full screen the title bar is
    /// hidden: project windows show their own window tabs and an Exit Full
    /// Screen button instead (`window_tabs`). It comes back as the window
    /// starts to leave full screen. Only project windows (tabbing
    /// identifier `project_tabbing_id`): other windows, such as the welcome
    /// window, have no such row and keep macOS's bar, so they can still
    /// leave full screen.
    pub fn hide_title_bar_in_full_screen(project_tabbing_id: &'static str) {
        let center = NSNotificationCenter::defaultCenter();
        let queue = NSOperationQueue::mainQueue();
        for (name, hidden) in [
            ("NSWindowDidEnterFullScreenNotification", true),
            ("NSWindowWillExitFullScreenNotification", false),
        ] {
            let block = block2::RcBlock::new(move |n: NonNull<NSNotification>| {
                // SAFETY: AppKit passes a live notification.
                let n = unsafe { n.as_ref() };
                set_title_bar_hidden(n, hidden, project_tabbing_id);
            });
            // SAFETY: the block only touches AppKit on the main queue.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(&NSString::from_str(name)),
                    None,
                    Some(&queue),
                    &block,
                )
            };
            // Observes for the app's life.
            std::mem::forget(token);
        }
    }

    /// Hide or show the title bar of the notification's window: the view
    /// that holds the window buttons and the tab bar.
    fn set_title_bar_hidden(n: &NSNotification, hidden: bool, project_tabbing_id: &str) {
        let Some(window) = n.object() else {
            return;
        };
        // SAFETY: plain AppKit messages, each result checked before use.
        unsafe {
            let is_window: bool = msg_send![&*window, isKindOfClass: objc2::class!(NSWindow)];
            if !is_window {
                return;
            }
            let tabbing: Option<Retained<NSString>> = msg_send![&*window, tabbingIdentifier];
            if tabbing.map(|t| t.to_string()).as_deref() != Some(project_tabbing_id) {
                return;
            }
            // NSWindowCloseButton = 0.
            let button: *mut AnyObject = msg_send![&*window, standardWindowButton: 0usize];
            let Some(button) = button.as_ref() else {
                return;
            };
            let bar: *mut AnyObject = msg_send![button, superview];
            let Some(bar) = bar.as_ref() else {
                return;
            };
            let container: *mut AnyObject = msg_send![bar, superview];
            let Some(container) = container.as_ref() else {
                return;
            };
            let () = msg_send![container, setHidden: hidden];
        }
        tracing::debug!(hidden, "full-screen title bar");
    }

    pub fn keep_out_of_window_list(project_tabbing_id: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        // SAFETY: plain AppKit messages on the main thread, each result
        // checked before use.
        unsafe {
            let app: *mut AnyObject = msg_send![objc2::class!(NSApplication), sharedApplication];
            let Some(app) = app.as_ref() else {
                return;
            };
            let windows: *mut AnyObject = msg_send![app, windows];
            let Some(windows) = windows.as_ref() else {
                return;
            };
            let count: usize = msg_send![windows, count];
            for i in 0..count {
                let window: *mut AnyObject = msg_send![windows, objectAtIndex: i];
                let Some(window) = window.as_ref() else {
                    continue;
                };
                let is_window: bool = msg_send![window, isKindOfClass: objc2::class!(NSWindow)];
                if !is_window {
                    continue;
                }
                let tabbing: Option<Retained<NSString>> = msg_send![window, tabbingIdentifier];
                if tabbing.map(|t| t.to_string()).as_deref() != Some(project_tabbing_id) {
                    continue;
                }
                let () = msg_send![window, setExcludedFromWindowsMenu: true];
                // Already listed (setting the title adds it): take it out.
                let () = msg_send![app, removeWindowsItem: window];
            }
        }
    }

    pub fn set_badge(label: Option<String>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let tile = app.dockTile();
        let label = label.map(|l| NSString::from_str(&l));
        tile.setBadgeLabel(label.as_deref());
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use futures::channel::mpsc::UnboundedSender;

    pub fn init(_tx: UnboundedSender<String>) {}

    pub fn hide_title_bar_in_full_screen(_project_tabbing_id: &'static str) {}

    pub fn keep_out_of_window_list(_project_tabbing_id: &str) {}

    /// No native window tabs on other platforms.
    pub fn tabbing_mode() -> super::TabbingMode {
        super::TabbingMode::Never
    }

    pub fn notify(title: &str, body: &str, _identifier: &str) {
        tracing::info!(title, body, "notification");
    }

    pub fn set_badge(label: Option<String>) {
        tracing::debug!(?label, "dock badge (macOS only)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_round_trip() {
        let project = Uuid::new_v4();
        let id = format!("anveg|{project}|abc-123|{}", Uuid::new_v4());
        assert_eq!(parse_identifier(&id), Some((project, "abc-123".to_owned())));
        assert_eq!(parse_identifier("other|x"), None);
    }
}
