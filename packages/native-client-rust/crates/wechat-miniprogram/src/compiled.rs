//! Index the compiler registrations in a service script without executing it.

use crate::valid_path;
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::{collections::BTreeMap, ops::Range};
use tree_sitter::{Node, Parser, Tree};

type Span = Range<usize>;

pub(super) struct Module {
    pub(super) factory: Span,
    pub(super) is_page: bool,
}

#[derive(Default)]
pub(super) struct ServiceIndex {
    pub(super) modules: BTreeMap<String, Module>,
    pub(super) configs: BTreeMap<String, Value>,
    pub(super) legacy_configs: BTreeMap<String, Value>,
    pub(super) glass: BTreeMap<String, Span>,
    pub(super) prelude: Option<Span>,
    pub(super) generators: BTreeMap<String, Span>,
    pub(super) legacy: BTreeMap<String, String>,
}

impl ServiceIndex {
    pub(super) fn parse(source: &str) -> Result<Self> {
        let tree = parse(source)?;
        let mut index = Self::default();
        let mut pending = children(tree.root_node()).rev().collect::<Vec<_>>();
        while let Some(node) = pending.pop() {
            match node.kind() {
                "expression_statement"
                | "sequence_expression"
                | "parenthesized_expression"
                | "statement_block"
                | "else_clause" => pending.extend(children(node).rev()),
                "if_statement" => {
                    if index.prelude.is_none() && is_prelude(source, node) {
                        index.prelude = Some(node.byte_range());
                    } else {
                        // Delayed and immediate template registration branches may
                        // both describe the same page.
                        pending.extend(node.child_by_field_name("alternative"));
                        pending.extend(node.child_by_field_name("consequence"));
                    }
                }
                "call_expression" => index.call(source, node)?,
                "assignment_expression" => index.assignment(source, node)?,
                _ => {}
            }
        }
        Ok(index)
    }

