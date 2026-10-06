use crate::ia::types::A11yNode;

/// Locate the message editor and its Send button, excluding the chat search field.
pub(super) fn find_edit_and_send_button(node: &A11yNode) -> Option<(&A11yNode, &A11yNode)> {
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

        for child in children {
            if let Some(result) = find_edit_and_send_button(child) {
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
