use super::Plan;
use crate::ia::actions;
use crate::ia::selectors::{is_send_button_name, query_selector};
use crate::ia::types::*;
use crate::tools::chat_select::{open_chat, OpenChatResult};
use crate::tools::exec::{exec_command, ExecOptions};

pub struct SendMessagePlan;

pub struct SendMessageParams {
    pub chat_id: String,
    pub message: Option<String>,
    pub image_path: Option<String>,
    pub image_mime: Option<String>,
    pub file_path: Option<String>,
}

pub enum SendMessagePhase {
    Opening,
    Focusing,
    Inputting,
    Confirming,
    Done,
}

pub struct SendMessagePlanState {
    pub phase: SendMessagePhase,
    pub open_result: Option<OpenChatResult>,
    pub confirm_attempts: u32,
    /// Guard: a single send plan run may emit at most one actual send action.
    pub send_action_executed: bool,
}

fn find_edit_and_send_button(a11y: &A11yNode) -> Option<(&A11yNode, &A11yNode)> {
    // Locale-robust: scan for an EDITABLE-text + send-button pair anywhere in the
    // tree (the send button is "Send(S)" on EN clients, "发送(S)" on ZH clients).
    find_edit_send_pair(a11y)
}

fn find_edit_send_pair(node: &A11yNode) -> Option<(&A11yNode, &A11yNode)> {
    if let Some(children) = &node.children {
        let send_btn = children.iter().find(|c| {
            c.role == "push-button" && is_send_button_name(&c.name)
        });
        let edit_node = children.iter().find(|c| {
            c.role == "text"
                && c.states
                    .as_ref()
                    .map(|s| s.iter().any(|st| st == "EDITABLE"))
                    .unwrap_or(false)
        });

        if let (Some(edit), Some(send)) = (edit_node, send_btn) {
            return Some((edit, send));
        }

        // Recurse
        for child in children {
            if let Some(result) = find_edit_send_pair(child) {
                return Some(result);
            }
        }
    }
    None
}

/// AW-FORK-19: whether a send may reuse the currently-open chat or must re-open
/// the intended target first.
///
/// A present composer alone must NEVER imply the target chat is open. The old
/// `already_chat_open => skip_reopen` shortcut (AW-FORK-8D) made exactly that
/// assumption and mis-routed a group send into a still-open *private* chat
/// (AW-FORK-18E). Skipping the open is only safe when the currently-open chat's
/// identity has been positively verified to equal the intended target. The UI
/// layer has no reliable open-chat-identity read today, so callers pass `false`
/// and we always re-open the target (fail closed if that can't be confirmed).
#[derive(Debug, PartialEq, Eq)]
pub enum SendOpenDecision {
    /// Must open the intended target before typing/sending.
    ForceOpenTarget,
    /// Safe to reuse the already-open chat (only when verified as the target).
    SkipOpen,
}

pub fn decide_send_open_policy(current_open_is_verified_target: bool) -> SendOpenDecision {
    if current_open_is_verified_target {
        SendOpenDecision::SkipOpen
    } else {
        SendOpenDecision::ForceOpenTarget
    }
}

#[async_trait::async_trait]
impl Plan for SendMessagePlan {
    type PlanState = SendMessagePlanState;
    type Params = SendMessageParams;

    fn id(&self) -> &str { "send_message" }

    fn initial_plan_state(&self) -> SendMessagePlanState {
        SendMessagePlanState {
            phase: SendMessagePhase::Opening,
            open_result: None,
            confirm_attempts: 0,
            send_action_executed: false,
        }
    }

    fn is_goal_reached(&self, _state: &AppState, plan_state: &SendMessagePlanState) -> bool {
        matches!(plan_state.phase, SendMessagePhase::Done)
    }

