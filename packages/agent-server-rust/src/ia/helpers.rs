use super::selectors::query_selector;
use super::types::{A11yNode, Bounds, FrameHint};

/// Generate a stable hash from a string.
fn hash_string(s: &str) -> String {
    let mut hash: i32 = 0;
    for ch in s.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(ch as i32);
    }
    format!("{}", hash.unsigned_abs())
}

/// Extract active chat ID from message view header.
pub fn extract_active_chat_id(a11y: &A11yNode) -> Option<String> {
    let header = query_selector(a11y, "label[name=/.+/]")?;
    if !header.name.is_empty() {
        Some(format!("chat_{}", hash_string(&header.name)))
    } else {
        None
    }
}

/// Check if bounds are valid (non-zero size).
pub fn has_valid_bounds(bounds: &Option<Bounds>) -> bool {
    bounds
        .as_ref()
        .map(|b| b.width > 0.0 && b.height > 0.0)
        .unwrap_or(false)
}

/// Calculate center point of bounds.
pub fn get_bounds_center(bounds: &Bounds) -> (f64, f64) {
    (
        (bounds.x + bounds.width / 2.0).round(),
        (bounds.y + bounds.height / 2.0).round(),
    )
}

/// Check whether an a11y node carries the given state (e.g. "FOCUSED",
/// "DISABLED", "EDITABLE").
pub fn node_has_state(node: &A11yNode, state: &str) -> bool {
    node.states
        .as_ref()
        .map(|s| s.iter().any(|st| st == state))
        .unwrap_or(false)
}

/// The main application frame is named "Weixin" or "WeChat" depending on
/// build/locale (the in-repo fixtures use "WeChat"; chat.rs matches both
/// names for the nav button for the same reason).
fn is_main_frame(node: &A11yNode) -> bool {
    node.role == "frame" && (node.name == "Weixin" || node.name == "WeChat")
}

/// A candidate composer: the editable text input plus its sibling Send(S) button.
struct ComposerPair<'a> {
    edit: &'a A11yNode,
    send: &'a A11yNode,
    /// True if this pair lives under the main application frame
    /// (as opposed to a detached/ghost chat frame leftover in the a11y tree).
    in_main_frame: bool,
}

/// Find the composer (editable + Send button) to operate on.
///
/// WeChat's accessibility tree can contain *multiple* edit+send pairs:
/// the live main-window composer plus stale "ghost" frames left behind by
/// chats that were previously detached into separate windows. A naive
/// depth-first "take the first pair" grabs the wrong (ghost) composer, whose
/// input never receives text and whose Send stays DISABLED forever, causing
/// plans to loop and ultimately fail with "No action selected".
///
/// To be robust we collect every candidate pair, then rank them so the
/// genuinely active composer wins:
///   1. editable currently FOCUSED          (strongest signal)
///   2. Send button NOT disabled            (composer already has text)
///   3. pair under the main application frame (not a ghost/detached frame)
/// The first pair (DFS order) breaks any remaining ties.
pub fn find_edit_and_send_button(a11y: &A11yNode) -> Option<(&A11yNode, &A11yNode)> {
    let mut candidates: Vec<ComposerPair> = Vec::new();
    collect_edit_send_pairs(a11y, false, &mut candidates);

    // Rank lexicographically; `false` sorts before `true`, so each criterion
    // is written as "false = preferred". DFS index breaks ties.
    candidates
        .iter()
        .enumerate()
        .min_by_key(|(idx, c)| {
            (
                !node_has_state(c.edit, "FOCUSED"),
                node_has_state(c.send, "DISABLED"),
                !c.in_main_frame,
                *idx,
            )
        })
        .map(|(_, c)| (c.edit, c.send))
}

/// Recursively collect all edit+send composer pairs, tracking whether each
/// pair is inside the main application frame.
fn collect_edit_send_pairs<'a>(
    node: &'a A11yNode,
    in_main_frame: bool,
    out: &mut Vec<ComposerPair<'a>>,
) {
    // Once we enter the main frame, everything below it is in-main.
    let in_main_frame = in_main_frame || is_main_frame(node);

    if let Some(children) = &node.children {
        let send_btn = children
            .iter()
            .find(|c| c.role == "push-button" && c.name == "Send(S)");
        let edit_node = children
            .iter()
            .find(|c| c.role == "text" && node_has_state(c, "EDITABLE"));

        if let (Some(edit), Some(send)) = (edit_node, send_btn) {
            out.push(ComposerPair {
                edit,
                send,
                in_main_frame,
            });
        }

        for child in children {
            collect_edit_send_pairs(child, in_main_frame, out);
        }
    }
}

/// Extract a FrameHint from an a11y frame node.
pub fn frame_hint_from_node(node: &A11yNode) -> Option<FrameHint> {
    let bounds = node.bounds.clone()?;
    Some(FrameHint {
        name: if node.name.is_empty() { None } else { Some(node.name.clone()) },
        bounds,
        pid: node.window.as_ref().map(|w| w.pid),
    })
}

/// Find the innermost frame ancestor that contains a node matching `selector`.
/// Walks the tree top-down, preferring deeper frames so we get the tightest
/// enclosing frame (e.g. "Settings" frame, not the root desktop-frame).
pub fn find_frame_for(a11y: &A11yNode, selector: &str) -> Option<FrameHint> {
    fn walk<'a>(node: &'a A11yNode, selector: &str, current_frame: Option<&'a A11yNode>) -> Option<&'a A11yNode> {
        let frame = if node.role == "frame" { Some(node) } else { current_frame };

        // If this subtree contains the target, the deepest frame wins
        if query_selector(node, selector).is_some() {
            // Check children for a tighter frame
            if let Some(children) = &node.children {
                for child in children {
                    if let Some(deeper) = walk(child, selector, frame) {
                        return Some(deeper);
                    }
                }
            }
            // No deeper frame found — return current
            return frame;
        }
        None
    }
    walk(a11y, selector, None).and_then(frame_hint_from_node)
}

