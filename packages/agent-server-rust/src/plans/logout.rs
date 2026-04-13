use super::Plan;
use crate::ia::actions::{click_bounds, wait_short};
use crate::ia::types::*;

pub struct LogoutPlan;

pub struct LogoutParams;

#[async_trait::async_trait]
impl Plan for LogoutPlan {
    type PlanState = ();
    type Params = LogoutParams;

    fn id(&self) -> &str {
        "logout"
    }

    fn initial_plan_state(&self) -> () {
        ()
    }

    fn is_goal_reached(&self, state: &AppState, _plan_state: &()) -> bool {
        !state.main_window.is_logged_in
    }

    async fn select_action(
        &self,
        state: &AppState,
        _params: &LogoutParams,
        identified: &IdentifiedStates,
        _plan_state: &mut (),
        _a11y: &A11yNode,
        _session_id: &str,
    ) -> Option<SelectedAction> {
        let settings_frame = identified.settings.as_ref().and_then(|s| s.frame.clone());
        let main_frame = identified
            .main_window
            .as_ref()
            .and_then(|m| m.frame.clone());

        // 1. Settings confirmation modal → click OK
        if let Some(ref settings) = state.settings {
            if let Some(ref b) = settings.modal_ok_bounds {
                return Some(SelectedAction {
                    action: click_bounds(b),
                    frame: settings_frame.clone(),
                });
            }

            // 2. Settings has Log Out button → click it
            if let Some(ref b) = settings.logout_button_bounds {
                return Some(SelectedAction {
                    action: click_bounds(b),
                    frame: settings_frame.clone(),
                });
            }

            // 3. Settings open, no Log Out → click My Account
            if let Some(ref b) = settings.my_account_bounds {
                return Some(SelectedAction {
                    action: click_bounds(b),
                    frame: settings_frame.clone(),
                });
            }

            // 4. Settings open but nothing clickable → wait
            return Some(SelectedAction {
                action: wait_short(),
                frame: None,
            });
        }

        // 5. More dropdown open → click Settings menu item (inside main window area)
        if let Some(ref b) = state.main_window.settings_menu_item_bounds {
            return Some(SelectedAction {
                action: click_bounds(b),
                frame: main_frame.clone(),
            });
        }

        // 6. Chat view → click More button
        if let Some(ref b) = state.main_window.more_button_bounds {
            return Some(SelectedAction {
                action: click_bounds(b),
                frame: main_frame,
            });
        }

        // 7. Unknown state → no action (execution will fail with "No action selected")
        None
    }
}
