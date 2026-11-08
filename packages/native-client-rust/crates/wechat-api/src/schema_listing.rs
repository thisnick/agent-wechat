//! Shared flattening of JSON schemas into `ITEM | ATTRIBUTES | DOC` rows, used by
//! the interface-listing tests for the calls WebSocket and the HTTP routes.
use schemars::generate::Contract;
use serde_json::{Map, Value};
use std::{collections::BTreeMap, collections::BTreeSet, fs, path::PathBuf};

pub(crate) const MISSING: &str = "MISSING DESCRIPTION";
const UPDATE: &str = "AGENT_WECHAT_UPDATE_INTERFACES";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Direction {
    Client,
    Server,
}

impl Direction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
    pub(crate) fn contract(self) -> Contract {
        match self {
            Self::Client => Contract::Deserialize,
            Self::Server => Contract::Serialize,
        }
    }
}

pub(crate) struct Row {
    pub(crate) item: String,
    pub(crate) attributes: Vec<String>,
    pub(crate) doc: String,
}

impl Row {
    pub(crate) fn render(&self) -> String {
        let attributes = if self.attributes.is_empty() {
            "-".to_owned()
        } else {
            self.attributes.join(", ")
        };
        for text in [&self.item, &attributes, &self.doc] {
            assert!(
                !text.contains(" | ") && !text.contains('\n'),
                "listing cell must stay on one line without ' | ': {text}"
            );
        }
        format!("{} | {attributes} | {}", self.item, self.doc)
    }
}

/// Collapses a doc comment to one line.
pub(crate) fn describe(text: Option<&Value>) -> String {
    text.and_then(Value::as_str)
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| MISSING.to_owned())
}

pub(crate) fn literal(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(crate) fn is_null_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
}

/// A schema with references, nullability and sibling keywords folded in.
pub(crate) struct Shape {
    pub(crate) schema: Map<String, Value>,
    pub(crate) nullable: bool,
    /// Set when the schema refers to a definition that is already being listed.
    pub(crate) cycle: Option<String>,
}

/// Flattens one message type's schema into rows.
pub(crate) struct Walker<'a> {
    pub(crate) defs: &'a Map<String, Value>,
    pub(crate) direction: Direction,
    /// Definitions being expanded, to stop a recursive type.
    pub(crate) stack: Vec<String>,
    pub(crate) rows: Vec<Row>,
}

