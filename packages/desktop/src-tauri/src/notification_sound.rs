//! Route sound through the OS notification service so system quiet settings apply.
use crate::notifications::Target;
use anyhow::Result;

#[cfg(any(target_os = "macos", windows))]
use tauri_plugin_notification::NotificationExt;

#[cfg(target_os = "macos")]
const SOUND_NAME: &str = "AgentWechatSoftPair.wav";
#[cfg(windows)]
const SOUND_NAME: &str = "Default";

#[cfg(any(target_os = "macos", windows))]
pub async fn show(
    app: &tauri::AppHandle,
    title: &str,
    body: &str,
    target: Option<Target>,
    sound: bool,
) -> Result<()> {
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

#[cfg(target_os = "linux")]
fn linux_notification(
    title: &str,
    body: &str,
    sound: Option<&std::path::Path>,
) -> notify_rust::Notification {
    let mut notification = notify_rust::Notification::new();
    notification
        .appname("AgentWechat")
        .summary(title)
        .body(body)
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
    body: &str,
    target: Option<Target>,
    sound: bool,
) -> Result<()> {
    use tauri::Manager;
    let path = app
        .path()
        .resource_dir()?
        .join("sounds/AgentWechatSoftPair.wav");
    let mut notification = linux_notification(title, body, sound.then_some(path.as_path()));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_sound_has_a_file_or_explicit_suppression() {
        use notify_rust::Hint;
        let path = std::path::Path::new("/fixture/AgentWechatSoftPair.wav");
        assert!(super::linux_notification("Message", "Hello", Some(path))
            .hints
            .contains(&Hint::SoundFile(path.display().to_string())));
        assert!(super::linux_notification("Message", "Hello", None)
            .hints
            .contains(&Hint::SuppressSound(true)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_native_default_sound() {
        assert_eq!(super::SOUND_NAME, "Default");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_sound_matches_the_bundled_filename() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let resource = config["bundle"]["resources"]["sounds/AgentWechatSoftPair.wav"]
            .as_str()
            .unwrap();
        assert_eq!(super::SOUND_NAME, resource);
    }
}
