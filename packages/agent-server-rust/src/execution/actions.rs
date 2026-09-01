use crate::ia::selectors::query_selector;
use crate::ia::types::{A11yNode, Action, FrameHint, SubscriptionEvent};
use crate::tools::exec::{exec_command, ExecOptions};
use std::future::Future;
use std::pin::Pin;

/// Build --window activation args from a FrameHint.
fn window_activate_args(hint: &FrameHint) -> Vec<String> {
    if let Some(pid) = hint.pid {
        vec![
            "--window".to_string(),
            pid.to_string(),
            (hint.bounds.x as i32).to_string(),
            (hint.bounds.y as i32).to_string(),
            (hint.bounds.width as i32).to_string(),
            (hint.bounds.height as i32).to_string(),
            "--".to_string(),
        ]
    } else {
        vec![]
    }
}

/// Execute a single action against the WeChat UI.
/// `frame` is the target window hint from the plan — used for window activation.
/// Returns a BoxFuture to support recursive calls (Sequence action).
pub fn execute_action<'a>(
    action: &'a Action,
    frame: Option<&'a FrameHint>,
    options: &'a ExecOptions,
    a11y: &'a A11yNode,
    emit: &'a (dyn Fn(SubscriptionEvent) + Send + Sync),
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let activate_args = frame.map(|f| window_activate_args(f)).unwrap_or_default();

        match action {
            Action::ClickSelector { selector } => {
                let node_match = query_selector(a11y, selector);

                if let Some(node) = node_match {
                    if let Some(bounds) = &node.bounds {
                        let cx = (bounds.x + bounds.width / 2.0).round() as i32;
                        let cy = (bounds.y + bounds.height / 2.0).round() as i32;
                        tracing::info!("[action] click selector '{selector}' → ({cx}, {cy})");

                        let mut args = activate_args.clone();
                        args.push(cx.to_string());
                        args.push(cy.to_string());
                        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        exec_command("click", &args_ref, options).await;
                    } else {
                        tracing::warn!(
                            "[action] click selector '{selector}' matched but no bounds"
                        );
                    }
                } else {
                    tracing::warn!("[action] click selector '{selector}' — no match");
                }
            }

            Action::ClickCoords { x, y } => {
                let mut args = activate_args.clone();
                args.push((*x as i32).to_string());
                args.push((*y as i32).to_string());
                let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                exec_command("click", &args_ref, options).await;
            }

            Action::Type { text, selector: _ } => {
                exec_command("input", &[text.as_str()], options).await;
            }

            Action::Key { combo } => {
                let mut args = activate_args.clone();
                args.push(combo.clone());
                let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                exec_command("key", &args_ref, options).await;
            }

            Action::Scroll {
                direction,
                x: _,
                y: _,
                amount,
            } => {
                let dir = match direction {
                    crate::ia::types::ScrollDirection::Up => "up",
                    crate::ia::types::ScrollDirection::Down => "down",
                };
                let mut args = vec![dir.to_string()];
                if let Some(amt) = amount {
                    args.push(amt.to_string());
                }
                let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                exec_command("scroll", &args_ref, options).await;
            }

            Action::Wait { ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(*ms)).await;
            }

            Action::Emit { event } => {
                emit(event.clone());
            }

            Action::Sequence { actions } => {
                for a in actions {
                    execute_action(a, frame, options, a11y, emit).await;
                }
            }
        }
    })
}
