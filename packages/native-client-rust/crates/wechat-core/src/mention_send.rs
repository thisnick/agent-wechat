use crate::Core;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;
use wechat_profile::ListQuery;

/// UTF-16 offsets into the submitted text, with an exclusive end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextMentionPosition {
    pub start: u32,
    pub end: u32,
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextMentions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub user_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub all: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<TextMentionPosition>>", optional)]
    pub positions: Vec<TextMentionPosition>,
}
impl TextMentions {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.all == self.user_ids.is_empty(), "invalid_mentions");
        let unique: BTreeSet<_> = self.user_ids.iter().collect();
        ensure!(
            self.user_ids.len() <= 128 && unique.len() == self.user_ids.len(),
            "invalid_mentions"
        );
        for id in &self.user_ids {
            ensure!(
                !id.is_empty()
                    && id.len() <= 256
                    && id != "notify@all"
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-@.".contains(&b)),
                "invalid_mentions"
            );
        }
        ensure!(self.positions.len() <= 256, "invalid_mentions");
        if !self.positions.is_empty() {
            let recipients = if self.all { 1 } else { self.user_ids.len() };
            let mut covered = BTreeSet::new();
            let mut end = 0;
            for position in &self.positions {
                ensure!(
                    position.start >= end
                        && position.start < position.end
                        && (position.index as usize) < recipients,
                    "invalid_mentions"
                );
                end = position.end;
                covered.insert(position.index as usize);
            }
            ensure!(covered.len() == recipients, "invalid_mentions");
        }
        Ok(())
    }
}

/// Frozen submission material in the encrypted outbox, not a second message store.
#[derive(Serialize, Deserialize)]
pub(crate) struct ResolvedMentions {
    pub request: TextMentions,
    pub text: String,
    pub source: String,
}

fn render(
    request: TextMentions,
    text: &str,
    names: &BTreeMap<String, String>,
) -> Result<ResolvedMentions> {
    request.validate()?;
    ensure!(
        !text.is_empty() && !text.contains('\0') && text.len() <= 64 * 1024,
        "invalid_mention_text"
    );
    let mut rendered = String::new();
    let name = |index: usize| -> Result<&str> {
        if request.all {
            Ok("all")
        } else {
            names
                .get(&request.user_ids[index])
                .map(String::as_str)
                .ok_or_else(|| anyhow::anyhow!("mention_member_unavailable"))
        }
    };
    if request.positions.is_empty() {
        for index in 0..if request.all {
            1
        } else {
            request.user_ids.len()
        } {
            rendered.push('@');
            rendered.push_str(name(index)?);
            rendered.push('\u{2005}');
        }
        rendered.push_str(text);
    } else {
        let mut boundaries = vec![None; text.encode_utf16().count() + 1];
        let mut units = 0;
        for (byte, character) in text.char_indices() {
            boundaries[units] = Some(byte);
            units += character.len_utf16();
        }
        boundaries[units] = Some(text.len());
        let mut previous = 0;
        for position in &request.positions {
            let start = boundaries
                .get(position.start as usize)
                .copied()
                .flatten()
                .ok_or_else(|| anyhow::anyhow!("invalid_mentions"))?;
            let end = boundaries
                .get(position.end as usize)
                .copied()
                .flatten()
                .ok_or_else(|| anyhow::anyhow!("invalid_mentions"))?;
            let label = text[start..end]
                .strip_prefix('@')
                .and_then(|span| span.strip_suffix('\u{2005}'))
                .ok_or_else(|| anyhow::anyhow!("invalid_mentions"))?;
            ensure!(safe_label(label), "invalid_mentions");
            rendered.push_str(&text[previous..start]);
            rendered.push('@');
            rendered.push_str(name(position.index as usize)?);
            rendered.push('\u{2005}');
            previous = end;
        }
        rendered.push_str(&text[previous..]);
    }
    ensure!(rendered.len() <= 64 * 1024, "invalid_mention_text");
    let list = if request.all {
        "notify@all".into()
    } else {
        request.user_ids.join(",")
    };
    // IDs are restricted above, so none can introduce XML or list delimiters.
    let source = format!(
        "<msgsource><atuserlist>{list}</atuserlist><alnode><fr>1</fr></alnode></msgsource>"
    );
    Ok(ResolvedMentions {
        request,
        text: rendered,
        source,
    })
}

