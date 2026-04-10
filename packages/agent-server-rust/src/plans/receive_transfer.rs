use super::Plan;
use crate::ia::actions;
use crate::ia::helpers::{find_frame_for, frame_hint_from_node};
use crate::ia::selectors::query_selector;
use crate::ia::types::*;
use crate::tools::chat_select::{open_chat, OpenChatResult};

const ACCEPT_SELECTOR: &str = r#"push-button[name=/^(确认收钱|Accept|Receive)$/]"#;
const SUCCESS_SELECTOR: &str =
    r#"*[name=/已(收钱|接收|领取)|收款成功|接收成功|领取成功|已存入|Received|Success/i]"#;
const DIALOG_CLOSE_SELECTOR: &str = r#"push-button[name=/^(Disable|Close|关闭|完成|Done)$/]"#;
const MAIN_CHAT_SELECTOR: &str = r#"list[name="Chats"]"#;

pub struct ReceiveTransferPlan;

pub struct ReceiveTransferParams {
    pub chat_id: String,
    pub transaction_id: Option<String>,
    pub amount_text: Option<String>,
}

pub enum ReceiveTransferPhase {
    OpeningChat,
    ClickingTransfer,
    ClickingReceive,
    WaitingSuccess,
    ClosingSuccess,
    Done,
}

pub struct ReceiveTransferPlanState {
    pub phase: ReceiveTransferPhase,
    pub open_result: Option<OpenChatResult>,
    pub find_attempts: u32,
    pub receive_attempts: u32,
    pub success_attempts: u32,
    pub close_attempts: u32,
    pub received: bool,
}

fn normalize_amount_text(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect()
}

fn is_transfer_item_name(name: &str, expected_amount: Option<&str>) -> bool {
    let lower = name.to_lowercase();
    let transfer_like = name.contains("微信转账")
        || lower.contains("wechat transfer")
        || lower.contains("confirm receipt")
        || name.contains("确认收钱")
        || name.contains("收钱");

    if !transfer_like {
        return false;
    }

    if let Some(expected_amount) = expected_amount {
        let hint = normalize_amount_text(expected_amount);
        if !hint.is_empty() {
            return normalize_amount_text(name).contains(&hint);
        }
    }

    true
}

fn find_transfer_message<'a>(
    a11y: &'a A11yNode,
    expected_amount: Option<&str>,
) -> Option<&'a A11yNode> {
    let list = query_selector(a11y, r#"list[name="Messages"]"#)?;
    let children = list.children.as_ref()?;
    children
        .iter()
        .rev()
        .find(|node| node.role == "list-item" && is_transfer_item_name(&node.name, expected_amount))
}

fn find_deepest_frame_with<'a>(node: &'a A11yNode, selector: &str) -> Option<&'a A11yNode> {
    if query_selector(node, selector).is_none() {
        return None;
    }

    if let Some(children) = &node.children {
        for child in children {
            if let Some(frame) = find_deepest_frame_with(child, selector) {
                return Some(frame);
            }
        }
    }

    if node.role == "frame" {
        Some(node)
    } else {
        None
    }
}

fn find_transfer_dialog_frame<'a>(a11y: &'a A11yNode, selector: &str) -> Option<&'a A11yNode> {
    find_deepest_frame_with(a11y, selector).filter(|frame| {
        query_selector(frame, MAIN_CHAT_SELECTOR).is_none()
            && query_selector(frame, r#"list[name="Messages"]"#).is_none()
    })
}

fn find_accept_button(a11y: &A11yNode) -> Option<(&A11yNode, Option<FrameHint>)> {
    if let Some(frame) = find_transfer_dialog_frame(a11y, ACCEPT_SELECTOR) {
        return query_selector(frame, ACCEPT_SELECTOR)
            .map(|btn| (btn, frame_hint_from_node(frame)));
    }

    query_selector(a11y, ACCEPT_SELECTOR).map(|btn| (btn, find_frame_for(a11y, ACCEPT_SELECTOR)))
}

fn has_receive_success(a11y: &A11yNode) -> bool {
    find_transfer_dialog_frame(a11y, SUCCESS_SELECTOR).is_some()
}

fn find_success_close_button(a11y: &A11yNode) -> Option<(&A11yNode, Option<FrameHint>)> {
    let frame = find_transfer_dialog_frame(a11y, SUCCESS_SELECTOR)?;
    query_selector(frame, DIALOG_CLOSE_SELECTOR).map(|btn| (btn, frame_hint_from_node(frame)))
}

#[async_trait::async_trait]
impl Plan for ReceiveTransferPlan {
    type PlanState = ReceiveTransferPlanState;
    type Params = ReceiveTransferParams;

    fn id(&self) -> &str {
        "receive_transfer"
    }

    fn initial_plan_state(&self) -> ReceiveTransferPlanState {
        ReceiveTransferPlanState {
            phase: ReceiveTransferPhase::OpeningChat,
            open_result: None,
            find_attempts: 0,
            receive_attempts: 0,
            success_attempts: 0,
            close_attempts: 0,
            received: false,
        }
    }

    fn is_goal_reached(&self, _state: &AppState, plan_state: &ReceiveTransferPlanState) -> bool {
        matches!(plan_state.phase, ReceiveTransferPhase::Done) && plan_state.received
    }

