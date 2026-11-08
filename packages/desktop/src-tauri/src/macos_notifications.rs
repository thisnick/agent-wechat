//! macOS alerts through the UserNotifications framework.
//!
//! Alerts are grouped by chat, carry what a click or action needs inside the
//! notification itself (so they still work after the app restarts), and offer
//! Mark as Read, plus an inline Reply when the message can be quoted. The
//! framework only works in the bundled app: check [`available`] before calling
//! anything else here.
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread};
use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSError, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationContent, UNNotificationDefaultActionIdentifier,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSound, UNTextInputNotificationAction, UNTextInputNotificationResponse,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, Once, OnceLock};

/// Identifier of the inline Reply action.
pub const REPLY_ACTION: &str = "reply";
/// Identifier of the Mark as Read action.
pub const MARK_READ_ACTION: &str = "mark-read";
/// Category of alerts that offer Mark as Read only.
const CATEGORY: &str = "message";
/// Category of alerts that offer Reply and Mark as Read.
const REPLY_CATEGORY: &str = "message-reply";
/// `userInfo` key holding the alert's JSON payload.
const PAYLOAD_KEY: &str = "agentWechat";

/// One alert to show.
pub struct Alert<'a> {
    /// Unique per message; posting the same identifier again replaces the alert.
    pub request_id: &'a str,
    /// Alerts with the same key are grouped together in Notification Center.
    pub thread_key: &'a str,
    pub title: &'a str,
    pub subtitle: Option<&'a str>,
    pub body: &'a str,
    /// File name of a sound in the app bundle, or `None` for a silent alert.
    pub sound: Option<&'a str>,
    /// Opaque JSON handed back with the response.
    pub payload: &'a str,
    /// Whether to offer the inline Reply action.
    pub reply: bool,
}

/// What the user did with an alert.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Clicked the alert itself.
    Open,
    /// Sent this text with the Reply action.
    Reply(String),
    /// Chose Mark as Read.
    MarkRead,
    /// Anything else, such as a dismissal.
    Other,
}

/// A response to an alert, with the payload it was posted with.
pub struct Response {
    pub request_id: String,
    pub action: Action,
    pub payload: Option<String>,
}

type Handler = Box<dyn Fn(Response) + Send + Sync>;
static HANDLER: OnceLock<Handler> = OnceLock::new();
static INSTALL: Once = Once::new();
/// 0 while unknown, 1 once permission is granted, 2 when it is refused.
static PERMISSION: AtomicU8 = AtomicU8::new(0);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Whether this process is a bundled app that UserNotifications can serve. An
/// unbundled binary, such as `tauri dev`, raises an exception on first use.
pub fn available() -> bool {
    let bundle = NSBundle::mainBundle();
    bundle.bundleIdentifier().is_some() && bundle.bundlePath().to_string().ends_with(".app")
}

/// Registers the response handler, the alert categories and the delegate, and asks
/// for permission. Call it once while the app launches, before it finishes
/// launching, so a click that launched the app is delivered. Later calls do nothing.
pub fn install(handler: impl Fn(Response) + Send + Sync + 'static) {
    INSTALL.call_once(|| {
        let _ = HANDLER.set(Box::new(handler));
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = Delegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center keeps only a weak reference; the delegate lives as long as the app.
        std::mem::forget(delegate);
        center.setNotificationCategories(&categories());
        let done = RcBlock::new(|granted: Bool, error: *mut NSError| {
            PERMISSION.store(if granted.as_bool() { 1 } else { 2 }, Ordering::Relaxed);
            // SAFETY: the framework passes a valid error or null.
            if let Some(error) = unsafe { error.as_ref() } {
                record(error.localizedDescription().to_string());
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &done,
        );
    });
}

/// Shows `alert`.
pub fn post(alert: &Alert) {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(alert.title));
    if let Some(subtitle) = alert.subtitle {
        content.setSubtitle(&NSString::from_str(subtitle));
    }
    content.setBody(&NSString::from_str(alert.body));
    if let Some(sound) = alert.sound {
        content.setSound(Some(&UNNotificationSound::soundNamed(&NSString::from_str(
            sound,
        ))));
    }
    content.setThreadIdentifier(&NSString::from_str(alert.thread_key));
    content.setCategoryIdentifier(&NSString::from_str(category(alert.reply)));
    let info = NSDictionary::<NSString, NSString>::from_slices(
        &[&*NSString::from_str(PAYLOAD_KEY)],
        &[&*NSString::from_str(alert.payload)],
    );
    // SAFETY: an NSDictionary of strings is a valid `userInfo`; the generic parameters
    // only exist at compile time.
    unsafe { content.setUserInfo(&Retained::cast_unchecked::<NSDictionary>(info)) };
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(alert.request_id),
        &content,
        None,
    );
    let request_id = alert.request_id.to_owned();
    let done = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: the framework passes a valid error or null.
        if let Some(error) = unsafe { error.as_ref() } {
            record(format!("{request_id}: {}", error.localizedDescription()));
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&done));
}

