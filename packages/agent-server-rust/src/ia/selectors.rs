use super::types::A11yNode;
use regex::Regex;

// ============================================
// Ancestor Traversal
// ============================================

/// Find an ancestor node matching a role name.
/// Uses parent_index to walk up the flattened tree.
pub fn find_ancestor_by_role<'a>(
    node: &'a A11yNode,
    role: &str,
    all_nodes: &'a [A11yNode],
) -> Option<&'a A11yNode> {
    let mut current_idx = node.parent_index;
    while let Some(idx) = current_idx {
        if idx < all_nodes.len() {
            let parent = &all_nodes[idx];
            if parent.role == role {
                return Some(parent);
            }
            current_idx = parent.parent_index;
        } else {
            break;
        }
    }
    None
}

// ============================================
// Selector AST Types
// ============================================

#[derive(Debug)]
struct SelectorNode {
    role: String,
    attrs: Vec<AttrMatcher>,
    pseudo: Option<PseudoSelector>,
}

#[derive(Debug)]
struct AttrMatcher {
    name: String,
    op: AttrOp,
    value: AttrValue,
}

#[derive(Debug)]
enum AttrOp {
    Exact,    // =
    Contains, // *=
    Starts,   // ^=
    Ends,     // $=
}

#[derive(Debug)]
enum AttrValue {
    Str(String),
    Regex(Regex),
}

#[derive(Debug)]
struct PseudoSelector {
    index: usize, // 1-indexed like CSS
}

#[derive(Debug)]
enum Combinator {
    Descendant,
    Child,
}

#[derive(Debug)]
struct SelectorAST {
    nodes: Vec<SelectorNode>,
    combinators: Vec<Combinator>,
}

// ============================================
// Tokenizer
// ============================================

fn tokenize(selector: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut in_regex = false;
    let mut quote_char = ' ';
    let chars: Vec<char> = selector.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        if in_quotes {
            current.push(ch);
            if ch == quote_char && (i == 0 || chars[i - 1] != '\\') {
                in_quotes = false;
            }
        } else if in_regex {
            current.push(ch);
            if ch == '/' && (i == 0 || chars[i - 1] != '\\') {
                // Check for flags after closing /
                while i + 1 < chars.len() && "gimsuy".contains(chars[i + 1]) {
                    i += 1;
                    current.push(chars[i]);
                }
                in_regex = false;
            }
        } else if ch == '"' || ch == '\'' {
            in_quotes = true;
            quote_char = ch;
            current.push(ch);
        } else if ch == '/' && !current.is_empty() && current.ends_with('=') {
            in_regex = true;
            current.push(ch);
        } else if ch == ' ' || ch == '>' {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                tokens.push(trimmed);
            }
            if ch == '>' {
                tokens.push(">".to_string());
            }
            current.clear();
        } else {
            current.push(ch);
        }

        i += 1;
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        tokens.push(trimmed);
    }

    tokens
}

// ============================================
// Parser
// ============================================

fn parse_node(token: &str) -> SelectorNode {
    // Parse role
    let role_re = Regex::new(r"^([a-z][-a-z]*|\*)").unwrap();
    let role = role_re
        .find(token)
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "*".to_string());

    let mut attrs = Vec::new();

    // Match attributes: [name="value"], [name=/regex/flags], [name*="value"]
    let attr_re =
        Regex::new(r#"\[([a-z]+)(=|\*=|\^=|\$=)("([^"]+)"|'([^']+)'|/(.+?)/([gimsuy]*))\]"#)
            .unwrap();

    for cap in attr_re.captures_iter(token) {
        let name = cap[1].to_string();
        let op = match &cap[2] {
            "=" => AttrOp::Exact,
            "*=" => AttrOp::Contains,
            "^=" => AttrOp::Starts,
            "$=" => AttrOp::Ends,
            _ => AttrOp::Exact,
        };

        let value = if let Some(regex_body) = cap.get(6) {
            let flags = cap.get(7).map(|m| m.as_str()).unwrap_or("");
            let mut prefix = String::new();
            if flags.contains('i') {
                prefix.push_str("(?i)");
            }
            if flags.contains('s') {
                prefix.push_str("(?s)");
            }
            let pattern = format!("{}{}", prefix, regex_body.as_str());
            AttrValue::Regex(Regex::new(&pattern).unwrap_or_else(|_| Regex::new("$^").unwrap()))
        } else {
            let s = cap
                .get(4)
                .or_else(|| cap.get(5))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            AttrValue::Str(s)
        };

        attrs.push(AttrMatcher { name, op, value });
    }

    // Parse :nth-child(n)
    let pseudo_re = Regex::new(r":nth-child\((\d+)\)").unwrap();
    let pseudo = pseudo_re.captures(token).map(|cap| PseudoSelector {
        index: cap[1].parse().unwrap_or(1),
    });

    SelectorNode {
        role,
        attrs,
        pseudo,
    }
}