    fn call(&mut self, source: &str, node: Node<'_>) -> Result<()> {
        let Some(callee) = node.child_by_field_name("function") else {
            return Ok(());
        };
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return Ok(());
        };
        let mut args = children(arguments);
        match source_at(source, callee) {
            "define" => {
                let Some(path) = args.next().and_then(|arg| literal(source, arg)) else {
                    return Ok(());
                };
                if !valid_path(path) || !path.ends_with(".js") {
                    return Ok(());
                }
                let Some(factory) = args
                    .next()
                    .filter(|arg| arg.kind() == "function_expression")
                else {
                    return Ok(());
                };
                let Some(metadata) = args.next() else {
                    return Ok(());
                };
                if property(source, metadata, "currentFile").and_then(|node| literal(source, node))
                    != Some(path)
                    || !property(source, metadata, "isComponent")
                        .is_some_and(|flag| matches!(flag.kind(), "true" | "false"))
                {
                    return Ok(());
                }
                let Some(flag) = property(source, metadata, "isPage") else {
                    return Ok(());
                };
                if !matches!(flag.kind(), "true" | "false") {
                    return Ok(());
                }
                insert_unique(
                    &mut self.modules,
                    path,
                    Module {
                        factory: factory.byte_range(),
                        is_page: flag.kind() == "true",
                    },
                    |a, b| {
                        a.is_page == b.is_page
                            && source[a.factory.clone()] == source[b.factory.clone()]
                    },
                )?;
            }
            _ if is_member(
                source,
                callee,
                "__wxCodeSpace__",
                "addComponentStaticConfig",
            ) =>
            {
                if let (Some(path), Some(config)) = (
                    args.next().and_then(|arg| literal(source, arg)),
                    args.next(),
                ) {
                    if valid_path(path) && config.kind() == "object" {
                        let config = static_config(source, config)
                            .context("Unsupported Mini Program component configuration")?;
                        insert_unique(&mut self.configs, path, config, Value::eq)?;
                    }
                }
            }
            _ if is_member(
                source,
                callee,
                "__wxCodeSpace__",
                "batchAddCompiledTemplate",
            ) =>
            {
                let Some(factory) = args
                    .next()
                    .filter(|arg| arg.kind() == "function_expression")
                else {
                    return Ok(());
                };
                if let Some(object) = factory
                    .child_by_field_name("body")
                    .and_then(|body| children(body).next_back())
                    .filter(|stmt| stmt.kind() == "return_statement")
                    .and_then(|stmt| children(stmt).next())
                    .filter(|arg| arg.kind() == "object")
                {
                    for pair in children(object) {
                        if let Some(path) = pair
                            .child_by_field_name("key")
                            .and_then(|key| literal(source, key))
                            .filter(|path| valid_path(path))
                        {
                            insert_span(&mut self.glass, source, path, factory.byte_range())?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn assignment(&mut self, source: &str, node: Node<'_>) -> Result<()> {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return Ok(());
        };
        if left.kind() == "identifier"
            && generator_name(source_at(source, left))
            && right.kind() == "call_expression"
            && right
                .child_by_field_name("function")
                .is_some_and(|function| function.kind() == "function_expression")
        {
            insert_span(
                &mut self.generators,
                source,
                source_at(source, left),
                right.byte_range(),
            )?;
        } else if left.kind() == "subscript_expression"
            && left
                .child_by_field_name("object")
                .is_some_and(|object| source_at(source, object) == "__wxAppCode__")
        {
            let Some(path) = left
                .child_by_field_name("index")
                .and_then(|index| literal(source, index))
            else {
                return Ok(());
            };
            if let Some(path) = path.strip_suffix(".wxml").filter(|path| valid_path(path)) {
                let path = format!("./{path}.wxml");
                if let Some(name) = template_registration(source, right, &path) {
                    insert_unique(&mut self.legacy, &path, name.to_owned(), String::eq)?;
                }
            } else if let Some(path) = path
                .strip_suffix(".json")
                .filter(|path| valid_path(path) && right.kind() == "object")
            {
                let config = static_config(source, right)
                    .context("Unsupported legacy Mini Program component configuration")?;
                insert_unique(&mut self.legacy_configs, path, config, Value::eq)?;
            }
        }
        Ok(())
    }
}

fn insert_unique<T>(
    map: &mut BTreeMap<String, T>,
    path: &str,
    item: T,
    same: impl FnOnce(&T, &T) -> bool,
) -> Result<()> {
    use std::collections::btree_map::Entry;

    match map.entry(path.to_owned()) {
        Entry::Vacant(entry) => {
            entry.insert(item);
        }
        Entry::Occupied(entry) => {
            ensure!(
                same(entry.get(), &item),
                "Ambiguous Mini Program registration"
            );
        }
    }
    Ok(())
}

fn insert_span(
    map: &mut BTreeMap<String, Span>,
    source: &str,
    key: &str,
    span: Span,
) -> Result<()> {
    insert_unique(map, key, span, |a, b| {
        source[a.clone()] == source[b.clone()]
    })
}

/// Decode string tokens as JavaScript, then let JSON validate the object.
fn static_config(source: &str, node: Node<'_>) -> Result<Value> {
    let raw = source_at(source, node);
    if let Ok(config) = serde_json::from_str(raw) {
        return Ok(config);
    }
    let mut normalized = String::with_capacity(raw.len());
    let mut cursor = node.start_byte();
    let mut pending = vec![node];
    while let Some(part) = pending.pop() {
        if part.kind() == "string" {
            normalized.push_str(&source[cursor..part.start_byte()]);
            normalized.push_str(&serde_json::to_string(&decode_string(source_at(
                source, part,
            ))?)?);
            cursor = part.end_byte();
        } else {
            pending.extend(children(part).rev());
        }
    }
    normalized.push_str(&source[cursor..node.end_byte()]);
    Ok(serde_json::from_str(&normalized)?)
}

pub(super) fn decode_string(source: &str) -> Result<String> {
    let mut units = Vec::new();
    let mut chars = source[1..source.len() - 1].chars().peekable();
    while let Some(mut character) = chars.next() {
        if character == '\\' {
            character = chars
                .next()
                .context("Incomplete JavaScript string escape")?;
            match character {
                'x' | 'u' => {
                    let count = if character == 'x' { 2 } else { 4 };
                    if character == 'u' && chars.peek() == Some(&'{') {
                        chars.next();
                        let mut hex = String::new();
                        loop {
                            let ch = chars
                                .next()
                                .context("Incomplete JavaScript Unicode escape")?;
                            if ch == '}' {
                                break;
                            }
                            ensure!(
                                ch.is_ascii_hexdigit() && hex.len() < 6,
                                "Invalid JavaScript Unicode escape"
                            );
                            hex.push(ch);
                        }
                        character = char::from_u32(u32::from_str_radix(&hex, 16)?)
                            .context("Invalid JavaScript Unicode escape")?;
                    } else {
                        let mut value = 0u16;
                        for _ in 0..count {
                            value = value * 16
                                + chars
                                    .next()
                                    .and_then(|ch| ch.to_digit(16))
                                    .context("Invalid JavaScript string escape")?
                                    as u16;
                        }
                        units.push(value);
                        continue;
                    }
                }
                'n' => character = '\n',
                'r' => character = '\r',
                't' => character = '\t',
                'b' => character = '\u{0008}',
                'f' => character = '\u{000c}',
                'v' => character = '\u{000b}',
                '0' if !chars.peek().is_some_and(char::is_ascii_digit) => character = '\0',
                '0'..='9' => bail!("Legacy numeric JavaScript string escapes are unsupported"),
                '\n' | '\u{2028}' | '\u{2029}' => continue,
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    continue;
                }
                _ => {}
            }
        }
        units.extend_from_slice(character.encode_utf16(&mut [0; 2]));
    }
    String::from_utf16(&units).context("Invalid Unicode in JavaScript string")
}

pub(super) fn parse(source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .context("Could not parse Mini Program JavaScript")?;
    ensure!(
        !tree.root_node().has_error(),
        "Invalid Mini Program JavaScript"
    );
    Ok(tree)
}

pub(super) fn children(node: Node<'_>) -> impl DoubleEndedIterator<Item = Node<'_>> {
    (0..node.named_child_count())
        .filter_map(move |index| node.named_child(index))
        .filter(|child| child.kind() != "comment")
}

pub(super) fn source_at<'a>(source: &'a str, node: Node<'_>) -> &'a str {
    &source[node.byte_range()]
}

pub(super) fn literal<'a>(source: &'a str, node: Node<'_>) -> Option<&'a str> {
    if node.kind() != "string" {
        return None;
    }
    let token = source_at(source, node);
    let value = &token[1..token.len() - 1];
    (!value.contains('\\')).then_some(value)
}

fn property<'tree>(source: &str, object: Node<'tree>, key: &str) -> Option<Node<'tree>> {
    if object.kind() != "object" {
        return None;
    }
    let mut values = children(object).filter_map(|pair| {
        let name = pair.child_by_field_name("key")?;
        (literal(source, name).unwrap_or_else(|| source_at(source, name)) == key)
            .then(|| pair.child_by_field_name("value"))
            .flatten()
    });
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn is_member(source: &str, node: Node<'_>, object: &str, property: &str) -> bool {
    node.kind() == "member_expression"
        && node
            .child_by_field_name("object")
            .map(|part| source_at(source, part))
            == Some(object)
        && node
            .child_by_field_name("property")
            .map(|part| source_at(source, part))
            == Some(property)
}

fn is_prelude(source: &str, node: Node<'_>) -> bool {
    node.child_by_field_name("consequence")
        .filter(|stmt| stmt.kind() == "expression_statement")
        .and_then(|stmt| children(stmt).next())
        .filter(|expr| expr.kind() == "call_expression")
        .is_some_and(|call| {
            let Some(function) = call.child_by_field_name("function") else {
                return false;
            };
            let Some(args) = call.child_by_field_name("arguments") else {
                return false;
            };
            let mut args = children(args);
            is_member(source, function, "Object", "defineProperty")
                && args.next().is_some_and(|arg| arg.kind() == "this")
                && args.next().and_then(|arg| literal(source, arg)) == Some("__g")
        })
}

fn generator_name(name: &str) -> bool {
    name.strip_prefix("$gwx").is_some_and(|suffix| {
        suffix.len() <= 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn template_registration<'a>(source: &'a str, node: Node<'_>, path: &str) -> Option<&'a str> {
    let (function, argument) = match node.kind() {
        "call_expression" => (
            node.child_by_field_name("function")?,
            children(node.child_by_field_name("arguments")?).next()?,
        ),
        "array" => {
            let mut args = children(node);
            (args.next()?, args.next()?)
        }
        _ => return None,
    };
    let name = source_at(source, function);
    (function.kind() == "identifier"
        && generator_name(name)
        && literal(source, argument) == Some(path))
    .then_some(name)
}

/// Some older page files identify the generator in a separate WebView script.
pub(super) fn html_generator(html: &str, path: &str) -> Result<String> {
    let mut candidates = BTreeMap::new();
    let scripts = if html.trim_start().starts_with('<') {
        html.split("<script>")
            .skip(1)
            .filter_map(|part| part.split_once("</script>").map(|(js, _)| js))
            .collect()
    } else {
        vec![html]
    };
    for source in scripts {
        let tree = parse(source)?;
        let mut pending = vec![tree.root_node()];
        while let Some(node) = pending.pop() {
            if node.kind() == "call_expression" {
                if let Some(name) = template_registration(source, node, path) {
                    candidates.insert(name.to_owned(), ());
                }
            }
            pending.extend(children(node));
        }
    }
    ensure!(
        candidates.len() <= 1,
        "Ambiguous legacy Mini Program template"
    );
    candidates
        .into_keys()
        .next()
        .context("Missing legacy page template")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_compiled_configuration_strings_without_evaluating_expressions() {
        let source = r#"
            __wxCodeSpace__.addComponentStaticConfig("app",
                {"title":"Ticket\x3d国际", "children":[{"name":"\x26"}]});
        "#;
        let index = ServiceIndex::parse(source).unwrap();
        assert_eq!(index.configs["app"]["title"], "Ticket=国际");
        assert_eq!(index.configs["app"]["children"][0]["name"], "&");
        assert!(ServiceIndex::parse(&source.replace(r#""\x26""#, "getValue()")).is_err());
    }

    #[test]
    fn indexes_real_registrations_with_reordered_metadata_and_ignores_code_inside_modules() {
        let source = r#"
          // define("ignored.js", function(){}, {isPage:false,isComponent:false,currentFile:"ignored.js"});
          define(
            'pages/index.js', /* a compiler can place comments here */ function(require, module) {
                const sample = 'define("string.js", function(){})';
                define('nested.js', function(){}, {isPage:false,isComponent:false,currentFile:'nested.js'});
            },
            { currentFile: 'pages/index.js', isComponent: true, isPage: true },
          );
          __wxCodeSpace__.batchAddCompiledTemplate(function () {
              const local = {"not-a-page": true};
              return { 'pages/index': local };
          });
          __wxCodeSpace__.addComponentStaticConfig("pages/index", {"usingComponents":{"unit":"/components/unit"}});
          __wxAppCode__['pages/index.json'] = {"usingComponents":{"unit":"../components/unit"}};
        "#;
        let index = ServiceIndex::parse(source).unwrap();
        assert_eq!(index.modules.len(), 1);
        assert!(index.modules["pages/index.js"].is_page);
        assert_eq!(index.glass.keys().collect::<Vec<_>>(), ["pages/index"]);
        assert_eq!(
            index.configs["pages/index"]["usingComponents"]["unit"],
            "/components/unit"
        );
        assert_eq!(
            index.legacy_configs["pages/index"]["usingComponents"]["unit"],
            "../components/unit"
        );
    }

    #[test]
    fn accepts_matching_delayed_routes_and_rejects_conflicting_chunks() {
        let source = r#"
          if (this && this.__g === undefined) Object.defineProperty(this, "__g", {value: {aa:0}});
          $gwx_A = function () { return function() {}; }(__g.aa);
          $gwx_B = function () { return function() {}; }(__g.aa);
          if (__vd_version_info__.delayedGwx)
              __wxAppCode__["pages/index.wxml"] = [$gwx_A, "./pages/index.wxml"];
          else
              __wxAppCode__["pages/index.wxml"] = $gwx_A("./pages/index.wxml");
          __wxAppCode__["components/counter.wxml"] = $gwx_B("./components/counter.wxml");
        "#;
        let index = ServiceIndex::parse(source).unwrap();
        assert!(index.prelude.is_some());
        assert_eq!(index.generators.len(), 2);
        assert_eq!(index.legacy["./pages/index.wxml"], "$gwx_A");
        assert_eq!(index.legacy["./components/counter.wxml"], "$gwx_B");
        let conflicting = source.replace(
            "$gwx_A(\"./pages/index.wxml\")",
            "$gwx_B(\"./pages/index.wxml\")",
        );
        assert!(ServiceIndex::parse(&conflicting).is_err());
    }

    #[test]
    fn html_hint_requires_a_real_unambiguous_generator_call() {
        let html = r#"<script>
            var sample = '$gwx_Other("./pages/index.wxml")';
            (function () { const rendered = $gwx_Main( './pages/index.wxml' ); })();
        </script>"#;
        assert_eq!(
            html_generator(html, "./pages/index.wxml").unwrap(),
            "$gwx_Main"
        );
        assert!(html_generator(html, "./missing.wxml").is_err());
        assert!(html_generator("$gwx_A('./x.wxml'); $gwx_B('./x.wxml');", "./x.wxml").is_err());
    }
}