fn safe_label(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 1024
        && !value.chars().any(|c| c.is_control() || c == '\u{2005}')
}

/// Only shared names may be put on the wire; private contact remarks stay local.
pub(crate) fn mention_name(member: &wechat_profile::GroupMember) -> &str {
    member
        .room_display_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(member.contact.nickname.as_deref().filter(|s| !s.is_empty()))
        .filter(|s| s.len() <= 1024 && !s.chars().any(|c| c.is_control() || c == '\u{2005}'))
        .unwrap_or(&member.username)
}

impl Core {
    pub(crate) fn require_mention_all(&self, account: &str, room: &str, own: &str) -> Result<()> {
        let reader = self
            .profile_reader_owned(account)
            .context("mention_all_permission_unavailable")?;
        let permission = reader
            .group_mention_permissions(room, Some(own))
            .context("mention_all_permission_unavailable")?;
        match permission.can_mention_all {
            Some(true) => Ok(()),
            Some(false) => bail!("mention_all_forbidden"),
            None => bail!("mention_all_permission_unavailable"),
        }
    }

    pub(crate) fn resolve_mentions(
        &self,
        account: &str,
        room: &str,
        text: &str,
        request: TextMentions,
        own: &str,
    ) -> Result<ResolvedMentions> {
        let names = self.mention_names(account, room, &request, own)?;
        render(request, text, &names)
    }

    pub(crate) fn validate_mention_recipients(
        &self,
        account: &str,
        room: &str,
        request: &TextMentions,
        own: &str,
    ) -> Result<()> {
        self.mention_names(account, room, request, own).map(|_| ())
    }

