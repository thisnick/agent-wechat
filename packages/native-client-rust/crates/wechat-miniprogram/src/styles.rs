//! Decode compiler stylesheet data without evaluating JavaScript helpers.

use crate::compiled::{children, decode_string, literal, parse, source_at};
use crate::valid_path;
use anyhow::{bail, ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

const MAX_CSS: usize = 512 * 1024;

#[derive(PartialEq)]
enum Token {
    Text(String),
    Rpx(f64),
    Scope,
    Import(String),
}

#[derive(Default)]
pub(super) struct Styles {
    common: BTreeMap<String, Vec<Token>>,
    entries: BTreeMap<String, Vec<Token>>,
}

impl Styles {
    pub(super) fn parse(source: &str) -> Result<Self> {
        let tree = parse(source)?;
        let mut result = Self::default();
        let mut pending = children(tree.root_node()).rev().collect::<Vec<_>>();
        while let Some(node) = pending.pop() {
            match node.kind() {
                "expression_statement"
                | "sequence_expression"
                | "statement_block"
                | "parenthesized_expression" => pending.extend(children(node).rev()),
                "assignment_expression" => result.assignment(source, node)?,
                "call_expression" => {
                    if let Some(call) = node
                        .child_by_field_name("function")
                        .filter(|call| call.kind() == "call_expression")
                        .filter(|call| {
                            call.child_by_field_name("function")
                                .is_some_and(|f| source_at(source, f) == "setCssToHead")
                        })
                    {
                        let data = call
                            .child_by_field_name("arguments")
                            .and_then(|args| children(args).next())
                            .context("Missing global compiled stylesheet")?;
                        result
                            .entries
                            .entry("app.wxss".to_owned())
                            .or_default()
                            .extend(tokens(source, data)?);
                    }
                }
                "if_statement" => {
                    let condition = node
                        .child_by_field_name("condition")
                        .and_then(|node| children(node).next());
                    if condition.is_some_and(|node| {
                        node.kind() == "unary_expression"
                            && node
                                .child_by_field_name("operator")
                                .is_some_and(|op| source_at(source, op) == "!")
                            && node
                                .child_by_field_name("argument")
                                .is_some_and(|arg| compiler_guard(source, arg))
                    }) {
                        pending.extend(node.child_by_field_name("consequence"));
                    }
                }
                // Do not enter functions, plugin scopes or computed branches.
                _ => {}
            }
        }
        Ok(result)
    }

    fn assignment(&mut self, source: &str, node: Node<'_>) -> Result<()> {
        let (Some(left), Some(mut right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return Ok(());
        };
        if left.kind() != "subscript_expression" {
            return Ok(());
        }
        let Some(object) = left.child_by_field_name("object") else {
            return Ok(());
        };
        let map = match source_at(source, object) {
            "__COMMON_STYLESHEETS__" => &mut self.common,
            "__wxAppCode__" => {
                if right.kind() != "call_expression"
                    || right
                        .child_by_field_name("function")
                        .is_none_or(|node| source_at(source, node) != "setCssToHead")
                {
                    return Ok(());
                }
                right = right
                    .child_by_field_name("arguments")
                    .and_then(|node| children(node).next())
                    .context("Missing compiled stylesheet data")?;
                &mut self.entries
            }
            _ => return Ok(()),
        };
        let name = left
            .child_by_field_name("index")
            .and_then(|node| literal(source, node))
            .context("Unsupported compiled stylesheet name")?;
        let name = name.strip_prefix("./").unwrap_or(name);
        ensure!(
            valid_path(name) && name.ends_with(".wxss"),
            "Invalid compiled stylesheet name"
        );
        let tokens = tokens(source, right)?;
        if let Some(previous) = map.get(name) {
            ensure!(
                previous == &tokens,
                "Conflicting compiled stylesheet registration"
            );
        } else {
            map.insert(name.to_owned(), tokens);
        }
        Ok(())
    }

    pub(super) fn decode(&self) -> Result<BTreeMap<String, String>> {
        let mut result = BTreeMap::new();
        let mut total = 0;
        for (name, tokens) in &self.entries {
            let mut css = String::new();
            self.expand(tokens, &mut BTreeSet::new(), &mut css, 0)?;
            total += css.len();
            ensure!(
                total <= 8 * 1024 * 1024,
                "Compiled styles exceed package limit"
            );
            result.insert(name.trim_end_matches(".wxss").to_owned(), css);
        }
        Ok(result)
    }

    fn expand(
        &self,
        tokens: &[Token],
        seen: &mut BTreeSet<String>,
        out: &mut String,
        depth: usize,
    ) -> Result<()> {
        ensure!(depth <= 32, "Compiled stylesheet import depth exceeded");
        // The compiler walks backwards and keeps only the last import of each
        // common stylesheet, including cyclic imports. Preserve that ordering.
        let mut parts = Vec::new();
        let mut size = 0;
        for token in tokens.iter().rev() {
            let part = match token {
                Token::Text(text) => text.clone(),
                Token::Rpx(value) => format!("calc({value} * 100vw / 750)"),
                Token::Scope => String::new(), // Each view has its own style root.
                Token::Import(name) => {
                    if !seen.insert(name.clone()) {
                        continue;
                    }
                    let imported = self
                        .common
                        .get(name)
                        .context("Missing compiled stylesheet import")?;
                    let mut text = String::new();
                    self.expand(imported, seen, &mut text, depth + 1)?;
                    text
                }
            };
            size += part.len();
            ensure!(size <= MAX_CSS, "Compiled stylesheet exceeds size limit");
            parts.push(part);
        }
        for part in parts.into_iter().rev() {
            out.push_str(&part);
        }
        Ok(())
    }
}

fn compiler_guard(source: &str, node: Node<'_>) -> bool {
    if source_at(source, node) == "noCss" {
        return true;
    }
    if node.kind() != "call_expression" {
        return false;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return false;
    };
    function.kind() == "member_expression"
        && function
            .child_by_field_name("object")
            .is_some_and(|n| source_at(source, n) == "__COMMON_STYLESHEETS__")
        && function
            .child_by_field_name("property")
            .is_some_and(|n| source_at(source, n) == "hasOwnProperty")
        && node.child_by_field_name("arguments").is_some_and(|args| {
            let args = children(args).collect::<Vec<_>>();
            args.len() == 1
                && literal(source, args[0])
                    .is_some_and(|key| valid_path(key.strip_prefix("./").unwrap_or(key)))
        })
}

fn tokens(source: &str, node: Node<'_>) -> Result<Vec<Token>> {
    ensure!(
        node.kind() == "array",
        "Unsupported compiled stylesheet data"
    );
    children(node)
        .map(|node| {
            if node.kind() == "string" {
                return Ok(Token::Text(decode_string(source_at(source, node))?));
            }
            ensure!(
                node.kind() == "array",
                "Unsupported compiled stylesheet token"
            );
            let fields = children(node).collect::<Vec<_>>();
            let op = fields.first().map(|node| source_at(source, *node));
            match (op, fields.len()) {
                (Some("0"), 2) => {
                    let value: f64 = source_at(source, fields[1])
                        .parse()
                        .context("Invalid responsive CSS value")?;
                    ensure!(
                        value.is_finite() && value.abs() <= 1e9,
                        "Invalid responsive CSS value"
                    );
                    Ok(Token::Rpx(value))
                }
                (Some("1"), 1) => Ok(Token::Scope),
                (Some("2"), 2) => {
                    let name = literal(source, fields[1]).context("Invalid compiled CSS import")?;
                    let name = name.strip_prefix("./").unwrap_or(name);
                    ensure!(
                        valid_path(name) && name.ends_with(".wxss"),
                        "Invalid compiled CSS import"
                    );
                    Ok(Token::Import(name.to_owned()))
                }
                _ => bail!("Unsupported compiled stylesheet opcode"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_javascript_strings_without_evaluation() {
        assert_eq!(
            decode_string(r#"'a\x20\'b\"\u4e2d\uD83D\uDE00\u{1f600}'"#).unwrap(),
            "a 'b\"中😀😀"
        );
        assert!(decode_string(r#""\uD800""#).is_err());
        assert!(decode_string(r#""\123""#).is_err());
    }

    #[test]
    fn styles_preserve_import_order_and_do_not_execute_scripts() {
        let styles = Styles::parse(r#"
            if (!noCss) { if (!__COMMON_STYLESHEETS__.hasOwnProperty('./shared.wxss')) __COMMON_STYLESHEETS__['./shared.wxss'] = [".shared{margin:",[0,-15],"}"]; }
            if (!noCss) { __wxAppCode__['pages/home.wxss'] = setCssToHead([[2,"./shared.wxss"],".",[1],"page{color:red}",[2,"./shared.wxss"]]); }
            function ignored(){__wxAppCode__['pages/fake.wxss']=setCssToHead(["fake"])}
        "#).unwrap().decode().unwrap();
        assert_eq!(styles.len(), 1);
        assert_eq!(
            styles["pages/home"],
            ".page{color:red}.shared{margin:calc(-15 * 100vw / 750)}"
        );
    }

    #[test]
    fn decodes_eager_global_styles_in_source_order() {
        let styles = Styles::parse(r#"if (!noCss) { setCssToHead(["a{}"])(); setCssToHead(["b{}"],undefined,{path:"./app.wxss"})(); }"#).unwrap().decode().unwrap();
        assert_eq!(styles["app"], "a{}b{}");
    }

    #[test]
    fn styles_reject_missing_imports_unknown_tokens_and_conflicting_entries() {
        assert!(
            Styles::parse(r#"__wxAppCode__['p.wxss']=setCssToHead([[2,"missing.wxss"]]);"#)
                .unwrap()
                .decode()
                .is_err()
        );
        assert!(Styles::parse(r#"__wxAppCode__['p.wxss']=setCssToHead([[99]]);"#).is_err());
        assert!(Styles::parse(r#"__wxAppCode__['p.wxss']=setCssToHead(["a"]);__wxAppCode__['p.wxss']=setCssToHead(["b"]);"#).is_err());
    }

    #[test]
    fn styles_bound_import_expansion_and_deduplicate_cycles() {
        let source = r#"__COMMON_STYLESHEETS__['a.wxss']=[[2,"a.wxss"],"a"];
            __wxAppCode__['p.wxss']=setCssToHead([[2,"a.wxss"]]);"#;
        assert_eq!(Styles::parse(source).unwrap().decode().unwrap()["p"], "a");
        let source = format!(
            "__wxAppCode__['p.wxss']=setCssToHead([{}]);",
            serde_json::to_string(&"a".repeat(MAX_CSS + 1)).unwrap()
        );
        assert!(Styles::parse(&source).unwrap().decode().is_err());
    }
}
