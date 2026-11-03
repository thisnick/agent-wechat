use anyhow::Result;
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy)]
pub(crate) struct Output {
    json: bool,
}

impl Output {
    pub(crate) fn new(json: bool) -> Self {
        Self { json }
    }

    pub(crate) fn print(self, value: &impl serde::Serialize) -> Result<()> {
        let value = serde_json::to_value(value)?;
        super::outcome::record(&value);
        if self.json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            print!("{}", details(&value));
        }
        Ok(())
    }

    pub(crate) fn chats(self, value: &Value) -> Result<()> {
        if self.json {
            return self.print(value);
        }
        let Some(items) = value["items"].as_array() else {
            return self.print(value);
        };
        super::outcome::record(value);
        if items.is_empty() {
            println!("No chats found.");
        } else {
            print_table(
                ["Name", "Chat ID", "Kind", "Unread", "Last message"],
                items.iter().map(chat_row),
            );
        }
        print_cursor(value);
        if value["coverage"]["partial"] == true {
            println!("Only partial chat data is available.");
        }
        Ok(())
    }

    pub(crate) fn messages(self, value: &Value) -> Result<()> {
        if self.json {
            return self.print(value);
        }
        let Some(items) = value["items"].as_array() else {
            return self.print(value);
        };
        super::outcome::record(value);
        if items.is_empty() {
            println!("No messages found.");
        } else {
            print_table(
                ["Sender", "Message", "Timestamp", "ID"],
                items.iter().map(message_row),
            );
        }
        print_cursor(value);
        if value["coverage"]["partial"] == true {
            println!("Only partial message history is available.");
        }
        Ok(())
    }
}

fn print_table<const N: usize>(headers: [&str; N], items: impl Iterator<Item = [String; N]>) {
    let mut rows = vec![headers.map(str::to_owned)];
    rows.extend(items);
    let mut widths = [0; N];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.width());
        }
    }
    for row in rows {
        let cells: [String; N] = std::array::from_fn(|index| padded(&row[index], widths[index]));
        println!("{}", cells.join("  ").trim_end());
    }
}

fn print_cursor(value: &Value) {
    if let Some(cursor) = value["nextCursor"].as_str() {
        println!("Next cursor: {}", one_line(cursor));
    }
}

fn message_row(message: &Value) -> [String; 4] {
    let timestamp = message["createdAt"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            let timestamp = chrono::DateTime::from_timestamp(message["createTime"].as_i64()?, 0)?;
            Some(timestamp.to_rfc3339())
        });
    let kind = message["type"]
        .as_i64()
        .or_else(|| message["messageType"].as_i64());
    let sender = if message["direction"] == "outgoing" || message["outgoing"] == true {
        "You"
    } else {
        message["senderId"]
            .as_str()
            .or_else(|| message["sender"].as_str())
            .unwrap_or(if matches!(kind, Some(10000 | 10002)) {
                "System"
            } else {
                "Unknown sender"
            })
    };
    let body = message["recall"]["replacementText"]
        .as_str()
        .or_else(|| message["body"].as_str())
        .or_else(|| message["content"]["text"].as_str())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(match kind {
            Some(3) => "[Photo]",
            Some(34) => "[Voice message]",
            Some(43 | 62) => "[Video]",
            Some(47) => "[Sticker]",
            Some(49) => "[Shared content]",
            _ => "[Message text unavailable]",
        });
    let id = message["id"]
        .as_str()
        .or_else(|| message["observationId"].as_str())
        .unwrap_or("—");
    // Encoded IDs start with shared metadata. Keep the suffix, which contains
    // the local message number, so nearby rows are easier to distinguish.
    let id = if id.len() > 12 && id.is_ascii() {
        format!("…{}", one_line(&id[id.len() - 11..]))
    } else {
        shortened(id, 12)
    };
    [
        shortened(sender, 18),
        shortened(&body.split_whitespace().collect::<Vec<_>>().join(" "), 42),
        one_line(timestamp.as_deref().unwrap_or("—")),
        id,
    ]
}

fn chat_row(chat: &Value) -> [String; 5] {
    let id = chat["id"].as_str().or_else(|| chat["username"].as_str());
    let name = chat["displayName"]
        .as_str()
        .or_else(|| chat["label"].as_str())
        .filter(|name| !name.is_empty())
        .or(id);
    let kind = chat["kind"].as_str().or_else(|| {
        chat["username"].as_str().map(|username| {
            if username.ends_with("@chatroom") {
                "group"
            } else {
                "direct"
            }
        })
    });
    [
        shortened(name.unwrap_or("—"), 28),
        one_line(id.unwrap_or("—")),
        one_line(kind.unwrap_or("—")),
        chat["unreadCount"]
            .as_i64()
            .map_or_else(|| "—".into(), |count| count.to_string()),
        shortened(chat["lastMessage"]["text"].as_str().unwrap_or("—"), 40),
    ]
}

fn shortened(text: &str, limit: usize) -> String {
    let text = one_line(text);
    if text.width() <= limit {
        return text;
    }
    let mut width = 0;
    let mut preview = String::new();
    for c in text.chars() {
        width += c.width().unwrap_or(0);
        if width >= limit {
            break;
        }
        preview.push(c);
    }
    preview.push('…');
    preview
}

fn padded(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width - text.width()))
}

pub(crate) fn details(value: &Value) -> String {
    let mut result = String::new();
    render(&mut result, value, 0, "", true);
    result
}

fn render(result: &mut String, value: &Value, depth: usize, prefix: &str, labels: bool) {
    let has_children = match value {
        Value::Object(fields) => !fields.is_empty(),
        Value::Array(items) => !items.is_empty(),
        _ => false,
    };
    let has_prefix = !prefix.is_empty();
    if has_prefix || !has_children {
        result.push_str(&"  ".repeat(depth));
    }
    if has_children && has_prefix {
        result.push_str(prefix.trim_end());
        result.push('\n');
    } else {
        result.push_str(prefix);
    }
    let child_depth = depth + usize::from(has_prefix);
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            for (key, value) in fields {
                let field = if labels { label(key) } else { one_line(key) };
                // Saved servers and profiles are maps keyed by the user's name.
                let child_labels = key != "items" || !value.is_object();
                render(
                    result,
                    value,
                    child_depth,
                    &format!("{field}: "),
                    child_labels,
                );
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for item in items {
                render(result, item, child_depth, "- ", true);
            }
        }
        Value::Object(_) | Value::Array(_) => result.push_str("(none)\n"),
        value => {
            result.push_str(&scalar(value));
            result.push('\n');
        }
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "—".into(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::String(text) => one_line(text),
        value => value.to_string(),
    }
}

fn one_line(text: &str) -> String {
    let mut safe = String::new();
    for c in text.chars() {
        if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
            safe.extend(c.escape_debug());
        } else {
            safe.push(c);
        }
    }
    safe
}

fn label(key: &str) -> String {
    let mut chars = key.chars();
    let mut label = String::new();
    let Some(first) = chars.next() else {
        return label;
    };
    label.extend(first.to_uppercase());
    let mut previous = first;
    for c in chars {
        if c.is_ascii_uppercase() && (previous.is_ascii_lowercase() || previous.is_ascii_digit()) {
            label.push(' ');
        }
        label.push(c);
        previous = c;
    }
    let words = label.split(' ').map(|word| match word {
        "Id" => "ID",
        "Url" => "URL",
        word => word,
    });
    one_line(&words.collect::<Vec<_>>().join(" "))
}