    async fn select_action(
        &self,
        state: &AppState,
        params: &ReceiveTransferParams,
        identified: &IdentifiedStates,
        plan_state: &mut ReceiveTransferPlanState,
        a11y: &A11yNode,
        _session_id: &str,
    ) -> Option<SelectedAction> {
        let main_state_id = identified.main_window.as_ref().map(|m| m.state_id.as_str());

        // Dismiss other popups if unexpected
        if state.popup.is_some()
            && identified.popup.is_some()
            && matches!(
                plan_state.phase,
                ReceiveTransferPhase::ClickingTransfer | ReceiveTransferPhase::OpeningChat
            )
        {
            return Some(SelectedAction {
                action: actions::dismiss_popup(),
                frame: identified
                    .main_window
                    .as_ref()
                    .and_then(|m| m.frame.clone()),
            });
        }

        loop {
            match plan_state.phase {
                ReceiveTransferPhase::OpeningChat => {
                    if main_state_id != Some("chat") && main_state_id != Some("chat_open") {
                        return None; // Wait for app to be ready
                    }

                    let chat_list_item = query_selector(a11y, r#"list[name="Chats"] > list-item"#);
                    let click_xy = chat_list_item.and_then(|item| {
                        item.bounds.as_ref().map(|b| {
                            (
                                (b.x + b.width / 2.0).round(),
                                (b.y + b.height / 2.0).round(),
                            )
                        })
                    });

                    let force = main_state_id == Some("chat");
                    let result = open_chat(&params.chat_id, force, click_xy).await;

                    if !result.ok {
                        return None; // open_chat failed
                    }

                    let skipped = result.skipped.unwrap_or(false);
                    plan_state.open_result = Some(result);
                    plan_state.phase = ReceiveTransferPhase::ClickingTransfer;

                    if !skipped {
                        return Some(SelectedAction {
                            action: actions::wait_short(),
                            frame: identified
                                .main_window
                                .as_ref()
                                .and_then(|m| m.frame.clone()),
                        });
                    }
                    continue;
                }

                ReceiveTransferPhase::ClickingTransfer => {
                    if main_state_id != Some("chat_open") {
                        return None;
                    }

                    let transfer_node = find_transfer_message(a11y, params.amount_text.as_deref());
                    if let Some(node) = transfer_node {
                        if let Some(bounds) = &node.bounds {
                            plan_state.phase = ReceiveTransferPhase::ClickingReceive;
                            return Some(SelectedAction {
                                action: actions::sequence(vec![
                                    actions::click_bounds(bounds),
                                    actions::wait_short(),
                                ]),
                                frame: identified
                                    .main_window
                                    .as_ref()
                                    .and_then(|m| m.frame.clone()),
                            });
                        }
                    }

                    // If not found, perhaps try scrolling up? Not implemented here, assume it's visible or recent.
                    plan_state.find_attempts += 1;
                    if plan_state.find_attempts > 10 {
                        // Abort if we can't find it after a while
                        return None;
                    }

                    return Some(SelectedAction {
                        action: actions::wait_short(),
                        frame: identified
                            .main_window
                            .as_ref()
                            .and_then(|m| m.frame.clone()),
                    });
                }

                ReceiveTransferPhase::ClickingReceive => {
                    if has_receive_success(a11y) {
                        plan_state.phase = ReceiveTransferPhase::ClosingSuccess;
                        continue;
                    }

                    if let Some((btn, frame)) = find_accept_button(a11y) {
                        if let Some(bounds) = &btn.bounds {
                            if plan_state.receive_attempts >= 5 {
                                return None;
                            }
                            plan_state.receive_attempts += 1;
                            plan_state.phase = ReceiveTransferPhase::WaitingSuccess;
                            return Some(SelectedAction {
                                action: actions::sequence(vec![
                                    actions::click_bounds(bounds),
                                    actions::wait_long(),
                                ]),
                                frame: frame.or_else(|| {
                                    identified
                                        .main_window
                                        .as_ref()
                                        .and_then(|m| m.frame.clone())
                                }),
                            });
                        }
                    }

                    plan_state.receive_attempts += 1;
                    if plan_state.receive_attempts > 20 {
                        // Timeout waiting for popup
                        return None;
                    }

                    return Some(SelectedAction {
                        action: actions::wait_short(),
                        frame: identified
                            .main_window
                            .as_ref()
                            .and_then(|m| m.frame.clone()),
                    });
                }

                ReceiveTransferPhase::WaitingSuccess => {
                    if has_receive_success(a11y) {
                        plan_state.phase = ReceiveTransferPhase::ClosingSuccess;
                        continue;
                    }

                    plan_state.success_attempts += 1;
                    if plan_state.success_attempts > 30 {
                        return None;
                    }

                    return Some(SelectedAction {
                        action: actions::wait_long(),
                        frame: find_frame_for(a11y, ACCEPT_SELECTOR).or_else(|| {
                            identified
                                .main_window
                                .as_ref()
                                .and_then(|m| m.frame.clone())
                        }),
                    });
                }

                ReceiveTransferPhase::ClosingSuccess => {
                    if let Some((btn, frame)) = find_success_close_button(a11y) {
                        if let Some(bounds) = &btn.bounds {
                            plan_state.received = true;
                            plan_state.phase = ReceiveTransferPhase::Done;
                            return Some(SelectedAction {
                                action: actions::sequence(vec![
                                    actions::click_bounds(bounds),
                                    actions::wait_short(),
                                ]),
                                frame: frame.or_else(|| {
                                    identified
                                        .main_window
                                        .as_ref()
                                        .and_then(|m| m.frame.clone())
                                }),
                            });
                        }
                    }

                    plan_state.close_attempts += 1;
                    if plan_state.close_attempts > 10 {
                        return None;
                    }

                    return Some(SelectedAction {
                        action: actions::wait_short(),
                        frame: identified
                            .main_window
                            .as_ref()
                            .and_then(|m| m.frame.clone()),
                    });
                }

                ReceiveTransferPhase::Done => return None,
            }
        }
    }
}
