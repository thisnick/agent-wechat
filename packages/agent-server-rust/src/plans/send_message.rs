use super::Plan;
use crate::ia::actions;
use crate::ia::selectors::query_selector;
use crate::ia::types::*;
use crate::tools::chat_select::{open_chat, OpenChatResult};
use crate::tools::exec::{exec_command, ExecOptions};

pub struct SendMessagePlan;

#[cfg(test)]
mod composer_tests {
    use super::*;
    use serde_json::{json, Value};

    fn editor(y: f64) -> Value {
        json!({"role":"text","name":"","states":["EDITABLE"],
            "bounds":{"x":300.0,"y":y,"width":650.0,"height":80.0}})
    }
    fn send(name: &str) -> Value {
        json!({"role":"push-button","name":name,"states":["DISABLED"],
            "bounds":{"x":890.0,"y":700.0,"width":55.0,"height":24.0}})
    }
    fn group(children: Vec<Value>) -> Value {
        json!({"role":"filler","name":"","children":children})
    }
    fn parse(value: Value) -> A11yNode { serde_json::from_value(value).unwrap() }

    #[test]
    fn legacy_sibling_composer() {
        let tree = parse(group(vec![editor(610.0), send("Send(S)")]));
        assert!(find_edit_and_send_button(&tree).is_some());
    }

    #[test]
    fn nested_new_composer_with_search_field() {
        let composer = group(vec![group(vec![editor(610.0)]),
            group(vec![group(vec![send("Send Voice"), send("Send")])])]);
        let tree = parse(group(vec![editor(45.0), composer]));
        let (edit, button) = find_edit_and_send_button(&tree).unwrap();
        assert_eq!(edit.bounds.as_ref().unwrap().y, 610.0);
        assert_eq!(button.name, "Send");
    }

    #[test]
    fn does_not_pair_search_field_with_send() {
        let tree = parse(group(vec![group(vec![editor(45.0)]), group(vec![send("Send")])]));
        assert!(find_edit_and_send_button(&tree).is_none());
    }

    #[test]
    fn rejects_ambiguous_editors_and_voice_button() {
        let tree = parse(group(vec![group(vec![editor(610.0), editor(610.0)]), group(vec![send("Send")])]));
        assert!(find_edit_and_send_button(&tree).is_none());
        let voice = parse(group(vec![editor(610.0), send("Send Voice")]));
        assert!(find_edit_and_send_button(&voice).is_none());
    }
}

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
}

fn find_edit_and_send_button(a11y: &A11yNode) -> Option<(&A11yNode, &A11yNode)> {
    find_edit_send_pair(a11y)
}

fn find_edit_send_pair(node: &A11yNode) -> Option<(&A11yNode, &A11yNode)> {
    if let Some(children) = &node.children {
        let send_btn = children.iter().find(|c| {
            c.role == "push-button" && matches!(c.name.as_str(), "Send(S)" | "Send")
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

        // 4.1.13 nests the editor and Send button in separate composer
        // branches. Search the smallest common subtree, requiring one editor
        // and a nearby button below it so the chat-search field cannot match.
        fn collect<'a>(node: &'a A11yNode, edits: &mut Vec<&'a A11yNode>, sends: &mut Vec<&'a A11yNode>) {
            if node.role == "text" && node.states.as_ref().is_some_and(|s| s.iter().any(|s| s == "EDITABLE")) {
                edits.push(node);
            }
            if node.role == "push-button" && matches!(node.name.as_str(), "Send(S)" | "Send") {
                sends.push(node);
            }
            for child in node.children.as_deref().unwrap_or_default() {
                collect(child, edits, sends);
            }
        }
        let (mut edits, mut sends) = (Vec::new(), Vec::new());
        collect(node, &mut edits, &mut sends);
        if let ([edit], [send]) = (edits.as_slice(), sends.as_slice()) {
            if let (Some(e), Some(s)) = (&edit.bounds, &send.bounds) {
                let gap = s.y - (e.y + e.height);
                if e.width > 0.0 && e.height > 0.0 && s.width > 0.0 && s.height > 0.0
                    && (-8.0..=120.0).contains(&gap)
                    && s.x < e.x + e.width && s.x + s.width > e.x
                {
                    return Some((edit, send));
                }
            }
        }
    }
    None
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
                    let result = open_chat(&params.chat_id, force, click_xy).await;

                    if !result.ok {
                        return None;
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
                        return None;
                    }

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
