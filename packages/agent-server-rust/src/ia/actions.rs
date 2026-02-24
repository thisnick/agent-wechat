use super::types::{Action, Bounds};

// ============================================
// Common Actions
// ============================================

pub fn wait(ms: u64) -> Action {
    Action::Wait { ms }
}

pub fn wait_short() -> Action {
    Action::Wait { ms: 200 }
}

pub fn wait_long() -> Action {
    Action::Wait { ms: 1000 }
}

// ============================================
// Window Control Actions
// ============================================

pub fn maximize() -> Action {
    Action::ClickSelector {
        selector: r#"tool-bar push-button[name="Maximize"]"#.to_string(),
    }
}

pub fn minimize() -> Action {
    Action::ClickSelector {
        selector: r#"tool-bar push-button[name="Minimize"]"#.to_string(),
    }
}

pub fn close_window() -> Action {
    Action::ClickSelector {
        selector: r#"tool-bar push-button[name="Disable"]"#.to_string(),
    }
}

// ============================================
// Login Actions
// ============================================

pub fn click_login() -> Action {
    Action::ClickSelector {
        selector: r#"push-button[name=/^(Log In|Open WeChat)$/]"#.to_string(),
    }
}

pub fn click_switch_account() -> Action {
    Action::ClickSelector {
        selector: r#"push-button[name="Switch Account"]"#.to_string(),
    }
}

// ============================================
// Popup Actions
// ============================================

pub fn dismiss_popup() -> Action {
    Action::ClickSelector {
        selector: r#"push-button[name=/OK|Confirm|确定|确认/i]"#.to_string(),
    }
}

pub fn cancel_popup() -> Action {
    Action::ClickSelector {
        selector: r#"push-button[name=/Cancel|取消/i]"#.to_string(),
    }
}

// ============================================
// Helpers
// ============================================

pub fn click_at(x: f64, y: f64) -> Action {
    Action::ClickCoords { x, y }
}

pub fn click_bounds(bounds: &Bounds) -> Action {
    click_at(
        (bounds.x + bounds.width / 2.0).round(),
        (bounds.y + bounds.height / 2.0).round(),
    )
}

pub fn click_selector(selector: &str) -> Action {
    Action::ClickSelector {
        selector: selector.to_string(),
    }
}

pub fn click_back() -> Action {
    click_selector(r#"push-button[name="Back"]"#)
}

pub fn sequence(actions: Vec<Action>) -> Action {
    Action::Sequence { actions }
}