fn build_ast(tokens: &[String]) -> SelectorAST {
    let mut nodes = Vec::new();
    let mut combinators = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        let token = &tokens[i];

        if token == ">" {
            combinators.push(Combinator::Child);
            i += 1;
            continue;
        }

        if nodes.len() > combinators.len() {
            combinators.push(Combinator::Descendant);
        }

        nodes.push(parse_node(token));
        i += 1;
    }

    SelectorAST { nodes, combinators }
}

fn parse_selector(selector: &str) -> SelectorAST {
    let tokens = tokenize(selector);
    build_ast(&tokens)
}

// ============================================
// Matcher
// ============================================

fn get_node_attr(node: &A11yNode, name: &str) -> String {
    match name {
        "name" => node.name.clone(),
        "role" => node.role.clone(),
        _ => String::new(),
    }
}

fn matches_node(node: &A11yNode, target: &SelectorNode, sibling_index: Option<usize>) -> bool {
    // Check role
    if target.role != "*" && node.role != target.role {
        return false;
    }

    // Check attributes
    for attr in &target.attrs {
        let node_value = get_node_attr(node, &attr.name);

        match &attr.value {
            AttrValue::Regex(re) => {
                if !re.is_match(&node_value) {
                    return false;
                }
            }
            AttrValue::Str(s) => match attr.op {
                AttrOp::Exact => {
                    if node_value != *s {
                        return false;
                    }
                }
                AttrOp::Contains => {
                    if !node_value.contains(s.as_str()) {
                        return false;
                    }
                }
                AttrOp::Starts => {
                    if !node_value.starts_with(s.as_str()) {
                        return false;
                    }
                }
                AttrOp::Ends => {
                    if !node_value.ends_with(s.as_str()) {
                        return false;
                    }
                }
            },
        }
    }

    // Check :nth-child (1-indexed)
    if let Some(pseudo) = &target.pseudo {
        match sibling_index {
            Some(idx) if idx + 1 == pseudo.index => {}
            _ => return false,
        }
    }

    true
}

/// Find the first descendant matching the target selector node.
fn walk_tree_match<'a>(node: &'a A11yNode, target: &SelectorNode) -> Option<&'a A11yNode> {
    if matches_node(node, target, None) {
        return Some(node);
    }
    if let Some(children) = &node.children {
        for (i, child) in children.iter().enumerate() {
            // For nth-child, pass sibling index
            if matches_node(child, target, Some(i)) {
                return Some(child);
            }
            if let Some(found) = walk_tree_match(child, target) {
                return Some(found);
            }
        }
    }
    None
}

/// Find the first direct child matching the target selector node.
fn walk_children_match<'a>(node: &'a A11yNode, target: &SelectorNode) -> Option<&'a A11yNode> {
    if let Some(children) = &node.children {
        for (i, child) in children.iter().enumerate() {
            if matches_node(child, target, Some(i)) {
                return Some(child);
            }
        }
    }
    None
}

fn match_ast_for_result<'a>(
    node: &'a A11yNode,
    ast: &SelectorAST,
    node_index: usize,
) -> Option<&'a A11yNode> {
    if node_index >= ast.nodes.len() {
        return None;
    }

    let target = &ast.nodes[node_index];
    let is_last = node_index == ast.nodes.len() - 1;
    let is_descendant = node_index == 0
        || matches!(
            ast.combinators.get(node_index - 1),
            Some(Combinator::Descendant) | None
        );

    if is_descendant {
        // Search entire subtree
        if matches_node(node, target, None) {
            if is_last {
                return Some(node);
            }
            // Stay on this node so the next combinator resolves correctly
            if let Some(result) = match_ast_for_result(node, ast, node_index + 1) {
                return Some(result);
            }
        }
        // Recurse into children
        if let Some(children) = &node.children {
            for child in children {
                if let Some(result) = match_ast_for_result(child, ast, node_index) {
                    return Some(result);
                }
            }
        }
    } else {
        // Child combinator: only check direct children
        if let Some(children) = &node.children {
            for (i, child) in children.iter().enumerate() {
                if matches_node(child, target, Some(i)) {
                    if is_last {
                        return Some(child);
                    }
                    if let Some(result) = match_ast_for_result(child, ast, node_index + 1) {
                        return Some(result);
                    }
                }
            }
        }
    }

    None
}

/// Find the first element matching a CSS-like selector.
pub fn query_selector<'a>(root: &'a A11yNode, selector: &str) -> Option<&'a A11yNode> {
    let ast = parse_selector(selector);
    match_ast_for_result(root, &ast, 0)
}

