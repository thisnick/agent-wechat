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

/// Extract a FrameHint from an a11y frame node.
pub fn frame_hint_from_node(node: &A11yNode) -> Option<FrameHint> {
    let bounds = node.bounds.clone()?;
    Some(FrameHint {
        name: if node.name.is_empty() {
            None
        } else {
            Some(node.name.clone())
        },
        bounds,
        pid: node.window.as_ref().map(|w| w.pid),
    })
}

/// Find the innermost frame ancestor that contains a node matching `selector`.
/// Walks the tree top-down, preferring deeper frames so we get the tightest
/// enclosing frame (e.g. "Settings" frame, not the root desktop-frame).
pub fn find_frame_for(a11y: &A11yNode, selector: &str) -> Option<FrameHint> {
    fn walk<'a>(
        node: &'a A11yNode,
        selector: &str,
        current_frame: Option<&'a A11yNode>,
    ) -> Option<&'a A11yNode> {
        let frame = if node.role == "frame" {
            Some(node)
        } else {
            current_frame
        };

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
