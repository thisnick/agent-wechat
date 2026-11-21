//! Route sound through the OS notification service so system quiet settings apply.
use crate::notifications::{CallActionTarget, Target};
use anyhow::Result;

#[cfg(windows)]
use tauri_plugin_notification::NotificationExt;

#[cfg(target_os = "macos")]
const SOUND_NAME: &str = "AgentWechatSoftPair";
/// The bundled message sound's file name, as the UserNotifications framework looks it up.
#[cfg(target_os = "macos")]
const SOUND_FILE: &str = "AgentWechatSoftPair.wav";
#[cfg(target_os = "macos")]
const RING_SOUND_NAME: &str = "AgentWechatSoftRing";
/// The bundled incoming-call ringtone's file name, as UserNotifications looks it up.
#[cfg(target_os = "macos")]
const RING_SOUND_FILE: &str = "AgentWechatSoftRing.wav";
#[cfg(windows)]
const SOUND_NAME: &str = "Default";

/// Handle to a posted incoming-call OS alert so it can be dismissed as soon as
/// the offer expires, is answered, is declined, or the main window is focused.
pub struct ActiveCallAlert {
    #[cfg(target_os = "macos")]
    request_id: Option<String>,
    #[cfg(target_os = "linux")]
    cancel: tokio_util::sync::CancellationToken,
}

impl ActiveCallAlert {
    pub fn clear(self) {
        #[cfg(target_os = "macos")]
        if let Some(request_id) = self.request_id {
            if crate::macos_notifications::available() {
                crate::macos_notifications::remove_request(&request_id);
            }
        }
        #[cfg(target_os = "linux")]
        self.cancel.cancel();
    }
}

#[cfg(windows)]
pub async fn show(
    app: &tauri::AppHandle,
    title: &str,
    subtitle: Option<&str>,
    body: &str,
    target: Option<Target>,
    _server_id: Option<String>,
    sound: bool,
) -> Result<()> {
    if let Some(subtitle) = subtitle {
        return show_group(app, title, subtitle, body, target, sound);
    }
    let mut notification = app.notification().builder().title(title).body(body);
    if let Some(target) = target {
        notification = notification.extra("target", target);
    }
    if sound {
        notification = notification.sound(SOUND_NAME);
    }
    notification.show()?;
    Ok(())
}

#[cfg(windows)]
pub async fn show_call(
    app: &tauri::AppHandle,
    _request_id: &str,
    title: &str,
    body: &str,
    target: CallActionTarget,
    sound: bool,
) -> Result<ActiveCallAlert> {
    let mut notification = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .extra("callTarget", target);
    if sound {
        notification = notification.sound(SOUND_NAME);
    }
    notification.show()?;
    Ok(ActiveCallAlert {})
}

#[cfg(windows)]
fn native_group_notification(
    title: &str,
    subtitle: &str,
    body: &str,
    sound: bool,
) -> notify_rust::Notification {
    let mut notification = notify_rust::Notification::new();
    notification
        .summary(title)
        .subtitle(subtitle)
        .body(body)
        .auto_icon();
    if sound {
        notification.sound_name(SOUND_NAME);
    }
    notification
}

#[cfg(windows)]
fn show_group(
    app: &tauri::AppHandle,
    title: &str,
    subtitle: &str,
    body: &str,
    target: Option<Target>,
    sound: bool,
) -> Result<()> {
    // The Tauri builder omits subtitles. Use its underlying native library for
    // group alerts, preserving the same application identity and click handling.
    let mut notification = native_group_notification(title, subtitle, body, sound);
    let exe = tauri::utils::platform::current_exe()?;
    if exe.parent().is_some_and(|parent| {
        !["debug", "release"]
            .iter()
            .any(|profile| parent.ends_with(std::path::Path::new("target").join(profile)))
    }) {
        notification.app_id(&app.config().identifier);
    }
    let clicked = app.clone();
    std::thread::Builder::new()
        .name("group-notification".into())
        .spawn(move || {
            let Ok(handle) = notification.show() else {
                return;
            };
            let _ = handle.wait_for_response(|response: &notify_rust::NotificationResponse| {
                if response.is_default_action() {
                    if let Some(target) = target {
                        crate::notifications::open_target(clicked, target);
                    }
                }
            });
        })?;
    Ok(())
}

/// Shows the alert and opens `target` when it is clicked.
///
/// The bundled app posts through the UserNotifications framework: alerts are grouped
/// by chat, a click works even after the app restarted, and a quotable message offers
/// an inline Reply (see `macos_notifications`). Without a bundle, such as under
/// `tauri dev`, it falls back to `mac-notification-sys`.
#[cfg(target_os = "macos")]
pub async fn show(
    app: &tauri::AppHandle,
    title: &str,
    subtitle: Option<&str>,
    body: &str,
    target: Option<Target>,
    server_id: Option<String>,
    sound: bool,
) -> Result<()> {
    if let Some(target) = target
        .as_ref()
        .filter(|_| crate::macos_notifications::available())
    {
        let payload = crate::notifications::Payload {
            target: target.clone(),
            server_id,
        };
        let category = if payload.quotable() {
            crate::macos_notifications::AlertCategory::MessageReply
        } else {
            crate::macos_notifications::AlertCategory::Message
        };
        crate::macos_notifications::post(&crate::macos_notifications::Alert {
            request_id: &target.request_id(),
            thread_key: &target.thread_key(),
            title,
            subtitle,
            body,
            sound: sound.then_some(SOUND_FILE),
            payload: &serde_json::to_string(&payload)?,
            category,
        });
        return Ok(());
    }
    show_fallback(app, title, subtitle, body, target, sound)
}