impl<'a> Walker<'a> {
    pub(crate) fn new(defs: &'a Map<String, Value>, direction: Direction) -> Self {
        Self {
            defs,
            direction,
            stack: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Resolves `$ref`, unwraps `Option`-style nullability and merges sibling
    /// keywords over the referenced definition, so a field's own doc comment
    /// replaces the doc comment of its type.
    pub(crate) fn normalize(&mut self, schema: &Value) -> Shape {
        let mut current = schema.as_object().cloned().unwrap_or_default();
        let mut nullable = false;
        loop {
            if let Some(reference) = current.remove("$ref") {
                let name = reference
                    .as_str()
                    .and_then(|reference| reference.strip_prefix("#/$defs/"))
                    .unwrap_or_else(|| panic!("unsupported schema reference {reference}"))
                    .to_owned();
                if self.stack.contains(&name) {
                    return Shape {
                        schema: current,
                        nullable,
                        cycle: Some(name),
                    };
                }
                let mut base = self
                    .defs
                    .get(&name)
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| panic!("schema has no definition {name}"))
                    .clone();
                self.stack.push(name);
                base.extend(current);
                current = base;
                continue;
            }
            if let Some(Value::Array(branches)) = current.get("anyOf") {
                let others: Vec<&Value> = branches.iter().filter(|b| !is_null_schema(b)).collect();
                if others.len() == 1 && others.len() < branches.len() {
                    nullable = true;
                    let mut base = others[0].as_object().cloned().unwrap_or_default();
                    current.remove("anyOf");
                    base.extend(current);
                    current = base;
                    continue;
                }
                if others.len() > 1 && others.len() < branches.len() {
                    nullable = true;
                    let kept: Vec<Value> = others.into_iter().cloned().collect();
                    current.insert("anyOf".into(), Value::Array(kept));
                }
            }
            if let Some(Value::Array(types)) = current.get("type") {
                let kept: Vec<Value> = types
                    .iter()
                    .filter(|t| t.as_str() != Some("null"))
                    .cloned()
                    .collect();
                if kept.len() < types.len() {
                    nullable = true;
                    let replacement = if kept.len() == 1 {
                        kept[0].clone()
                    } else {
                        Value::Array(kept)
                    };
                    current.insert("type".into(), replacement);
                }
            }
            return Shape {
                schema: current,
                nullable,
                cycle: None,
            };
        }
    }

    pub(crate) fn walk(
        &mut self,
        item: String,
        schema: &Value,
        required: bool,
        when: Option<String>,
    ) {
        let depth = self.stack.len();
        let shape = self.normalize(schema);
        if let Some(name) = &shape.cycle {
            let mut attributes = vec!["object".to_owned()];
            attributes.extend(required.then(|| "required".to_owned()));
            attributes.push(format!("recursive, see {name}"));
            self.rows.push(Row {
                item,
                attributes,
                doc: describe(shape.schema.get("description")),
            });
        } else {
            self.emit(item, &shape, required, when);
        }
        self.stack.truncate(depth);
    }

    pub(crate) fn type_label(schema: &Map<String, Value>) -> String {
        let from_branches = || {
            schema
                .get("oneOf")
                .or_else(|| schema.get("anyOf"))
                .and_then(Value::as_array)
                .and_then(|branches| branches.first())
                .and_then(|branch| branch.get("type"))
        };
        match schema.get("type").or_else(from_branches) {
            Some(Value::String(kind)) => kind.clone(),
            Some(Value::Array(kinds)) => kinds
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|"),
            _ if schema.contains_key("oneOf") || schema.contains_key("anyOf") => {
                "object".to_owned()
            }
            _ => "any".to_owned(),
        }
    }

    /// Attributes shared by every kind of value.
    pub(crate) fn attributes(
        &self,
        schema: &Map<String, Value>,
        nullable: bool,
        required: bool,
        when: Option<String>,
    ) -> Vec<String> {
        let mut attributes = vec![Self::type_label(schema)];
        if required {
            attributes.push("required".into());
        }
        if nullable {
            attributes.push("nullable".into());
        }
        if let Some(Value::Array(values)) = schema.get("enum") {
            let values: Vec<_> = values.iter().map(literal).collect();
            attributes.push(format!("values {}", values.join("|")));
        }
        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            if branches.iter().all(|branch| branch.get("const").is_some()) {
                let values: Vec<_> = branches.iter().map(|b| literal(&b["const"])).collect();
                attributes.push(format!("values {}", values.join("|")));
            }
        }
        if let Some(value) = schema.get("const") {
            attributes.push(format!("const {}", literal(value)));
        }
        for (key, label) in [
            ("minimum", "min"),
            ("maximum", "max"),
            ("minLength", "min length"),
            ("maxLength", "max length"),
            ("minItems", "min items"),
            ("maxItems", "max items"),
            ("pattern", "pattern"),
            ("format", "format"),
        ] {
            if let Some(value) = schema.get(key) {
                attributes.push(format!("{label} {}", literal(value)));
            }
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
            attributes.push("unique items".into());
        }
        // The daemon always sends a field it serializes, so a default only
        // tells a client what omitting the field means.
        if self.direction == Direction::Client {
            if let Some(value) = schema.get("default") {
                attributes.push(format!("default {}", literal(value)));
            }
        }
        if self.direction == Direction::Client
            && schema.get("additionalProperties") == Some(&Value::Bool(false))
        {
            attributes.push("rejects unknown fields".into());
        }
        attributes.extend(when);
        attributes
    }

    pub(crate) fn emit(
        &mut self,
        item: String,
        shape: &Shape,
        required: bool,
        when: Option<String>,
    ) {
        let schema = &shape.schema;
        let doc = describe(schema.get("description"));
        let mut attributes = self.attributes(schema, shape.nullable, required, when);
        let alternatives = schema
            .get("oneOf")
            .or_else(|| schema.get("anyOf"))
            .and_then(Value::as_array);
        if let Some(branches) = alternatives {
            if !branches.iter().all(|branch| branch.get("const").is_some()) {
                self.rows.push(Row {
                    item: item.clone(),
                    attributes,
                    doc,
                });
                self.union(&item, branches);
                return;
            }
        }
        if schema.get("type").and_then(Value::as_str) == Some("array") {
            let items = schema.get("items").cloned().unwrap_or(Value::Bool(true));
            let depth = self.stack.len();
            let element = self.normalize(&items);
            let nested = element.cycle.is_none()
                && (element.schema.contains_key("properties")
                    || element.schema.contains_key("oneOf")
                    || element.schema.contains_key("anyOf"));
            let label = Self::type_label(&element.schema);
            attributes[0] = format!("array of {label}");
            if !nested {
                let mut element_attributes = self.attributes(&element.schema, false, false, None);
                // The first entry repeats the type label already in the array's own.
                element_attributes.remove(0);
                attributes.extend(
                    element_attributes
                        .into_iter()
                        .map(|attribute| format!("items {attribute}")),
                );
            }
            self.rows.push(Row {
                item: item.clone(),
                attributes,
                doc,
            });
            if nested {
                self.emit(format!("{item}[]"), &element, false, None);
            }
            self.stack.truncate(depth);
            return;
        }
        self.rows.push(Row {
            item: item.clone(),
            attributes,
            doc,
        });
        self.properties(&item, schema);
    }

