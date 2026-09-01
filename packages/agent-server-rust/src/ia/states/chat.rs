use super::base::extract_window_control_bounds;
use crate::ia::helpers::{extract_active_chat_id, find_frame_for};
use crate::ia::selectors::query_selector;
use crate::ia::types::*;

/// Find the More menu dropdown filler (filler containing Settings + Lock buttons).
/// Searches recursively since the filler may be nested under application > ...
fn find_more_menu_filler<'a>(node: &'a A11yNode) -> Option<&'a A11yNode> {
    if let Some(children) = &node.children {
        for child in children {
            if child.role == "filler"
                && query_selector(child, r#"push-button[name="Settings"]"#).is_some()
                && query_selector(child, r#"push-button[name="Lock"]"#).is_some()
            {
                return Some(child);
            }
            // Recurse, but skip frame nodes (the filler is outside the WeChat frame)
            if child.role != "frame" {
                if let Some(found) = find_more_menu_filler(child) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Extract More button bounds and Settings menu item bounds (when More dropdown is open).
fn extract_more_menu_bounds(a11y: &A11yNode, state: &mut AppState) {
    // More button in Navigation toolbar
    state.main_window.more_button_bounds = query_selector(
        a11y,
        r#"tool-bar[name="Navigation"] push-button[name="More"]"#,
    )
    .and_then(|n| n.bounds.clone());

    // Settings menu item from More dropdown (only when dropdown is open)
    state.main_window.settings_menu_item_bounds = find_more_menu_filler(a11y)
        .and_then(|filler| query_selector(filler, r#"push-button[name="Settings"]"#))
        .and_then(|n| n.bounds.clone());
}

fn is_chat_view(a11y: &A11yNode) -> bool {
    let main_btn = query_selector(a11y, r#"push-button[name="Weixin"]"#)
        .or_else(|| query_selector(a11y, r#"push-button[name="WeChat"]"#));
    if main_btn.is_none() {
        return false;
    }
    let has_contacts = query_selector(a11y, r#"push-button[name="Contacts"]"#).is_some();
    let has_chats = query_selector(a11y, r#"list[name="Chats"]"#).is_some();
    has_contacts && has_chats
}

fn find_selected_chat_item(a11y: &A11yNode) -> Option<&A11yNode> {
    let chat_list = query_selector(a11y, r#"list[name="Chats"]"#)?;
    chat_list.children.as_ref()?.iter().find(|item| {
        item.states
            .as_ref()
            .map(|s| s.iter().any(|st| st == "SELECTED"))
            .unwrap_or(false)
    })
}

/// Chat state — no chat selected.
struct ChatState;

impl IAState for ChatState {
    fn fsm(&self) -> &str {
        "mainWindow"
    }
    fn id(&self) -> &str {
        "chat"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        if !is_chat_view(args.a11y) {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }
        if find_selected_chat_item(args.a11y).is_some() {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }
        Ok(IdentifyResult {
            identified: true,
            frame: find_frame_for(args.a11y, r#"list[name="Chats"]"#),
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let wb = extract_window_control_bounds(None);
        let mut state = args.prev.clone();
        state.main_window.view = MainWindowView::Chat;
        state.main_window.is_logged_in = true;
        state.main_window.opened_chat_name = None;
        state.main_window.opened_chat_is_group = None;
        state.main_window.selected_chat_bounds = None;
        state.main_window.close_button_bounds = wb.close_button_bounds;
        state.main_window.minimize_button_bounds = wb.minimize_button_bounds;
        state.main_window.maximize_button_bounds = wb.maximize_button_bounds;
        extract_more_menu_bounds(args.a11y, &mut state);
        state
    }
}

/// Chat open state — a chat is selected and showing messages.
struct ChatOpenState;

impl IAState for ChatOpenState {
    fn fsm(&self) -> &str {
        "mainWindow"
    }
    fn id(&self) -> &str {
        "chat_open"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        if !is_chat_view(args.a11y) {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }
        if find_selected_chat_item(args.a11y).is_none() {
            return Ok(IdentifyResult {
                identified: false,
                frame: None,
            });
        }
        Ok(IdentifyResult {
            identified: true,
            frame: find_frame_for(args.a11y, r#"list[name="Chats"]"#),
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let wb = extract_window_control_bounds(None);
        let a11y = args.a11y;

        // Extract opened chat name from header area
        let chat_list = query_selector(a11y, r#"list[name="Chats"]"#);
        let chat_list_right = chat_list
            .and_then(|c| c.bounds.as_ref())
            .map(|b| b.x + b.width)
            .unwrap_or(272.0);

        // Collect all labels and find one to the right of chat list, near top
        let mut all_labels = Vec::new();
        collect_labels(a11y, &mut all_labels);

        let header_label = all_labels.iter().find(|label| {
            if let Some(b) = &label.bounds {
                b.x > chat_list_right
                    && b.y < 70.0
                    && !label.name.is_empty()
                    && !label.name.contains("Send")
            } else {
                false
            }
        });

        let raw_name = header_label.map(|l| l.name.clone());

        // Detect group via "(n)" suffix
        let member_count_re = regex::Regex::new(r"\((\d+)\)$").unwrap();
        let is_group = raw_name
            .as_ref()
            .map(|n| member_count_re.is_match(n))
            .unwrap_or(false);
        let opened_chat_name = if is_group {
            raw_name
                .as_ref()
                .map(|n| member_count_re.replace(n, "").trim().to_string())
        } else {
            raw_name.clone()
        };

        // Selected chat bounds
        let selected_item = find_selected_chat_item(a11y);
        let selected_chat_bounds = selected_item.and_then(|item| item.bounds.clone());

        // Active chat ID
        let selected_chat_id =
            extract_active_chat_id(a11y).or_else(|| args.prev.main_window.selected_chat_id.clone());

        let mut state = args.prev.clone();
        state.main_window.view = MainWindowView::ChatOpen;
        state.main_window.is_logged_in = true;
        state.main_window.selected_chat_id = selected_chat_id;
        state.main_window.opened_chat_name = opened_chat_name;
        state.main_window.opened_chat_is_group = Some(is_group);
        state.main_window.selected_chat_bounds = selected_chat_bounds;
        state.main_window.close_button_bounds = wb.close_button_bounds;
        state.main_window.minimize_button_bounds = wb.minimize_button_bounds;
        state.main_window.maximize_button_bounds = wb.maximize_button_bounds;
        extract_more_menu_bounds(args.a11y, &mut state);
        state
    }
}

fn collect_labels<'a>(node: &'a A11yNode, out: &mut Vec<&'a A11yNode>) {
    if node.role == "label" && !node.name.is_empty() && node.bounds.is_some() {
        out.push(node);
    }
    if let Some(children) = &node.children {
        for child in children {
            collect_labels(child, out);
        }
    }
}

pub static CHAT_STATES: std::sync::LazyLock<Vec<Box<dyn IAState>>> =
    std::sync::LazyLock::new(|| vec![Box::new(ChatState), Box::new(ChatOpenState)]);
