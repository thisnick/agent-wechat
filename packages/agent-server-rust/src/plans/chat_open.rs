use super::Plan;
use super::composer::find_edit_and_send_button;
use crate::ia::actions;
use crate::ia::selectors::{query_selector, query_selector_all};
use crate::ia::types::*;
use crate::tools::chat_select::{open_chat, OpenChatResult};

pub struct ChatOpenPlan;

pub struct ChatOpenParams {
    pub chat_id: String,
    pub clear_unreads: bool,
}

pub struct ChatOpenPlanState {
    pub phase: ChatOpenPhase,
    pub result: Option<OpenChatResult>,
}

pub enum ChatOpenPhase {
    Opening,
    Focusing,
    ClickingAudio,
    Done,
}

fn find_edit_area(a11y: &A11yNode) -> Option<&A11yNode> {
    find_edit_and_send_button(a11y).map(|(edit, _)| edit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn editor(y: f64) -> Value {
        json!({"role":"text","name":"","states":["EDITABLE"],
            "bounds":{"x":321.0,"y":y,"width":942.0,"height":79.0}})
    }

    fn send(name: &str) -> Value {
        json!({"role":"push-button","name":name,"states":["DISABLED"],
            "bounds":{"x":1204.0,"y":735.0,"width":55.0,"height":24.0}})
    }

    fn group(children: Vec<Value>) -> Value {
        json!({"role":"filler","name":"","children":children})
    }

    fn identified() -> IdentifiedStates {
        IdentifiedStates {
            main_window: Some(IdentifiedState {
                state_id: "chat_open".into(), fsm: "main_window".into(), frame: None,
            }),
            popup: None, contact_card: None, settings: None,
        }
    }

    #[tokio::test]
    async fn focusing_supports_both_send_labels_and_layouts() {
        for name in ["Send", "Send(S)"] {
            for nested in [false, true] {
                let composer = if nested {
                    group(vec![group(vec![editor(646.0)]),
                        group(vec![group(vec![send("Send Voice"), send(name)])])])
                } else {
                    group(vec![editor(646.0), send(name)])
                };
                let tree: A11yNode = serde_json::from_value(group(vec![editor(46.0), composer])).unwrap();
                let mut phase = ChatOpenPlanState { phase: ChatOpenPhase::Focusing, result: None };
                let action = ChatOpenPlan.select_action(
                    &AppState::default(),
                    &ChatOpenParams { chat_id: "filehelper".into(), clear_unreads: true },
                    &identified(), &mut phase, &tree, "default",
                ).await.unwrap();
                assert!(matches!(action.action, Action::ClickCoords { x: 792.0, y: 686.0 }),
                    "wrong focus target for {name}, nested={nested}");
                assert!(matches!(phase.phase, ChatOpenPhase::ClickingAudio));
            }
        }
    }

    #[tokio::test]
    async fn focusing_does_not_click_search_or_send_voice() {
        for value in [
            group(vec![group(vec![editor(46.0)]), group(vec![send("Send")])]),
            group(vec![editor(646.0), send("Send Voice")]),
        ] {
            let tree: A11yNode = serde_json::from_value(value).unwrap();
            let mut phase = ChatOpenPlanState { phase: ChatOpenPhase::Focusing, result: None };
            let action = ChatOpenPlan.select_action(
                &AppState::default(),
                &ChatOpenParams { chat_id: "filehelper".into(), clear_unreads: true },
                &identified(), &mut phase, &tree, "default",
            ).await;
            assert!(action.is_none());
            assert!(matches!(phase.phase, ChatOpenPhase::Focusing));
        }
    }
}

#[async_trait::async_trait]
impl Plan for ChatOpenPlan {
    type PlanState = ChatOpenPlanState;
    type Params = ChatOpenParams;

    fn id(&self) -> &str { "chat_open" }

    fn initial_plan_state(&self) -> ChatOpenPlanState {
        ChatOpenPlanState {
            phase: ChatOpenPhase::Opening,
            result: None,
        }
    }

    fn is_goal_reached(&self, _state: &AppState, plan_state: &ChatOpenPlanState) -> bool {
        matches!(plan_state.phase, ChatOpenPhase::Done)
    }

    async fn select_action(
        &self,
        state: &AppState,
        params: &ChatOpenParams,
        identified: &IdentifiedStates,
        plan_state: &mut ChatOpenPlanState,
        a11y: &A11yNode,
        _session_id: &str,
    ) -> Option<SelectedAction> {
        // Dismiss popups
        if state.popup.is_some() && identified.popup.is_some() {
            return Some(SelectedAction {
                action: actions::dismiss_popup(),
                frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
            });
        }

        let main_state_id = identified.main_window.as_ref().map(|m| m.state_id.as_str());

        loop {
            match &plan_state.phase {
                ChatOpenPhase::Opening => {
                    if main_state_id != Some("chat") && main_state_id != Some("chat_open") {
                        return None;
                    }

                    // Find click target
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
                        plan_state.result = Some(result);
                        return None;
                    }

                    let skipped = result.skipped.unwrap_or(false);
                    plan_state.result = Some(result);

                    if params.clear_unreads {
                        plan_state.phase = ChatOpenPhase::Focusing;
                        tracing::info!("[chat_open] Opening → Focusing, skipped={}", skipped);
                        if !skipped {
                            return Some(SelectedAction {
                                action: actions::wait_short(),
                                frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                            });
                        }
                        continue;
                    }

                    // No clear_unreads — done
                    plan_state.phase = ChatOpenPhase::Done;
                    tracing::info!("[chat_open] Opening → Done (no clear_unreads)");
                    return Some(SelectedAction {
                        action: actions::wait_short(),
                        frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                    });
                }

                ChatOpenPhase::Focusing => {
                    if main_state_id != Some("chat_open") {
                        tracing::info!("[chat_open] Focusing: wrong state {:?}", main_state_id);
                        return None;
                    }

                    let edit_node = match find_edit_area(a11y) {
                        Some(n) => n,
                        None => {
                            tracing::info!("[chat_open] Focusing: edit area not found");
                            return None;
                        }
                    };

                    plan_state.phase = ChatOpenPhase::ClickingAudio;
                    tracing::info!("[chat_open] Focusing → ClickingAudio, edit_bounds={:?}", edit_node.bounds);

                    if let Some(bounds) = &edit_node.bounds {
                        return Some(SelectedAction {
                            action: actions::click_bounds(bounds),
                            frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                        });
                    }
                    continue;
                }

                ChatOpenPhase::ClickingAudio => {
                    if main_state_id != Some("chat_open") {
                        tracing::info!("[chat_open] ClickingAudio: wrong state {:?}", main_state_id);
                        return None;
                    }

                    let unplayed = query_selector_all(
                        a11y,
                        r#"list[name="Messages"] > list-item[name=/^Audio.*Unplay/s]"#,
                    );

                    // Build a sequence: click each unplayed audio with a wait between
                    let mut seq: Vec<Action> = Vec::new();
                    for node in &unplayed {
                        if let Some(bounds) = &node.bounds {
                            let x = (bounds.x + 100.0).round();
                            let y = (bounds.y + bounds.height / 2.0).round();
                            seq.push(actions::click_at(x, y));
                            seq.push(Action::Wait { ms: 500 });
                        }
                    }
                    tracing::info!("[chat_open] ClickingAudio: found {} unplayed, sequence of {} actions", unplayed.len(), seq.len());

                    plan_state.phase = ChatOpenPhase::Done;

                    if seq.is_empty() {
                        return Some(SelectedAction {
                            action: actions::wait_short(),
                            frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                        });
                    }

                    return Some(SelectedAction {
                        action: Action::Sequence { actions: seq },
                        frame: identified.main_window.as_ref().and_then(|m| m.frame.clone()),
                    });
                }

                ChatOpenPhase::Done => return None,
            }
        }
    }
}