/// Find all elements matching a CSS-like selector.
pub fn query_selector_all<'a>(root: &'a A11yNode, selector: &str) -> Vec<&'a A11yNode> {
    let ast = parse_selector(selector);
    let mut results = Vec::new();
    collect_matches(root, &ast, 0, &mut results);
    results
}

fn collect_matches<'a>(
    node: &'a A11yNode,
    ast: &SelectorAST,
    start_index: usize,
    results: &mut Vec<&'a A11yNode>,
) {
    if start_index >= ast.nodes.len() {
        return;
    }

    let target = &ast.nodes[start_index];
    let is_last = start_index == ast.nodes.len() - 1;
    let is_descendant = start_index == 0
        || matches!(
            ast.combinators.get(start_index - 1),
            Some(Combinator::Descendant) | None
        );

    if is_descendant {
        if matches_node(node, target, None) {
            if is_last {
                results.push(node);
            } else {
                // Stay on this node so the next combinator resolves correctly
                collect_matches(node, ast, start_index + 1, results);
            }
        }
        if let Some(children) = &node.children {
            for child in children {
                collect_matches(child, ast, start_index, results);
            }
        }
    } else if let Some(children) = &node.children {
        for (i, child) in children.iter().enumerate() {
            if matches_node(child, target, Some(i)) {
                if is_last {
                    results.push(child);
                } else {
                    collect_matches(child, ast, start_index + 1, results);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ia::types::A11yNode;

    fn node(role: &str, name: &str, children: Option<Vec<A11yNode>>) -> A11yNode {
        A11yNode {
            role: role.into(),
            name: name.into(),
            bounds: None,
            children,
            parent_index: None,
            window: None,
            states: None,
        }
    }

    fn messages_tree() -> A11yNode {
        node(
            "desktop-frame",
            "main",
            Some(vec![node(
                "list",
                "Messages",
                Some(vec![
                    node("list-item", "08:35", None),
                    node("list-item", "Audio2\u{201d}sec\n", None),
                    node("list-item", "Audio2\u{201d}secUnplay\n", None),
                ]),
            )]),
        )
    }

    #[test]
    fn test_audio_unplay_selector_with_s_flag() {
        let tree = messages_tree();
        let results = query_selector_all(
            &tree,
            r#"list[name="Messages"] > list-item[name=/^Audio.*Unplay/s]"#,
        );
        assert_eq!(results.len(), 1, "should find 1 unplayed audio item");
        assert!(results[0].name.contains("Unplay"));
    }

    #[test]
    fn test_dot_does_not_match_newline_without_s_flag() {
        let tree = node("root", "", Some(vec![node("item", "Hello\nWorld", None)]));
        // Without s flag, . doesn't match \n
        let results = query_selector_all(&tree, r#"item[name=/Hello.*World/]"#);
        assert_eq!(results.len(), 0);
        // With s flag, it does
        let results = query_selector_all(&tree, r#"item[name=/Hello.*World/s]"#);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_audio_unplay_with_plain_quote() {
        // Test with ASCII double quote instead of unicode right quote
        let tree = node(
            "desktop-frame",
            "main",
            Some(vec![node(
                "list",
                "Messages",
                Some(vec![node("list-item", "Audio2\"secUnplay\n", None)]),
            )]),
        );
        let results = query_selector_all(
            &tree,
            r#"list[name="Messages"] > list-item[name=/^Audio.*Unplay/s]"#,
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_regex_case_insensitive_flag() {
        let tree = node(
            "root",
            "",
            Some(vec![
                node("button", "Submit", None),
                node("button", "cancel", None),
            ]),
        );
        let results = query_selector_all(&tree, r#"button[name=/submit/i]"#);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Submit");
    }

    #[test]
    fn test_child_combinator_query_selector() {
        let tree = messages_tree();
        let result = query_selector(&tree, r#"list[name="Messages"] > list-item"#);
        assert!(result.is_some(), "should find first list-item child");
        assert_eq!(result.unwrap().name, "08:35");
    }

    #[test]
    fn test_child_combinator_query_selector_all() {
        let tree = messages_tree();
        let results = query_selector_all(&tree, r#"list[name="Messages"] > list-item"#);
        assert_eq!(results.len(), 3, "should find all 3 list-item children");
    }

    #[test]
    fn test_descendant_combinator() {
        let tree = messages_tree();
        // Descendant (space) should find list-items anywhere under root
        let results = query_selector_all(&tree, r#"list-item"#);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_regex_combined_flags() {
        let tree = node("root", "", Some(vec![node("item", "Hello\nWorld", None)]));
        let results = query_selector_all(&tree, r#"item[name=/hello.*world/is]"#);
        assert_eq!(results.len(), 1);
    }
}