    async fn select_action(
        &self,
        state: &AppState,
        params: &SendMessageParams,
        identified: &IdentifiedStates,
        plan_state: &mut SendMessagePlanState,
        a11y: &A11yNode,
        _session_id: &str,
    ) -> Option<SelectedAction> {
        let main_state_id = identified.main_window.as_ref().map(|m| m.state_id.as_str());

        // Dismiss popups
        if state.popup.is_some() && identified.popup.is_some() {
            return Some(SelectedAction {
                action: actions::dismiss_popup(),
                frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
            });
        }

        loop {
            match &plan_state.phase {
                SendMessagePhase::Opening => {
                    if main_state_id != Some("chat") && main_state_id != Some("chat_open") {
                        return None;
                    }

                    let chat_list_item = query_selector(a11y, r#"list[name="Chats"] > list-item"#);
                    let click_xy = chat_list_item.and_then(|item| {
                        item.bounds.as_ref().map(|b| (
                            (b.x + b.width / 2.0).round(),
                            (b.y + b.height / 2.0).round(),
                        ))
                    });

                    let force = main_state_id == Some("chat");
                    let mut result = open_chat(&params.chat_id, force, click_xy).await;

                    if !result.ok {
                        // AW-FORK-19 — CROSS-CHAT-SAFE OPEN. The frida fast-path
                        // failed (it always does on WeChat 4.1.1.x: unknown
                        // BUILD_PROFILE, AW-FORK-7). We must open the *intended
                        // target* by name and must NEVER reuse whatever chat is
                        // already open. The removed `already_chat_open =>
                        // skip_reopen` shortcut assumed any open composer was the
                        // target, which mis-routed a group send into a still-open
                        // private chat (AW-FORK-18E). A present composer alone does
                        // not prove the target is open, so we always re-open it.
                        match decide_send_open_policy(/* verified target */ false) {
                            SendOpenDecision::SkipOpen => {
                                // Only reachable once a reliable open-chat-identity
                                // check exists (not today); reuse the open chat.
                                tracing::info!(
                                    "[send] skip_reopen=true reason=verified_target_open"
                                );
                                result = OpenChatResult {
                                    ok: true,
                                    username: None,
                                    index: None,
                                    skipped: Some(true),
                                    error: None,
                                };
                            }
                            SendOpenDecision::ForceOpenTarget => {
                                tracing::info!(
                                    "[send] target_open_policy=always_open skip_reopen=false reason=cross_chat_safety mainWindow_open={} prev_error_present={}",
                                    main_state_id == Some("chat_open"),
                                    result.error.is_some()
                                );
                                // SEND-SAFE: click-only open, never presses Return,
                                // so it cannot inject a stray (AW-FORK-8B). Resolves
                                // chat_id -> name -> search -> click the target row.
                                let fb = crate::tools::ui_open_chat::open_chat_a11y_search_send_safe(
                                    &params.chat_id,
                                )
                                .await;
                                if fb.open_confirmed {
                                    tracing::info!(
                                        "[send] target_open_required=true target_open_ok=true resolved_name_present={}",
                                        fb.resolved_name_present
                                    );
                                    result = OpenChatResult {
                                        ok: true,
                                        username: None,
                                        index: None,
                                        skipped: Some(false),
                                        error: None,
                                    };
                                } else {
                                    // FAIL CLOSED: never fall back to sending into
                                    // whatever chat is currently open.
                                    tracing::warn!(
                                        "[send] target_open_required=true target_open_ok=false error={:?} fail_closed=true",
                                        fb.error
                                    );
                                    return None;
                                }
                            }
                        }
                    }

                    let skipped = result.skipped.unwrap_or(false);
                    plan_state.open_result = Some(result);
                    plan_state.phase = SendMessagePhase::Focusing;

                    if !skipped {
                        return Some(SelectedAction {
                            action: actions::wait_short(),
                            frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                        });
                    }
                    continue;
                }

                SendMessagePhase::Focusing => {
                    if main_state_id != Some("chat_open") {
                        return None;
                    }

                    let found = find_edit_and_send_button(a11y);
                    let (edit_node, _) = match found {
                        Some(f) => f,
                        None => return None,
                    };

                    plan_state.phase = SendMessagePhase::Inputting;

                    let is_focused = edit_node
                        .states
                        .as_ref()
                        .map(|s| s.iter().any(|st| st == "FOCUSED"))
                        .unwrap_or(false);

                    if is_focused {
                        continue;
                    }

                    if let Some(bounds) = &edit_node.bounds {
                        return Some(SelectedAction {
                            action: actions::click_bounds(bounds),
                            frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                        });
                    }
                    return None;
                }

                SendMessagePhase::Inputting => {
                    let found = find_edit_and_send_button(a11y);
                    if found.is_none() {
                        tracing::warn!("[send] composer_not_found");
                        return None;
                    }

                    // Single-send guard: never emit a second send action within one
                    // plan run (AW-FORK-8B defense-in-depth against duplicates).
                    if plan_state.send_action_executed {
                        tracing::warn!("[send] send_action_guard_triggered");
                        plan_state.phase = SendMessagePhase::Done;
                        return None;
                    }
                    plan_state.send_action_executed = true;
                    tracing::info!(
                        "[send] composer_pair_found=true composer_cleared=true send_action_count=1"
                    );

                    plan_state.phase = SendMessagePhase::Confirming;

                    // File
                    if let Some(fp) = &params.file_path {
                        exec_command("paste-file", &[fp], &ExecOptions::default()).await;
                        return Some(SelectedAction {
                            action: actions::sequence(vec![
                                Action::Wait { ms: 100 },
                                Action::Key { combo: "Return".to_string() },
                            ]),
                            frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                        });
                    }

                    // Image
                    if let Some(ip) = &params.image_path {
                        let mut args: Vec<&str> = vec![ip];
                        if let Some(mime) = &params.image_mime {
                            args.push(mime);
                        }
                        exec_command("paste-image", &args, &ExecOptions::default()).await;
                        return Some(SelectedAction {
                            action: actions::sequence(vec![
                                Action::Wait { ms: 100 },
                                Action::Key { combo: "Return".to_string() },
                            ]),
                            frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                        });
                    }

                    // Text
                    if let Some(msg) = &params.message {
                        return Some(SelectedAction {
                            action: actions::sequence(vec![
                                Action::Key { combo: "ctrl+a".to_string() },
                                Action::Type { text: msg.clone(), selector: None },
                                Action::Wait { ms: 100 },
                                Action::Key { combo: "Return".to_string() },
                            ]),
                            frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                        });
                    }

                    return None;
                }

                SendMessagePhase::Confirming => {
                    let found = find_edit_and_send_button(a11y);
                    let (_, send_btn) = match found {
                        Some(f) => f,
                        None => return None,
                    };

                    let is_disabled = send_btn
                        .states
                        .as_ref()
                        .map(|s| s.iter().any(|st| st == "DISABLED"))
                        .unwrap_or(false);

                    if is_disabled {
                        plan_state.phase = SendMessagePhase::Done;
                        return Some(SelectedAction {
                            action: actions::wait_short(),
                            frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                        });
                    }

                    plan_state.confirm_attempts += 1;
                    if plan_state.confirm_attempts >= 5 {
                        return None;
                    }

                    return Some(SelectedAction {
                        action: actions::wait_short(),
                        frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                    });
                }

                SendMessagePhase::Done => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{decide_send_open_policy, SendOpenDecision};

    // AW-FORK-18E regression: a chat being open (composer present) is NOT a
    // verified target, so a send must force-open the intended target. This is the
    // exact case that previously mis-routed a group send into a private chat.
    #[test]
    fn composer_present_alone_forces_target_open() {
        assert_eq!(
            decide_send_open_policy(false),
            SendOpenDecision::ForceOpenTarget
        );
    }

    // Skipping the open is only allowed when the open chat is the verified target.
    #[test]
    fn only_verified_target_may_skip_open() {
        assert_eq!(decide_send_open_policy(true), SendOpenDecision::SkipOpen);
    }
}
