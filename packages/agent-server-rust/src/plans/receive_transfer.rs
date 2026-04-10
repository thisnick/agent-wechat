use super::Plan;
use crate::ia::actions;
use crate::ia::helpers::{find_frame_for, frame_hint_from_node};
use crate::ia::selectors::query_selector;
use crate::ia::types::*;
use crate::tools::chat_select::{open_chat, OpenChatResult};

const ACCEPT_SELECTOR: &str = r#"push-button[name=/^(确认收钱|Accept|Receive)$/]"#;
const SUCCESS_SELECTOR: &str =
    r#"*[name=/已(收钱|接收|领取)|收款成功|接收成功|领取成功|已存入|Accepted|Received|Success/i]"#;
const DIALOG_CLOSE_SELECTOR: &str =
    r#"push-button[name=/^(Disable|Close|关闭|完成|Done|OK|确定|确认|好|好的|知道了)$/]"#;
const WINDOW_CLOSE_SELECTOR: &str = r#"tool-bar push-button[name="Disable"]"#;
const MAIN_CHAT_SELECTOR: &str = r#"list[name="Chats"]"#;

pub struct ReceiveTransferPlan;

pub struct ReceiveTransferParams {
    pub chat_id: String,
    pub transaction_id: Option<String>,
    pub amount_text: Option<String>,
    pub is_self: bool,
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

fn is_receivable_transfer_item_name(name: &str, expected_amount: Option<&str>) -> bool {
    let lower = name.to_lowercase();
    let transfer_like = name.contains("微信转账")
        || lower.contains("wechat transfer")
        || lower.contains("confirm receipt")
        || name.contains("确认收钱")
        || name.contains("确认收款")
        || name.contains("待收钱")
        || name.contains("待接收")
        || name.contains("收钱");

    if !transfer_like {
        return false;
    }

    let received_like = lower.contains("accepted")
        || lower.contains("received")
        || name.contains("已收")
        || name.contains("已接收")
        || name.contains("已领取")
        || name.contains("已存入");
    let receivable_like = lower.contains("confirm receipt")
        || (lower.contains("receive") && !received_like)
        || name.contains("确认收钱")
        || name.contains("确认收款")
        || name.contains("待收钱")
        || name.contains("待接收")
        || (name.contains("收钱") && !received_like);

    if !receivable_like || received_like {
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
    children.iter().rev().find(|node| {
        node.role == "list-item" && is_receivable_transfer_item_name(&node.name, expected_amount)
    })
}

fn click_transfer_card(bounds: &Bounds, is_self: bool) -> Action {
    let max_offset = (bounds.width - 24.0).max(bounds.width / 2.0);
    let min_offset = 160.0_f64.min(max_offset);
    let x_offset = (bounds.width * 0.24).clamp(min_offset, max_offset);
    let x = if is_self {
        bounds.x + bounds.width - x_offset
    } else {
        bounds.x + x_offset
    };
    actions::click_at(x.round(), (bounds.y + bounds.height / 2.0).round())
}

fn is_transfer_dialog_frame(frame: &A11yNode) -> bool {
    query_selector(frame, MAIN_CHAT_SELECTOR).is_none()
        && query_selector(frame, r#"list[name="Messages"]"#).is_none()
}

fn find_transfer_dialog_frame<'a>(a11y: &'a A11yNode, selector: &str) -> Option<&'a A11yNode> {
    fn walk<'a>(node: &'a A11yNode, selector: &str) -> Option<&'a A11yNode> {
        let mut best: Option<&'a A11yNode> = None;

        if let Some(children) = &node.children {
            for child in children {
                if let Some(frame) = walk(child, selector) {
                    best = Some(frame);
                }
            }
        }

        if best.is_some() {
            return best;
        }

        if node.role == "frame"
            && query_selector(node, selector).is_some()
            && is_transfer_dialog_frame(node)
        {
            return Some(node);
        }

        None
    }

    walk(a11y, selector)
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

fn find_close_button_in_frame(frame: &A11yNode) -> Option<(&A11yNode, Option<FrameHint>)> {
    query_selector(frame, DIALOG_CLOSE_SELECTOR)
        .or_else(|| query_selector(frame, WINDOW_CLOSE_SELECTOR))
        .map(|btn| (btn, frame_hint_from_node(frame)))
}

fn find_any_transfer_dialog_close_button(
    a11y: &A11yNode,
) -> Option<(&A11yNode, Option<FrameHint>)> {
    if let Some(frame) = find_transfer_dialog_frame(a11y, DIALOG_CLOSE_SELECTOR) {
        if let Some(button) = find_close_button_in_frame(frame) {
            return Some(button);
        }
    }

    let frame = find_transfer_dialog_frame(a11y, WINDOW_CLOSE_SELECTOR)?;
    find_close_button_in_frame(frame)
}

fn find_success_close_button(a11y: &A11yNode) -> Option<(&A11yNode, Option<FrameHint>)> {
    let frame = find_transfer_dialog_frame(a11y, SUCCESS_SELECTOR)?;
    find_close_button_in_frame(frame)
}