/// Shows an incoming-call alert using the bundled ringtone and Answer/Decline actions.
#[cfg(target_os = "macos")]
pub async fn show_call(
    app: &tauri::AppHandle,
    request_id: &str,
    title: &str,
    body: &str,
    target: CallActionTarget,
    sound: bool,
) -> Result<ActiveCallAlert> {
    if crate::macos_notifications::available() {
        let thread_key = format!("call|{}|{}", target.connection_id, target.account_id);
        let payload = crate::notifications::MacPayload::Call(target);
        crate::macos_notifications::post(&crate::macos_notifications::Alert {
            request_id,
            thread_key: &thread_key,
            title,
            subtitle: None,
            body,
            sound: sound.then_some(RING_SOUND_FILE),
            payload: &serde_json::to_string(&payload)?,
            category: crate::macos_notifications::AlertCategory::CallIncoming,
        });
        return Ok(ActiveCallAlert {
            request_id: Some(request_id.to_owned()),
        });
    }
    let _ = mac_notification_sys::set_application(if tauri::is_dev() {
        "com.apple.Terminal"
    } else {
        &app.config().identifier
    });
    let (title, body) = (title.to_owned(), body.to_owned());
    let clicked = app.clone();
    std::thread::Builder::new()
        .name("call-notification".into())
        .spawn(move || {
            let mut notification = mac_notification_sys::Notification::new();
            notification
                .title(&title)
                .message(&body)
                .wait_for_click(true);
            if sound {
                notification.sound(RING_SOUND_NAME);
            }
            if opens_target(&notification.send()) {
                crate::notifications::open_call_action(clicked, target);
            }
        })?;
    Ok(ActiveCallAlert { request_id: None })
}

/// Shows the alert outside a bundle and opens `target` on a click, including one
/// later in Notification Center.
#[cfg(target_os = "macos")]
fn show_fallback(
    app: &tauri::AppHandle,
    title: &str,
    subtitle: Option<&str>,
    body: &str,
    target: Option<Target>,
    sound: bool,
) -> Result<()> {
    // The first call sets the identity for the life of the process.
    let _ = mac_notification_sys::set_application(if tauri::is_dev() {
        "com.apple.Terminal"
    } else {
        &app.config().identifier
    });
    let (title, subtitle, body) = (
        title.to_owned(),
        subtitle.map(str::to_owned),
        body.to_owned(),
    );
    let clicked = app.clone();
    std::thread::Builder::new()
        .name("notification".into())
        .spawn(move || {
            let mut notification = mac_notification_sys::Notification::new();
            notification
                .title(&title)
                .maybe_subtitle(subtitle.as_deref())
                .message(&body)
                .wait_for_click(true);
            if sound {
                notification.sound(SOUND_NAME);
            }
            if opens_target(&notification.send()) {
                if let Some(target) = target {
                    crate::notifications::open_target(clicked, target);
                }
            }
        })?;
    Ok(())
}

/// Only a click on the alert itself opens its conversation; a dismissal, an alert
/// that could not be delivered or one removed without a click opens nothing.
#[cfg(target_os = "macos")]
fn opens_target(
    response: &mac_notification_sys::error::NotificationResult<
        mac_notification_sys::NotificationResponse,
    >,
) -> bool {
    matches!(
        response,
        Ok(mac_notification_sys::NotificationResponse::Click)
    )
}

#[cfg(target_os = "linux")]
fn linux_notification(
    title: &str,
    subtitle: Option<&str>,
    body: &str,
    sound: Option<&std::path::Path>,
) -> notify_rust::Notification {
    let mut notification = notify_rust::Notification::new();
    // Freedesktop notifications have no subtitle field. Keep the same reading
    // order by putting the group on its own line above the escaped message.
    let body = subtitle.map_or_else(
        || body.to_owned(),
        |group| {
            let group = group
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            format!("{group}\n{body}")
        },
    );
    notification
        .appname("AgentWechat")
        .summary(title)
        .body(&body)
        .auto_icon();
    if let Some(path) = sound {
        notification.hint(notify_rust::Hint::SoundFile(
            path.to_string_lossy().into_owned(),
        ));
    } else {
        notification.hint(notify_rust::Hint::SuppressSound(true));
    }
    notification
}

