//! Desktop text (tray, dialogs, notifications) in the user's language.
//!
//! The text comes from the UI's catalogs under `desktopShell.*`, compiled in, so the
//! app and the UI share one source. English is the fallback for a missing key.
//! The UI reports the language it shows through the `desktop_language` command.
use serde_json::Value;
use std::sync::{OnceLock, RwLock};

/// Shipped catalogs. Keep in step with `languages` in `client-ui/src/i18n.ts`.
const CATALOGS: &[(&str, &str)] = &[
    ("en", include_str!("../../../client-ui/locales/en.json")),
    (
        "zh-CN",
        include_str!("../../../client-ui/locales/zh-CN.json"),
    ),
];

static LANGUAGE: RwLock<&str> = RwLock::new("en");

fn catalogs() -> &'static [(&'static str, Value)] {
    static PARSED: OnceLock<Vec<(&'static str, Value)>> = OnceLock::new();
    PARSED.get_or_init(|| {
        CATALOGS
            .iter()
            .map(|(tag, text)| (*tag, serde_json::from_str(text).unwrap_or(Value::Null)))
            .collect()
    })
}

/// The shipped language tag matching `tag` exactly, if any.
pub fn supported(tag: &str) -> Option<&'static str> {
    CATALOGS
        .iter()
        .map(|(shipped, _)| *shipped)
        .find(|shipped| *shipped == tag)
}

/// Use `tag` for later text. Unknown tags are refused and leave the language unchanged.
pub fn set_language(tag: &str) -> bool {
    let Some(tag) = supported(tag) else {
        return false;
    };
    if let Ok(mut language) = LANGUAGE.write() {
        *language = tag;
    }
    true
}

/// The current language tag.
pub fn language() -> &'static str {
    LANGUAGE.read().map(|language| *language).unwrap_or("en")
}

fn lookup(language: &str, key: &str) -> Option<&'static str> {
    let (_, catalog) = catalogs().iter().find(|(tag, _)| *tag == language)?;
    let mut node = catalog.get("desktopShell")?;
    for part in key.split('.') {
        node = node.get(part)?;
    }
    node.as_str().filter(|text| !text.is_empty())
}

/// `desktopShell.<key>` in `language`, falling back to English and then to the key.
pub fn text_in(language: &str, key: &str) -> String {
    lookup(language, key)
        .or_else(|| lookup("en", key))
        .unwrap_or(key)
        .to_owned()
}

/// `desktopShell.<key>` in the current language.
pub fn text(key: &str) -> String {
    text_in(language(), key)
}

/// `desktopShell.<key>` in the current language with `{{name}}` placeholders filled in.
// Only macOS failure alerts use it so far.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn text_with(key: &str, values: &[(&str, &str)]) -> String {
    values.iter().fold(text(key), |text, (name, value)| {
        text.replace(&format!("{{{{{name}}}}}"), value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_parses_and_english_has_every_desktop_key() {
        let english = lookup("en", "trayOpen");
        assert_eq!(english, Some("Open AgentWechat"));
        for (tag, catalog) in catalogs() {
            assert!(catalog.is_object(), "{tag} catalog does not parse");
            let Some(desktop) = catalog["desktopShell"].as_object() else {
                continue;
            };
            for key in desktop.keys() {
                assert!(
                    lookup("en", key).is_some(),
                    "{tag} has desktopShell.{key} but English does not"
                );
            }
        }
    }

    #[test]
    fn missing_keys_fall_back_to_english_and_unknown_languages_are_refused() {
        assert_eq!(text_in("zh-CN", "trayOpen"), "打开 AgentWechat");
        assert_eq!(text_in("xx", "trayOpen"), "Open AgentWechat");
        assert_eq!(text_in("en", "noSuchKey"), "noSuchKey");
        assert_eq!(supported("fr"), None);
        assert!(!set_language("fr"));
    }

    #[test]
    fn placeholders_are_filled_in() {
        assert_eq!(
            text_in("en", "actionFailedBody").replace("{{error}}", "Offline"),
            "Offline. Click to open the chat."
        );
    }
}