    fn mention_names(
        &self,
        account: &str,
        room: &str,
        request: &TextMentions,
        own: &str,
    ) -> Result<BTreeMap<String, String>> {
        request.validate()?;
        ensure!(room.ends_with("@chatroom"), "mentions_require_group");
        if request.all {
            self.require_mention_all(account, room, own)?;
        }
        let reader = self.profile_reader_owned(account)?;
        let mut cursor = None;
        let mut names = BTreeMap::new();
        let mut is_member = false;
        for _ in 0..500 {
            let page = reader.group_members(
                room,
                &ListQuery {
                    limit: Some(200),
                    cursor,
                    search: None,
                },
            )?;
            for member in page.items {
                is_member |= member.username == own;
                if request.user_ids.contains(&member.username) {
                    let name = mention_name(&member).to_owned();
                    names.insert(member.username, name);
                }
            }
            if is_member && names.len() == request.user_ids.len() {
                return Ok(names);
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        if !is_member {
            bail!("mention_group_membership_unavailable");
        }
        bail!("mention_member_unavailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(positions: &[(u32, u32, u32)]) -> TextMentions {
        TextMentions {
            user_ids: vec!["member_a".into()],
            all: false,
            positions: positions
                .iter()
                .map(|&(start, end, index)| TextMentionPosition { start, end, index })
                .collect(),
        }
    }

    #[test]
    fn inline_mentions_replace_only_selected_utf16_spans_with_shared_names() {
        let names = BTreeMap::from([("member_a".into(), "Alice".into())]);
        let text = "😀 @old\u{2005} + @again\u{2005}!";
        let result = render(inline(&[(3, 8, 0), (11, 18, 0)]), text, &names).unwrap();
        assert_eq!(result.text, "😀 @Alice\u{2005} + @Alice\u{2005}!");
        assert_eq!(result.source.matches("member_a").count(), 1);
        let mut all = inline(&[(7, 15, 0)]);
        all.user_ids.clear();
        all.all = true;
        let result = render(all, "Hello, @people\u{2005}!", &BTreeMap::new()).unwrap();
        assert_eq!(result.text, "Hello, @all\u{2005}!");
        assert!(result.source.contains("notify@all"));

        let mut selected = inline(&[(4, 7, 1), (13, 16, 0)]);
        selected.user_ids.push("member_b".into());
        let names = BTreeMap::from([
            ("member_a".into(), "Alice".into()),
            ("member_b".into(), "Bob".into()),
        ]);
        assert_eq!(
            render(selected, "Ask @b\u{2005} then @a\u{2005}.", &names)
                .unwrap()
                .text,
            "Ask @Bob\u{2005} then @Alice\u{2005}."
        );
        assert_eq!(
            render(inline(&[(0, 4, 0)]), "@😀\u{2005}", &names)
                .unwrap()
                .text,
            "@Alice\u{2005}"
        );

        let no_positions: TextMentions =
            serde_json::from_str(r#"{"userIds":["member_a"]}"#).unwrap();
        assert!(no_positions.positions.is_empty());
        assert_eq!(
            serde_json::to_value(&no_positions).unwrap(),
            serde_json::json!({"userIds":["member_a"]})
        );
        assert_eq!(
            render(no_positions, "hello", &names).unwrap().text,
            "@Alice\u{2005}hello"
        );
    }

    #[test]
    fn malformed_positions_and_labels_are_rejected_before_rendering() {
        let names = BTreeMap::from([("member_a".into(), "Alice".into())]);
        let text = "😀 @old\u{2005} + @again\u{2005}!";
        for positions in [
            vec![(1, 8, 0)],
            vec![(3, 4, 0)],
            vec![(3, 7, 0)],
            vec![(3, 9, 0)],
            vec![(3, 99, 0)],
            vec![(3, 8, 1)],
            vec![(3, 8, 0), (3, 8, 0)],
            vec![(11, 18, 0), (3, 8, 0)],
            vec![(3, 3, 0)],
        ] {
            assert!(render(inline(&positions), text, &names).is_err());
        }
        let mut incomplete = inline(&[(3, 8, 0)]);
        incomplete.user_ids.push("member_b".into());
        assert!(incomplete.validate().is_err());
        let too_many: Vec<_> = (0..257).map(|i| (i * 3, i * 3 + 3, 0)).collect();
        assert!(inline(&too_many).validate().is_err());
        for label in ["", " ", "bad\nname", "bad\u{0085}name", "bad\u{2005}name"] {
            let text = format!("@{label}\u{2005}");
            let end = text.encode_utf16().count() as u32;
            assert!(render(inline(&[(0, end, 0)]), &text, &names).is_err());
        }
        let text = format!("@{}\u{2005}", "a".repeat(1025));
        assert!(render(inline(&[(0, 1027, 0)]), &text, &names).is_err());
    }

    #[test]
    fn metadata_and_visible_tokens_are_separate_and_bounded() {
        let request = TextMentions {
            user_ids: vec!["member_a".into(), "member_b".into()],
            all: false,
            positions: vec![],
        };
        let names = BTreeMap::from([
            ("member_a".into(), "Member & <A>".into()),
            ("member_b".into(), "Member B".into()),
        ]);
        let resolved = render(request.clone(), "hello", &names).unwrap();
        assert_eq!(resolved.text, "@Member & <A>\u{2005}@Member B\u{2005}hello");
        assert_eq!(
            crate::mentions(Some(&resolved.source), Some("member_b"))["self"],
            true
        );
        assert!(!resolved.source.contains("Member"));
        assert!(render(request, &"x".repeat(65536), &names).is_err());
        let all = render(
            TextMentions {
                user_ids: vec![],
                all: true,
                positions: vec![],
            },
            "hello",
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(all.source.contains("<atuserlist>notify@all</atuserlist>"));
        assert_eq!(
            crate::mentions(Some(&all.source), Some("member_b"))["all"],
            true
        );
        for ids in [
            vec![],
            vec!["a,b"],
            vec!["a", "a"],
            vec!["notify@all"],
            vec!["<self>"],
            vec!["a\n"],
        ] {
            assert!(TextMentions {
                user_ids: ids.into_iter().map(str::to_owned).collect(),
                all: false,
                positions: vec![],
            }
            .validate()
            .is_err());
        }
        assert!(TextMentions {
            user_ids: vec!["member".into()],
            all: true,
            positions: vec![],
        }
        .validate()
        .is_err());
    }
}
