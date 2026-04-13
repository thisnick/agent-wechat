use super::base::extract_window_control_bounds;
use crate::ia::helpers::find_frame_for;
use crate::ia::selectors::query_selector;
use crate::ia::types::*;
use crate::tools::qr::decode_qr_from_base64;

/// login_qr: WeChat shows a QR code to scan.
struct LoginQrState;

impl IAState for LoginQrState {
    fn fsm(&self) -> &str {
        "mainWindow"
    }
    fn id(&self) -> &str {
        "login_qr"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        let scan_label = query_selector(args.a11y, r#"label[name*="Scan to log in"]"#);
        if scan_label.is_none() {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }

        let has_transfer =
            query_selector(args.a11y, r#"push-button[name*="Transfer files only"]"#).is_some();
        if !has_transfer {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }

        let qr = decode_qr_from_base64(args.screenshot);
        let has_wechat_qr = qr
            .as_ref()
            .map(|r| r.data.starts_with("http://weixin.qq.com/x/"))
            .unwrap_or(false);
        if !has_wechat_qr {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }

        Ok(IdentifyResult {
            identified: true,
            frame: find_frame_for(args.a11y, r#"label[name*="Scan to log in"]"#),
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let screenshot_b64 = base64::engine::general_purpose::STANDARD.encode(args.screenshot);
        let qr = decode_qr_from_base64(&screenshot_b64);
        let wb = extract_window_control_bounds(None);

        let mut state = args.prev.clone();
        state.main_window.view = MainWindowView::LoginQr;
        state.main_window.is_logged_in = false;
        if let Some(qr_result) = qr {
            state.main_window.qr_data = Some(qr_result.data);
            state.main_window.qr_binary_data = Some(qr_result.binary_data);
        }
        state.main_window.close_button_bounds = wb.close_button_bounds;
        state.main_window.minimize_button_bounds = wb.minimize_button_bounds;
        state.main_window.maximize_button_bounds = wb.maximize_button_bounds;
        state
    }
}

/// login_account: WeChat shows a saved account to confirm.
struct LoginAccountState;

impl IAState for LoginAccountState {
    fn fsm(&self) -> &str {
        "mainWindow"
    }
    fn id(&self) -> &str {
        "login_account"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        let log_in_btn = query_selector(args.a11y, r#"push-button[name="Log In"]"#)
            .or_else(|| query_selector(args.a11y, r#"push-button[name="Open WeChat"]"#));
        if log_in_btn.is_none() {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }

        let has_switch =
            query_selector(args.a11y, r#"push-button[name="Switch Account"]"#).is_some();
        if !has_switch {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }

        Ok(IdentifyResult {
            identified: true,
            frame: find_frame_for(args.a11y, r#"push-button[name="Switch Account"]"#),
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let name_el = query_selector(args.a11y, r#"label[name*="Current User"]"#);
        let account_name = name_el
            .map(|n| n.name.replace("Current User", "").trim().to_string())
            .filter(|s| !s.is_empty());

        let mut state = args.prev.clone();
        state.main_window.view = MainWindowView::LoginAccount;
        state.main_window.is_logged_in = false;
        state.main_window.account_name = account_name;
        state
    }
}

/// login_phone_confirm: User needs to confirm on phone.
struct LoginPhoneConfirmState;

impl IAState for LoginPhoneConfirmState {
    fn fsm(&self) -> &str {
        "mainWindow"
    }
    fn id(&self) -> &str {
        "login_phone_confirm"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        let confirm = query_selector(
            args.a11y,
            r#"label[name=/Comfirm on phone|Confirm.*phone|手机确认/i]"#,
        );
        Ok(IdentifyResult {
            identified: confirm.is_some(),
            frame: if confirm.is_some() {
                find_frame_for(
                    args.a11y,
                    r#"label[name=/Comfirm on phone|Confirm.*phone|手机确认/i]"#,
                )
            } else {
                None
            },
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let mut state = args.prev.clone();
        state.main_window.view = MainWindowView::LoginPhoneConfirm;
        state.main_window.is_logged_in = false;
        state
    }
}

/// login_loading: Transitional state while logging in.
struct LoginLoadingState;

impl IAState for LoginLoadingState {
    fn fsm(&self) -> &str {
        "mainWindow"
    }
    fn id(&self) -> &str {
        "login_loading"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        // Case 1: "Entering" or "Loading X%" labels
        if query_selector(args.a11y, r#"label[name="Entering"]"#).is_some() {
            return Ok(IdentifyResult {
                identified: true,
                frame: find_frame_for(args.a11y, r#"label[name="Entering"]"#),
            });
        }
        if query_selector(args.a11y, r#"label[name*="Loading"]"#).is_some() {
            return Ok(IdentifyResult {
                identified: true,
                frame: find_frame_for(args.a11y, r#"label[name*="Loading"]"#),
            });
        }

        // Case 2: Nav buttons but no Chats list
        let main_btn = query_selector(args.a11y, r#"push-button[name="Weixin"]"#)
            .or_else(|| query_selector(args.a11y, r#"push-button[name="WeChat"]"#));
        let has_contacts = query_selector(args.a11y, r#"push-button[name="Contacts"]"#).is_some();
        let has_chats = query_selector(args.a11y, r#"list[name="Chats"]"#).is_some();

        if main_btn.is_some() && has_contacts && !has_chats {
            return Ok(IdentifyResult {
                identified: true,
                frame: find_frame_for(args.a11y, r#"push-button[name="Contacts"]"#),
            });
        }

        Ok(IdentifyResult {
            identified: false,
            frame: None,
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let mut state = args.prev.clone();
        state.main_window.view = MainWindowView::LoginLoading;
        state.main_window.is_logged_in = false;
        state
    }
}

/// network_proxy_settings: WeChat network proxy settings page.
struct NetworkProxySettingsState;

impl IAState for NetworkProxySettingsState {
    fn fsm(&self) -> &str {
        "mainWindow"
    }
    fn id(&self) -> &str {
        "network_proxy_settings"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        let title = query_selector(args.a11y, r#"label[name="Network proxy settings"]"#);
        let checkbox = query_selector(args.a11y, r#"check-box[name="Use proxy"]"#);
        let identified = title.is_some() && checkbox.is_some();
        Ok(IdentifyResult {
            identified,
            frame: if identified {
                find_frame_for(args.a11y, r#"label[name="Network proxy settings"]"#)
            } else {
                None
            },
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let checkbox = query_selector(args.a11y, r#"check-box[name="Use proxy"]"#);
        let is_checked = checkbox
            .and_then(|n| n.states.as_ref())
            .map(|s| s.iter().any(|st| st == "CHECKED"))
            .unwrap_or(false);

        let has_discard = query_selector(args.a11y, r#"label[name="Discard changes?"]"#).is_some();

        let mut state = args.prev.clone();
        state.main_window.view = MainWindowView::NetworkProxySettings;
        state.main_window.proxy_enabled = Some(is_checked);
        state.main_window.proxy_save_failed = Some(has_discard);
        state
    }
}

use base64::Engine;

/// All login states (order matters — first match wins).
pub static LOGIN_STATES: std::sync::LazyLock<Vec<Box<dyn IAState>>> =
    std::sync::LazyLock::new(|| {
        vec![
            Box::new(NetworkProxySettingsState),
            Box::new(LoginQrState),
            Box::new(LoginAccountState),
            Box::new(LoginPhoneConfirmState),
            Box::new(LoginLoadingState),
        ]
    });
