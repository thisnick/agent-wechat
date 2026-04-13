use super::exec::{exec_command, ExecOptions};
use crate::ia::types::A11yNode;

const A11Y_SCRIPT_PATH: &str = "/opt/tools/a11y-dump";

/// Add parent index references to all nodes in the tree.
/// This enables traversal up the tree via find_ancestor.
fn add_parent_refs(node: &mut A11yNode, _parent_index: Option<usize>) {
    // Parent refs are set during flattening if needed.
    // For the tree-based approach, we skip parent indices
    // and rely on the recursive tree structure.
    if let Some(children) = &mut node.children {
        for child in children.iter_mut() {
            add_parent_refs(child, None);
        }
    }
}

/// Get the desktop accessibility tree as a nested structure.
/// Uses the Python a11y-dump script.
pub async fn get_a11y_desktop(options: &ExecOptions) -> Result<A11yNode, String> {
    let result = exec_command("python3", &[A11Y_SCRIPT_PATH, "--format", "json"], options).await;

    if result.exit_code != 0 {
        return Err(result.stderr.clone().or_if_empty(&result.stdout));
    }

    let mut tree: A11yNode =
        serde_json::from_str(&result.stdout).map_err(|e| format!("Failed to parse a11y: {e}"))?;

    add_parent_refs(&mut tree, None);
    Ok(tree)
}

/// Get a11y tree as ARIA-style text.
pub async fn get_a11y_aria(options: &ExecOptions) -> Result<String, String> {
    let result = exec_command("python3", &[A11Y_SCRIPT_PATH, "--format", "aria"], options).await;

    if result.exit_code != 0 {
        return Err(result.stderr.clone().or_if_empty(&result.stdout));
    }

    Ok(result.stdout)
}

/// Helper trait
trait OrIfEmpty {
    fn or_if_empty(self, other: &str) -> String;
}

impl OrIfEmpty for String {
    fn or_if_empty(self, other: &str) -> String {
        if self.is_empty() {
            other.to_string()
        } else {
            self
        }
    }
}

/// Convert A11yNode tree to ARIA-style human-readable format.
pub fn tree_to_aria(node: &A11yNode, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let bounds = node
        .bounds
        .as_ref()
        .map(|b| format!("@({},{} {}x{})", b.x, b.y, b.width, b.height))
        .unwrap_or_default();
    let name = if !node.name.is_empty() {
        format!("\"{}\"", node.name)
    } else {
        String::new()
    };
    let states = node
        .states
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| format!("[{}]", s.join(",")))
        .unwrap_or_default();

    let parts: Vec<&str> = [
        node.role.as_str(),
        name.as_str(),
        states.as_str(),
        bounds.as_str(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    let line = format!("{indent}- {}", parts.join(" "));

    if let Some(children) = &node.children {
        if !children.is_empty() {
            let child_lines: Vec<String> = children
                .iter()
                .map(|c| tree_to_aria(c, depth + 1))
                .collect();
            return format!("{line}\n{}", child_lines.join("\n"));
        }
    }

    line
}
