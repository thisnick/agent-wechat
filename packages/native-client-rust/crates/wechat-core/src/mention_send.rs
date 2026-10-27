use crate::Core;
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;
use wechat_profile::ListQuery;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextMentions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub user_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub all: bool,
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
    let mut rendered = String::new();
    if request.all {
        rendered.push_str("@all\u{2005}");
    } else {
        for id in &request.user_ids {
            let name = names
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("mention_member_unavailable"))?;
            rendered.push('@');
            rendered.push_str(name);
            rendered.push('\u{2005}');
        }
    }
    rendered.push_str(text);
    ensure!(
        !text.is_empty() && !text.contains('\0') && rendered.len() <= 64 * 1024,
        "invalid_mention_text"
    );
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

impl Core {
    pub(crate) fn resolve_mentions(
        &self,
        account: &str,
        room: &str,
        text: &str,
        request: TextMentions,
        own: &str,
    ) -> Result<ResolvedMentions> {
        request.validate()?;
        ensure!(room.ends_with("@chatroom"), "mentions_require_group");
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
                    // Group names/nicknames are shared presentation. A private
                    // contact remark must not be disclosed in outgoing text.
                    let name = member
                        .room_display_name
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .or(member.contact.nickname.as_deref().filter(|s| !s.is_empty()))
                        .filter(|s| {
                            s.len() <= 1024 && !s.chars().any(|c| c.is_control() || c == '\u{2005}')
                        })
                        .unwrap_or(&member.username)
                        .to_owned();
                    names.insert(member.username, name);
                }
            }
            if is_member && names.len() == request.user_ids.len() {
                return render(request, text, &names);
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
    #[test]
    fn metadata_and_visible_tokens_are_separate_and_bounded() {
        let request = TextMentions {
            user_ids: vec!["member_a".into(), "member_b".into()],
            all: false,
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
                all: false
            }
            .validate()
            .is_err());
        }
        assert!(TextMentions {
            user_ids: vec!["member".into()],
            all: true
        }
        .validate()
        .is_err());
    }
}
