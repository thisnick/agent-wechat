use super::Plan;
use crate::ia::types::*;

pub struct AuthStatusPlan;

pub struct AuthStatusParams;

#[async_trait::async_trait]
impl Plan for AuthStatusPlan {
    type PlanState = ();
    type Params = AuthStatusParams;

    fn id(&self) -> &str {
        "auth_status"
    }

    fn initial_plan_state(&self) -> () {
        ()
    }

    /// Goal reached immediately — we just want one observation.
    fn is_goal_reached(&self, _state: &AppState, _plan_state: &()) -> bool {
        true
    }

    /// No actions needed — just observe.
    async fn select_action(
        &self,
        _state: &AppState,
        _params: &AuthStatusParams,
        _identified: &IdentifiedStates,
        _plan_state: &mut (),
        _a11y: &A11yNode,
        _session_id: &str,
    ) -> Option<SelectedAction> {
        None
    }
}