    /// Rows for the properties of an object schema.
    pub(crate) fn properties(&mut self, item: &str, schema: &Map<String, Value>) {
        let Some(Value::Object(properties)) = schema.get("properties") else {
            return;
        };
        let required = required_keys(schema);
        for (key, property) in properties {
            self.walk(
                format!("{item}.{key}"),
                property,
                required.iter().any(|name| name == key),
                None,
            );
        }
    }

    /// Rows for a nested message with several variants. The tag becomes one
    /// field listing every value; other fields say which variants carry them.
    pub(crate) fn union(&mut self, item: &str, branches: &[Value]) {
        let depth = self.stack.len();
        let shapes: Vec<Shape> = branches.iter().map(|b| self.normalize(b)).collect();
        let objects: Vec<&Map<String, Value>> = shapes.iter().map(|shape| &shape.schema).collect();
        let properties = |object: &Map<String, Value>| {
            object
                .get("properties")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        };
        let tag = properties(objects[0])
            .into_iter()
            .find(|(key, _)| {
                objects.iter().all(|object| {
                    properties(object)
                        .get(key)
                        .is_some_and(|p| p.get("const").is_some())
                })
            })
            .map(|(key, _)| key);
        let Some(tag) = tag else {
            // An untagged union: list each alternative under its own name.
            self.stack.truncate(depth);
            for (index, branch) in branches.iter().enumerate() {
                let label = branch_label(branch, index);
                self.walk(format!("{item}<{label}>"), branch, false, None);
            }
            return;
        };
        let tag_value = |object: &Map<String, Value>| literal(&properties(object)[&tag]["const"]);
        let mut keys: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, object) in objects.iter().enumerate() {
            for key in properties(object).keys() {
                keys.entry(key.clone()).or_default().push(index);
            }
        }
        for (key, owners) in keys {
            let field = format!("{item}.{key}");
            let owner_values: Vec<String> = owners.iter().map(|&i| tag_value(objects[i])).collect();
            if key == tag {
                let docs: Vec<String> = owners
                    .iter()
                    .map(|&i| {
                        let doc = describe(objects[i].get("description"));
                        format!("{}: {}", tag_value(objects[i]), doc.trim_end_matches('.'))
                    })
                    .collect();
                let doc = if docs.iter().any(|doc| doc.contains(MISSING)) {
                    MISSING.to_owned()
                } else {
                    docs.join("; ")
                };
                self.rows.push(Row {
                    item: field,
                    attributes: vec![
                        "string".into(),
                        "required".into(),
                        format!("values {}", owner_values.join("|")),
                    ],
                    doc,
                });
                continue;
            }
            let first = properties(objects[owners[0]])[&key].clone();
            for &i in &owners[1..] {
                let other = properties(objects[i])[&key].clone();
                assert_eq!(
                    first, other,
                    "{field} differs between variants; the listing merges them"
                );
            }
            let all_required = owners
                .iter()
                .all(|&i| required_keys(objects[i]).iter().any(|name| name == &key));
            let when = (owners.len() < objects.len())
                .then(|| format!("when {tag} {}", owner_values.join("|")));
            self.walk(field, &first, all_required, when);
        }
        self.stack.truncate(depth);
    }
}

/// The definition a union alternative refers to, or a position when it is inline.
fn branch_label(branch: &Value, index: usize) -> String {
    branch
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.rsplit('/').next())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("variant{}", index + 1))
}

pub(crate) fn required_keys(schema: &Map<String, Value>) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Moves the `$defs` of schemas into one shared set of named components for an
/// OpenAPI or AsyncAPI document, rewriting references to match. Request types
/// are generated for deserializing and response types for serializing, so a
/// type whose two schemas differ gets an `Input` name for its request form.
pub(crate) struct Components {
    raw: [BTreeMap<String, Value>; 2],
}

impl Components {
    pub(crate) fn new() -> Self {
        Self {
            raw: [BTreeMap::new(), BTreeMap::new()],
        }
    }

    fn slot(direction: Direction) -> usize {
        match direction {
            Direction::Client => 0,
            Direction::Server => 1,
        }
    }

    /// Adds a schema and everything its `$defs` hold, under `name`.
    pub(crate) fn add(&mut self, direction: Direction, name: &str, root: &Value) {
        let mut root = root.as_object().cloned().unwrap_or_default();
        let defs = match root.remove("$defs") {
            Some(Value::Object(defs)) => defs,
            _ => Map::new(),
        };
        root.remove("$schema");
        root.remove("title");
        let set = &mut self.raw[Self::slot(direction)];
        for (key, mut value) in defs
            .into_iter()
            .chain([(name.to_owned(), Value::Object(root))])
        {
            if let Some(object) = value.as_object_mut() {
                object.remove("title");
            }
            if let Some(existing) = set.get(&key) {
                assert_eq!(existing, &value, "{key} has two different schemas");
            }
            set.insert(key, value);
        }
    }

