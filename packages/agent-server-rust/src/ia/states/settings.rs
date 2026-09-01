use crate::ia::helpers::frame_hint_from_node;
use crate::ia::selectors::query_selector;
use crate::ia::types::*;

/// Find the Settings frame in the a11y tree.
fn find_settings_frame<'a>(a11y: &'a A11yNode) -> Option<&'a A11yNode> {
    query_selector(a11y, r#"frame[name="Settings"]"#)
}

/// Settings modal state — Settings frame with confirmation dialog (OK + Cancel).
struct SettingsModalState;

impl IAState for SettingsModalState {
    fn fsm(&self) -> &str {
        "settings"
    }
    fn id(&self) -> &str {
        "settings_modal"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        let frame = match find_settings_frame(args.a11y) {
            Some(f) => f,
            None => {
                return Ok(IdentifyResult {
                    identified: false,
                    frame: None,
                })
            }
        };

        let has_ok = query_selector(frame, r#"push-button[name="OK"]"#).is_some();
        let has_cancel = query_selector(frame, r#"push-button[name="Cancel"]"#).is_some();

        Ok(IdentifyResult {
            identified: has_ok && has_cancel,
            frame: frame_hint_from_node(frame),
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let mut state = args.prev.clone();
        if let Some(frame) = find_settings_frame(args.a11y) {
            state.settings = Some(SettingsState {
                logout_button_bounds: query_selector(frame, r#"push-button[name="Log Out"]"#)
                    .and_then(|n| n.bounds.clone()),
                my_account_bounds: query_selector(frame, r#"push-button[name="My Account"]"#)
                    .and_then(|n| n.bounds.clone()),
                modal_ok_bounds: query_selector(frame, r#"push-button[name="OK"]"#)
                    .and_then(|n| n.bounds.clone()),
                modal_cancel_bounds: query_selector(frame, r#"push-button[name="Cancel"]"#)
                    .and_then(|n| n.bounds.clone()),
            });
        }
        state
    }
}

/// Settings state — Settings frame open, no confirmation modal.
struct SettingsStateImpl;

impl IAState for SettingsStateImpl {
    fn fsm(&self) -> &str {
        "settings"
    }
    fn id(&self) -> &str {
        "settings"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        let frame = match find_settings_frame(args.a11y) {
            Some(f) => f,
            None => {
                return Ok(IdentifyResult {
                    identified: false,
                    frame: None,
                })
            }
        };

        let has_ok = query_selector(frame, r#"push-button[name="OK"]"#).is_some();
        let has_cancel = query_selector(frame, r#"push-button[name="Cancel"]"#).is_some();

        Ok(IdentifyResult {
            identified: !has_ok && !has_cancel,
            frame: frame_hint_from_node(frame),
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let mut state = args.prev.clone();
        if let Some(frame) = find_settings_frame(args.a11y) {
            state.settings = Some(SettingsState {
                logout_button_bounds: query_selector(frame, r#"push-button[name="Log Out"]"#)
                    .and_then(|n| n.bounds.clone()),
                my_account_bounds: query_selector(frame, r#"push-button[name="My Account"]"#)
                    .and_then(|n| n.bounds.clone()),
                modal_ok_bounds: None,
                modal_cancel_bounds: None,
            });
        }
        state
    }
}

/// Settings states — settings_modal first so it takes priority when modal is present.
pub static SETTINGS_STATES: std::sync::LazyLock<Vec<Box<dyn IAState>>> =
    std::sync::LazyLock::new(|| vec![Box::new(SettingsModalState), Box::new(SettingsStateImpl)]);

#[cfg(test)]
mod tests {
    use crate::ia::identify_states;
    use crate::ia::types::A11yNode;

    fn load_fixture(name: &str) -> A11yNode {
        let json = match name {
            "chat_view.json" => include_str!("test_fixtures/chat_view.json"),
            "more_menu_open.json" => include_str!("test_fixtures/more_menu_open.json"),
            "settings_open.json" => include_str!("test_fixtures/settings_open.json"),
            "settings_confirm.json" => include_str!("test_fixtures/settings_confirm.json"),
            _ => panic!("Unknown fixture: {name}"),
        };
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn test_chat_view_no_settings() {
        let a11y = load_fixture("chat_view.json");
        let states = identify_states(&a11y, "");
        assert_eq!(states.main_window.as_ref().unwrap().state_id, "chat");
        assert!(states.settings.is_none());
        assert!(states.popup.is_none());
    }

    #[test]
    fn test_more_menu_not_a_popup() {
        let a11y = load_fixture("more_menu_open.json");
        let states = identify_states(&a11y, "");
        assert_eq!(states.main_window.as_ref().unwrap().state_id, "chat");
        // More menu is NOT identified as a popup
        assert!(states.popup.is_none());
        assert!(states.settings.is_none());
    }

    #[test]
    fn test_settings_identified() {
        let a11y = load_fixture("settings_open.json");
        let states = identify_states(&a11y, "");
        assert_eq!(states.main_window.as_ref().unwrap().state_id, "chat");
        assert_eq!(states.settings.as_ref().unwrap().state_id, "settings");
        // popup_confirm must NOT false-match inside Settings frame
        assert!(states.popup.is_none());
    }

    #[test]
    fn test_settings_confirm_modal() {
        let a11y = load_fixture("settings_confirm.json");
        let states = identify_states(&a11y, "");
        assert_eq!(states.main_window.as_ref().unwrap().state_id, "chat");
        assert_eq!(states.settings.as_ref().unwrap().state_id, "settings_modal");
        // popup must NOT false-match
        assert!(states.popup.is_none());
    }
}