#[cfg(target_os = "linux")]
pub async fn show(
    app: &tauri::AppHandle,
    title: &str,
    subtitle: Option<&str>,
    body: &str,
    target: Option<Target>,
    _server_id: Option<String>,
    sound: bool,
) -> Result<()> {
    use tauri::Manager;
    let path = app
        .path()
        .resource_dir()?
        .join("sounds/AgentWechatSoftPair.wav");
    let mut notification =
        linux_notification(title, subtitle, body, sound.then_some(path.as_path()));
    if target.is_some() {
        notification.action("default", "");
    }
    let handle = notification.show_async().await?;
    let cancel = app.state::<crate::Desktop>().notifications.cancel.clone();
    let clicked = app.clone();
    // Retain the handle/connection until dismissal; GNOME closes notifications
    // when their sender disconnects. Cancellation releases listeners on Quit.
    tauri::async_runtime::spawn(async move {
        tokio::select! {
            _ = cancel.cancelled() => {},
            _ = handle.wait_for_action_async(move |response| {
                if matches!(response, notify_rust::NotificationResponse::Default) {
                    if let Some(target) = target {
                        crate::notifications::open_target(clicked, target);
                    }
                }
            }) => {}
        }
    });
    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn show_call(
    app: &tauri::AppHandle,
    _request_id: &str,
    title: &str,
    body: &str,
    target: CallActionTarget,
    sound: bool,
) -> Result<ActiveCallAlert> {
    use tauri::Manager;
    let path = app
        .path()
        .resource_dir()?
        .join("sounds/AgentWechatSoftRing.wav");
    let escaped_body = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let mut notification =
        linux_notification(title, None, &escaped_body, sound.then_some(path.as_path()));
    notification.action("default", "");
    let handle = notification.show_async().await?;
    let app_cancel = app.state::<crate::Desktop>().notifications.cancel.clone();
    let alert_cancel = tokio_util::sync::CancellationToken::new();
    let watch_cancel = alert_cancel.clone();
    let clicked = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::select! {
            _ = app_cancel.cancelled() => {},
            _ = watch_cancel.cancelled() => {},
            _ = handle.wait_for_action_async(move |response| {
                if matches!(response, notify_rust::NotificationResponse::Default) {
                    crate::notifications::open_call_action(clicked, target);
                }
            }) => {}
        }
    });
    Ok(ActiveCallAlert {
        cancel: alert_cancel,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn soft_pair_asset_is_short_pcm_without_clipping() {
        let bytes = include_bytes!("../sounds/AgentWechatSoftPair.wav");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
        assert_eq!(bytes.len() - 44, 48_000 * 36 / 100 * 2);
        assert!(bytes[44..].chunks_exact(2).all(|sample| i16::from_le_bytes(
            sample.try_into().unwrap()
        )
        .unsigned_abs()
            <= 7_210));
    }

    #[test]
    fn soft_ring_asset_is_3s_pcm_without_clipping() {
        let bytes = include_bytes!("../sounds/AgentWechatSoftRing.wav");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
        assert_eq!(bytes.len() - 44, 48_000 * 3 * 2);
        assert!(bytes[44..].chunks_exact(2).all(|sample| i16::from_le_bytes(
            sample.try_into().unwrap()
        )
        .unsigned_abs()
            <= 8_000));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_sound_has_a_file_or_explicit_suppression() {
        use notify_rust::Hint;
        let path = std::path::Path::new("/fixture/AgentWechatSoftPair.wav");
        assert!(
            super::linux_notification("Message", None, "Hello", Some(path))
                .hints
                .contains(&Hint::SoundFile(path.display().to_string()))
        );
        assert!(super::linux_notification("Message", None, "Hello", None)
            .hints
            .contains(&Hint::SuppressSound(true)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_group_context_is_a_separate_escaped_line_above_the_message() {
        let notification =
            super::linux_notification("Alice", Some("Team <Friends>"), "Hello", None);
        assert_eq!(notification.summary, "Alice");
        assert_eq!(notification.body, "Team &lt;Friends&gt;\nHello");
    }

    #[cfg(windows)]
    #[test]
    fn native_groups_use_separate_title_subtitle_and_body_fields() {
        let notification = super::native_group_notification("Alice", "Team", "Hello", true);
        assert_eq!(notification.summary, "Alice");
        assert_eq!(notification.subtitle.as_deref(), Some("Team"));
        assert_eq!(notification.body, "Hello");
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_native_default_sound() {
        assert_eq!(super::SOUND_NAME, "Default");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_the_bundled_sound_names() {
        assert_eq!(super::SOUND_NAME, "AgentWechatSoftPair");
        assert_eq!(super::RING_SOUND_NAME, "AgentWechatSoftRing");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_opens_the_conversation_only_for_a_click() {
        use mac_notification_sys::NotificationResponse;
        assert!(super::opens_target(&Ok(NotificationResponse::Click)));
        for response in [
            NotificationResponse::None,
            NotificationResponse::CloseButton("Close".into()),
            NotificationResponse::ActionButton("Reply".into()),
            NotificationResponse::Reply("Hi".into()),
        ] {
            assert!(!super::opens_target(&Ok(response)));
        }
    }
}