fn receipt_completed_in_chat(
    a11y: &A11yNode,
    main_state_id: Option<&str>,
    expected_amount: Option<&str>,
) -> bool {
    main_state_id == Some("chat_open")
        && query_selector(a11y, r#"list[name="Messages"]"#).is_some()
        && find_accept_button(a11y).is_none()
        && find_transfer_message(a11y, expected_amount).is_none()
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
                                    click_transfer_card(bounds, params.is_self),
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

                    if receipt_completed_in_chat(a11y, main_state_id, params.amount_text.as_deref())
                    {
                        plan_state.received = true;
                        plan_state.phase = ReceiveTransferPhase::Done;
                        return Some(SelectedAction {
                            action: actions::wait_short(),
                            frame: identified
                                .main_window
                                .as_ref()
                                .and_then(|m| m.frame.clone()),
                        });
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

                    if find_accept_button(a11y).is_none()
                        && find_any_transfer_dialog_close_button(a11y).is_some()
                    {
                        plan_state.phase = ReceiveTransferPhase::ClosingSuccess;
                        continue;
                    }

                    if receipt_completed_in_chat(a11y, main_state_id, params.amount_text.as_deref())
                    {
                        plan_state.received = true;
                        plan_state.phase = ReceiveTransferPhase::Done;
                        return Some(SelectedAction {
                            action: actions::wait_short(),
                            frame: identified
                                .main_window
                                .as_ref()
                                .and_then(|m| m.frame.clone()),
                        });
                    }

                    plan_state.success_attempts += 1;
                    if plan_state.success_attempts > 20 {
                        return None;
                    }

                    return Some(SelectedAction {
                        action: actions::wait_short(),
                        frame: find_frame_for(a11y, ACCEPT_SELECTOR).or_else(|| {
                            identified
                                .main_window
                                .as_ref()
                                .and_then(|m| m.frame.clone())
                        }),
                    });
                }

                ReceiveTransferPhase::ClosingSuccess => {
                    if let Some((btn, frame)) = find_success_close_button(a11y)
                        .or_else(|| find_any_transfer_dialog_close_button(a11y))
                    {
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

                    if receipt_completed_in_chat(a11y, main_state_id, params.amount_text.as_deref())
                    {
                        plan_state.received = true;
                        plan_state.phase = ReceiveTransferPhase::Done;
                        return Some(SelectedAction {
                            action: actions::wait_short(),
                            frame: identified
                                .main_window
                                .as_ref()
                                .and_then(|m| m.frame.clone()),
                        });
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

#[cfg(test)]
mod tests {
    use super::{
        click_transfer_card, find_transfer_dialog_frame, is_receivable_transfer_item_name,
        SUCCESS_SELECTOR, WINDOW_CLOSE_SELECTOR,
    };
    use crate::ia::types::{A11yNode, Action, Bounds};

    #[test]
    fn transfer_item_matcher_only_accepts_receivable_cards() {
        assert!(is_receivable_transfer_item_name(
            "￥0.10 Confirm receipt WeChat Transfer",
            Some("￥0.10")
        ));
        assert!(!is_receivable_transfer_item_name(
            "￥0.10 Accepted WeChat Transfer",
            Some("￥0.10")
        ));
        assert!(!is_receivable_transfer_item_name(
            "￥0.20 Confirm receipt WeChat Transfer",
            Some("￥0.10")
        ));
    }

    #[test]
    fn transfer_card_click_uses_message_bubble_side() {
        let bounds = Bounds {
            x: 273.0,
            y: 505.0,
            width: 1004.0,
            height: 111.0,
        };

        let incoming = click_transfer_card(&bounds, false);
        let outgoing = click_transfer_card(&bounds, true);

        match incoming {
            Action::ClickCoords { x, y } => {
                assert!(x > 430.0);
                assert!(x < 620.0);
                assert_eq!(y, 561.0);
            }
            _ => panic!("expected incoming click coords"),
        }

        match outgoing {
            Action::ClickCoords { x, y } => {
                assert!(x > 930.0);
                assert!(x < 1120.0);
                assert_eq!(y, 561.0);
            }
            _ => panic!("expected outgoing click coords"),
        }
    }

    fn node(role: &str, name: &str, children: Vec<A11yNode>) -> A11yNode {
        A11yNode {
            role: role.to_string(),
            name: name.to_string(),
            bounds: None,
            children: if children.is_empty() {
                None
            } else {
                Some(children)
            },
            parent_index: None,
            window: None,
            states: None,
        }
    }

    #[test]
    fn dialog_frame_search_skips_main_chat_frame() {
        let main_frame = node(
            "frame",
            "Weixin",
            vec![
                node("tool-bar", "", vec![node("push-button", "Disable", vec![])]),
                node("list", "Chats", vec![]),
                node(
                    "list",
                    "Messages",
                    vec![node("list-item", "￥1.00 Accepted WeChat Transfer", vec![])],
                ),
            ],
        );
        let dialog_frame = node(
            "frame",
            "Weixin",
            vec![
                node("tool-bar", "", vec![node("push-button", "Disable", vec![])]),
                node(
                    "label",
                    "You've accepted the transfer. The money has been deposited to your Balance.",
                    vec![],
                ),
            ],
        );
        let desktop = node(
            "desktop-frame",
            "main",
            vec![node(
                "application",
                "wechat",
                vec![main_frame, dialog_frame.clone()],
            )],
        );

        let success_frame = find_transfer_dialog_frame(&desktop, SUCCESS_SELECTOR);
        let close_frame = find_transfer_dialog_frame(&desktop, WINDOW_CLOSE_SELECTOR);

        assert!(success_frame.is_some());
        assert!(close_frame.is_some());
        assert!(success_frame.is_some_and(|frame| frame.name == dialog_frame.name));
        assert!(close_frame.is_some_and(|frame| frame.name == dialog_frame.name));
    }
}
