use std::{collections::HashMap, sync::LazyLock};

// Shared with the webview; these are display approximations, never send data.
const ALIASES: &str = include_str!("../../../client-ui/src/emoji-aliases.json");
struct Catalog {
    brackets: HashMap<String, String>,
    slashes: Vec<(String, String)>,
    max_bytes: usize,
}
static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let entries: HashMap<String, String> =
        serde_json::from_str(ALIASES).expect("Embedded emoji aliases must be valid");
    let max_bytes = entries.keys().map(String::len).max().unwrap_or(0);
    let (brackets, slashes): (HashMap<_, _>, HashMap<_, _>) = entries
        .into_iter()
        .partition(|(alias, _)| alias.starts_with('['));
    let mut slashes: Vec<_> = slashes.into_iter().collect();
    slashes.sort_by_key(|(alias, _)| std::cmp::Reverse(alias.len()));
    Catalog {
        brackets,
        slashes,
        max_bytes,
    }
});

pub fn emoji_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while !remaining.is_empty() {
        // Keep URL spellings intact, including bracketed path/query values.
        if ["https://", "http://", "www."]
            .iter()
            .any(|prefix| remaining.starts_with(prefix))
        {
            let end = remaining
                .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '\u{feff}'))
                .unwrap_or(remaining.len());
            output.push_str(&remaining[..end]);
            remaining = &remaining[end..];
            continue;
        }
        let matched = if remaining.starts_with('[') {
            remaining.as_bytes()[..remaining.len().min(CATALOG.max_bytes)]
                .iter()
                .position(|byte| *byte == b']')
                .and_then(|end| {
                    CATALOG
                        .brackets
                        .get(&remaining[..=end])
                        .map(|emoji| (end + 1, emoji))
                })
        } else if remaining.starts_with('/') {
            CATALOG
                .slashes
                .iter()
                .find(|(alias, _)| remaining.starts_with(alias))
                .map(|(alias, emoji)| (alias.len(), emoji))
        } else {
            None
        };
        if let Some((bytes, emoji)) = matched {
            output.push_str(emoji);
            remaining = &remaining[bytes..];
        } else {
            let character = remaining.chars().next().expect("Nonempty text");
            output.push(character);
            remaining = &remaining[character.len_utf8()..];
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_cases_match_the_webview() {
        let cases: Vec<[String; 2]> = serde_json::from_str(include_str!(
            "../../../client-ui/test/fixtures/emoji-text.json"
        ))
        .unwrap();
        for [input, expected] in cases {
            assert_eq!(emoji_text(&input), expected, "{input}");
        }
    }

    #[test]
    fn every_alias_is_consumed_in_full() {
        let entries: HashMap<String, String> = serde_json::from_str(ALIASES).unwrap();
        for (alias, emoji) in entries {
            assert_eq!(emoji_text(&format!("甲{alias}乙")), format!("甲{emoji}乙"));
        }
    }
}