    fn references(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    match (key.as_str(), child) {
                        ("$ref", Value::String(reference)) => {
                            if let Some(name) = reference.strip_prefix("#/$defs/") {
                                found.push(name.to_owned());
                            }
                        }
                        _ => Self::references(child, found),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| Self::references(item, found)),
            _ => {}
        }
    }

    /// Names whose request and response schemas are not the same.
    fn differing(&self) -> Vec<String> {
        let [client, server] = &self.raw;
        let mut differing: Vec<String> = client
            .iter()
            .filter(|(name, value)| server.get(*name).is_some_and(|other| other != *value))
            .map(|(name, _)| name.clone())
            .collect();
        loop {
            let before = differing.len();
            for (name, value) in client {
                if differing.contains(name) || !server.contains_key(name) {
                    continue;
                }
                let mut found = Vec::new();
                Self::references(value, &mut found);
                if found.iter().any(|reference| differing.contains(reference)) {
                    differing.push(name.clone());
                }
            }
            if differing.len() == before {
                return differing;
            }
        }
    }

    /// The component name of a type for one direction.
    pub(crate) fn name(&self, direction: Direction, name: &str) -> String {
        if direction == Direction::Client && self.differing().iter().any(|n| n == name) {
            format!("{name}Input")
        } else {
            name.to_owned()
        }
    }

    fn rewrite(value: &mut Value, direction: Direction, differing: &[String], prefix: &str) {
        match value {
            Value::Object(object) => {
                for (key, child) in object.iter_mut() {
                    if key == "$ref" {
                        if let Value::String(reference) = child {
                            if let Some(name) = reference.strip_prefix("#/$defs/") {
                                let suffix = if direction == Direction::Client
                                    && differing.iter().any(|n| n == name)
                                {
                                    "Input"
                                } else {
                                    ""
                                };
                                *reference = format!("{prefix}{name}{suffix}");
                            }
                        }
                    } else {
                        Self::rewrite(child, direction, differing, prefix);
                    }
                }
            }
            Value::Array(items) => items
                .iter_mut()
                .for_each(|item| Self::rewrite(item, direction, differing, prefix)),
            _ => {}
        }
    }

    /// Every collected schema by component name, with references rewritten.
    pub(crate) fn schemas(&self, prefix: &str) -> Map<String, Value> {
        let differing = self.differing();
        let mut schemas = Map::new();
        for direction in [Direction::Server, Direction::Client] {
            for (name, value) in &self.raw[Self::slot(direction)] {
                let suffix = if direction == Direction::Client && differing.contains(name) {
                    "Input"
                } else {
                    ""
                };
                let mut value = value.clone();
                Self::rewrite(&mut value, direction, &differing, prefix);
                schemas.entry(format!("{name}{suffix}")).or_insert(value);
            }
        }
        schemas
    }
}

pub(crate) fn to_json_text(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}

/// Rewrites `interfaces/http/NAME` when `AGENT_WECHAT_UPDATE_INTERFACES` is set,
/// and otherwise fails with a short diff when the file is out of date.
pub(crate) fn check_file(name: &str, expected: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../interfaces/http")
        .join(name);
    if std::env::var_os(UPDATE).is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, expected).unwrap();
        return;
    }
    let actual = fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    if actual != expected {
        let old = actual.lines().collect::<BTreeSet<_>>();
        let new = expected.lines().collect::<BTreeSet<_>>();
        let changes = old
            .difference(&new)
            .map(|line| format!("- {line}"))
            .chain(new.difference(&old).map(|line| format!("+ {line}")))
            .take(40)
            .collect::<Vec<_>>()
            .join("\n");
        panic!("interfaces/http/{name} does not match the code. Run `pnpm interfaces` and review the diff:\n{changes}");
    }
}

/// Replaces a `oneOf` or `anyOf` with a single alternative by that alternative.
pub(crate) fn collapse_single_alternatives(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for child in object.values_mut() {
                collapse_single_alternatives(child);
            }
            for key in ["oneOf", "anyOf"] {
                let single = match object.get(key) {
                    Some(Value::Array(items)) if items.len() == 1 => items[0].clone(),
                    _ => continue,
                };
                object.remove(key);
                if let Value::Object(inner) = single {
                    for (name, item) in inner {
                        object.entry(name).or_insert(item);
                    }
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(collapse_single_alternatives),
        _ => {}
    }
}