/// Removes the delivered alerts grouped under `thread_key`, such as once the chat
/// has been read in the app.
pub fn clear_thread(thread_key: &str) {
    let key = thread_key.to_owned();
    let found = RcBlock::new(move |delivered: NonNull<NSArray<UNNotification>>| {
        // SAFETY: the framework passes a valid array for the duration of the call.
        let delivered = unsafe { delivered.as_ref() };
        let ids: Vec<Retained<NSString>> = delivered
            .iter()
            .map(|notification| notification.request())
            .filter(|request| request.content().threadIdentifier().to_string() == key)
            .map(|request| request.identifier())
            .collect();
        if !ids.is_empty() {
            UNUserNotificationCenter::currentNotificationCenter()
                .removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&ids));
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .getDeliveredNotificationsWithCompletionHandler(&found);
}

/// Whether the user allowed alerts, once the permission request has been answered.
pub fn permitted() -> Option<bool> {
    match PERMISSION.load(Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// The latest error from asking for permission or showing an alert.
pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().ok().and_then(|error| error.clone())
}

fn record(error: String) {
    if let Ok(mut last) = LAST_ERROR.lock() {
        *last = Some(error);
    }
}

fn category(reply: bool) -> &'static str {
    if reply {
        REPLY_CATEGORY
    } else {
        CATEGORY
    }
}

/// Maps a response's action identifier, and the text of a Reply, to an [`Action`].
/// `default` is the framework's identifier for a click on the alert itself.
fn action(identifier: &str, default: &str, text: Option<String>) -> Action {
    match (identifier, text) {
        (id, _) if id == default => Action::Open,
        (REPLY_ACTION, Some(text)) => Action::Reply(text),
        (MARK_READ_ACTION, _) => Action::MarkRead,
        _ => Action::Other,
    }
}

fn categories() -> Retained<NSSet<UNNotificationCategory>> {
    let none = NSArray::<NSString>::new();
    let reply = UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
        &NSString::from_str(REPLY_ACTION),
        &NSString::from_str("Reply"),
        UNNotificationActionOptions::empty(),
        &NSString::from_str("Send"),
        &NSString::from_str("Message"),
    );
    // Without the foreground option, an action is handled without bringing the app forward.
    let mark_read = || {
        UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(MARK_READ_ACTION),
            &NSString::from_str("Mark as Read"),
            UNNotificationActionOptions::empty(),
        )
    };
    let plain = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(CATEGORY),
        &NSArray::from_retained_slice(&[mark_read()]),
        &none,
        UNNotificationCategoryOptions::empty(),
    );
    let replying = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(REPLY_CATEGORY),
        &NSArray::from_retained_slice(&[reply.into_super(), mark_read()]),
        &none,
        UNNotificationCategoryOptions::empty(),
    );
    NSSet::from_retained_slice(&[plain, replying])
}

fn payload(content: &UNNotificationContent) -> Option<String> {
    let value = content
        .userInfo()
        .objectForKey(&NSString::from_str(PAYLOAD_KEY))?;
    value
        .downcast::<NSString>()
        .ok()
        .map(|value| value.to_string())
}

fn deliver(response: &UNNotificationResponse) {
    let request = response.notification().request();
    let text = (response as &AnyObject)
        .downcast_ref::<UNTextInputNotificationResponse>()
        .map(|reply| reply.userText().to_string());
    // SAFETY: a framework-provided constant string.
    let default = unsafe { UNNotificationDefaultActionIdentifier }.to_string();
    let response = Response {
        request_id: request.identifier().to_string(),
        action: action(&response.actionIdentifier().to_string(), &default, text),
        payload: payload(&request.content()),
    };
    if let Some(handler) = HANDLER.get() {
        handler(response);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "AgentWechatNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// The app only posts alerts the user is not reading, so show them even
        /// while it is in front.
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
            deliver(response);
            handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        action, category, Action, CATEGORY, MARK_READ_ACTION, REPLY_ACTION, REPLY_CATEGORY,
    };

    const DEFAULT: &str = "com.apple.UNNotificationDefaultActionIdentifier";

    #[test]
    fn clicks_open_reply_text_replies_and_mark_read_marks_read() {
        assert_eq!(action(DEFAULT, DEFAULT, None), Action::Open);
        assert_eq!(
            action(REPLY_ACTION, DEFAULT, Some("On my way".into())),
            Action::Reply("On my way".into())
        );
        assert_eq!(action(REPLY_ACTION, DEFAULT, None), Action::Other);
        assert_eq!(action(MARK_READ_ACTION, DEFAULT, None), Action::MarkRead);
        assert_eq!(
            action(
                "com.apple.UNNotificationDismissActionIdentifier",
                DEFAULT,
                None
            ),
            Action::Other
        );
    }

    #[test]
    fn only_quotable_alerts_offer_reply() {
        assert_eq!(category(true), REPLY_CATEGORY);
        assert_eq!(category(false), CATEGORY);
    }
}
