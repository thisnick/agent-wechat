//! Contact and device pickers run separately from media polling.
use super::*;
use std::collections::BTreeSet;
use tokio::task::JoinHandle;
use wechat_audio::{AudioDevice, AudioSession};
use wechat_core::call_types::CallKind;

struct Candidate {
    id: String,
    label: String,
}
enum SearchResult {
    Contacts(Vec<Candidate>),
    Devices(Vec<AudioDevice>),
}
pub(super) enum UiAction {
    None,
    Invite(Vec<String>),
    Devices(AudioOptions),
}
pub(super) struct Interaction {
    searching: bool,
    choosing: Vec<Candidate>,
    devices: Vec<AudioDevice>,
    task: Option<JoinHandle<Result<SearchResult>>>,
}
impl Interaction {
    pub(super) fn new() -> Self {
        Self {
            searching: false,
            choosing: vec![],
            devices: vec![],
            task: None,
        }
    }
    pub(super) fn line(
        &mut self,
        line: &str,
        remote: &Remote,
        call: Option<&CallView>,
        options: &AudioOptions,
        levels: &mut levels::CallLevels,
    ) -> Result<UiAction> {
        let line = line.trim();
        if line == "cancel" {
            self.searching = false;
            self.choosing.clear();
            if let Some(task) = self.task.take() {
                task.abort();
            }
            levels.notice("Selection cancelled.");
            return Ok(UiAction::None);
        }
        if line == "d" || line == "devices" {
            if self.task.is_some() {
                levels.notice("A lookup is still running; cancel it first.");
                return Ok(UiAction::None);
            }
            self.task = Some(tokio::spawn(async {
                Ok(SearchResult::Devices(
                    tokio::task::spawn_blocking(AudioSession::devices).await??,
                ))
            }));
            levels.notice("Loading local devices; media continues.");
            return Ok(UiAction::None);
        }
        if let Some(selection) = line.strip_prefix("d ") {
            let mut next = options.clone();
            if selection == "default" {
                return Ok(UiAction::Devices(AudioOptions::default()));
            }
            let mut parts = selection.split_whitespace();
            let kind = parts
                .next()
                .context("use d input NUMBER or d output NUMBER")?;
            let index: usize = parts.next().context("device number required")?.parse()?;
            anyhow::ensure!(
                parts.next().is_none(),
                "use d input NUMBER or d output NUMBER"
            );
            let device = self
                .devices
                .get(
                    index
                        .checked_sub(1)
                        .context("device number starts at one")?,
                )
                .context("select a device shown by d first")?;
            anyhow::ensure!(
                (kind == "input" && device.input) || (kind == "output" && !device.input),
                "device kind does not match"
            );
            if device.input {
                next.input_device = device.id.clone();
            } else {
                next.output_device = device.id.clone();
            }
            return Ok(UiAction::Devices(next));
        }
        if line == "i"
            || line == "invite"
            || line.starts_with("i ")
            || line.starts_with("invite ")
            || self.searching
            || !self.choosing.is_empty()
        {
            anyhow::ensure!(
                call.is_some_and(|call| call.kind == CallKind::Conference),
                "inviting is conference-only; direct-call promotion is unavailable"
            );
            if let Some(ids) = line.strip_prefix("invite ") {
                let ids = ids
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                anyhow::ensure!(
                    !ids.is_empty() && ids.len() <= 32,
                    "choose between one and 32 contact IDs"
                );
                self.searching = false;
                self.choosing.clear();
                return Ok(UiAction::Invite(ids));
            }
            if !self.choosing.is_empty()
                && line != "i"
                && line != "invite"
                && !line.starts_with("i ")
            {
                let mut chosen = BTreeSet::new();
                for number in line.split([',', ' ']).filter(|s| !s.is_empty()) {
                    let index: usize = number
                        .parse()
                        .context("choose contact numbers, or cancel")?;
                    chosen.insert(
                        self.choosing
                            .get(
                                index
                                    .checked_sub(1)
                                    .context("contact number starts at one")?,
                            )
                            .context("contact number is not in this result")?
                            .id
                            .clone(),
                    );
                }
                anyhow::ensure!(!chosen.is_empty(), "choose contact numbers, or cancel");
                self.choosing.clear();
                return Ok(UiAction::Invite(chosen.into_iter().collect()));
            }
            if line == "i" || line == "invite" {
                self.searching = true;
                self.choosing.clear();
                levels.notice("Search contacts by name, or enter invite CONTACT_ID [...]. Type cancel to return.");
                return Ok(UiAction::None);
            }
            if self.task.is_some() {
                levels.notice("Contact search is still running; media continues.");
                return Ok(UiAction::None);
            }
            let query = line.strip_prefix("i ").unwrap_or(line).to_owned();
            let remote = Remote {
                http: remote.http.clone(),
                base: remote.base.clone(),
                token: remote.token.clone(),
                account: remote.account.clone(),
            };
            let existing = call
                .into_iter()
                .flat_map(|call| call.participants.iter())
                .filter(|p| p.membership == wechat_core::call_types::CallMembership::Joined)
                .map(|p| p.contact_id.clone())
                .collect::<BTreeSet<_>>();
            self.searching = false;
            self.task = Some(tokio::spawn(async move {
                let path = super::super::query(remote.path("contacts")?, &[("query", Some(query))]);
                let value = timeout(Duration::from_secs(5), remote.get(&path))
                    .await
                    .context("contact search timed out")??;
                let items = value["items"]
                    .as_array()
                    .context("contact search response has no items")?;
                let mut candidates = vec![];
                for item in items.iter().take(20) {
                    let Some(id) = item["username"].as_str().or_else(|| item["id"].as_str()) else {
                        continue;
                    };
                    if existing.contains(id) || id.ends_with("@chatroom") {
                        continue;
                    }
                    let label = ["remark", "displayName", "nickname"]
                        .into_iter()
                        .find_map(|key| item[key].as_str())
                        .unwrap_or(id);
                    candidates.push(Candidate {
                        id: id.into(),
                        label: levels::clean_label(label, 64),
                    });
                }
                Ok(SearchResult::Contacts(candidates))
            }));
            levels.notice("Searching saved contacts; media continues.");
            return Ok(UiAction::None);
        }
        levels.notice("Unknown key. Type ? then Enter for controls.");
        Ok(UiAction::None)
    }
    pub(super) async fn completed(&mut self, levels: &mut levels::CallLevels) {
        let Some(task) = self.task.as_mut() else {
            std::future::pending::<()>().await;
            return;
        };
        let result = task.await;
        self.task = None;
        match result {
            Ok(Ok(SearchResult::Contacts(items))) => {
                let mut text =
                    String::from("Choose contact numbers separated by spaces, or cancel:\n");
                for (i, item) in items.iter().enumerate() {
                    text.push_str(&format!(
                        "{}: {} ({})\n",
                        i + 1,
                        item.label,
                        levels::clean_label(&item.id, 64)
                    ));
                }
                if items.is_empty() {
                    text =
                        "No saved contacts matched. Use i to search again, or invite CONTACT_ID."
                            .into();
                }
                self.choosing = items;
                levels.notice(text.trim_end());
            }
            Ok(Ok(SearchResult::Devices(items))) => {
                let mut text=String::from("Local devices (switching prepares the new devices before closing the current ones):\n");
                for (i, item) in items.iter().enumerate() {
                    text.push_str(&format!(
                        "{}: {} {}{}\n",
                        i + 1,
                        if item.input { "input" } else { "output" },
                        levels::clean_label(&item.name, 64),
                        if item.is_default { " (default)" } else { "" }
                    ));
                }
                text.push_str("Use d input NUMBER, d output NUMBER, or d default. A switch may briefly interrupt local audio.");
                self.devices = items;
                levels.notice(&text);
            }
            Ok(Err(error)) => levels.notice(&format!("Lookup failed: {error}")),
            Err(_) => levels.notice("Lookup worker stopped; the call is still active."),
        }
    }
}
impl Drop for Interaction {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[tokio::test]
    async fn delayed_contact_picker_preserves_media_and_filters_current_members() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let seen = entered.clone();
        let ready = release.clone();
        let router=axum::Router::new().route("/v1/accounts/test-account/contacts",axum::routing::get(move || {let seen=seen.clone();let ready=ready.clone();async move {seen.notify_one();ready.notified().await;axum::Json(serde_json::json!({"items":[{"username":"already-here","displayName":"Already here"},{"username":"new-contact","displayName":"New contact"}]}))}}));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut ui = Interaction::new();
        let mut levels = levels::CallLevels::for_test();
        let call = CallView {
            call_id: "fixture".into(),
            revision: 1,
            kind: CallKind::Conference,
            call_type: wechat_core::call_types::CallType::Voice,
            direction: wechat_core::call_types::CallDirection::Outgoing,
            state: wechat_core::call_types::CallState::Connected,
            owned_by_this_client: true,
            group_id: Some("group".into()),
            controls: vec![],
            participants: vec![wechat_core::call_types::CallParticipant {
                participant_id: "person".into(),
                contact_id: "already-here".into(),
                membership: wechat_core::call_types::CallMembership::Joined,
                microphone_enabled: Some(true),
            }],
        };
        let remote = super::super::tests::remote(address);
        ui.line(
            "i contact",
            &remote,
            Some(&call),
            &AudioOptions::default(),
            &mut levels,
        )
        .unwrap();
        timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        levels.roster(&call);
        levels.participant_audio(7, Some("person"), &[12000; 320]);
        levels.refresh();
        assert!(levels.participant_level("person") > -12.0);
        assert!(!ui.task.as_ref().unwrap().is_finished());
        release.notify_one();
        timeout(Duration::from_secs(2), ui.completed(&mut levels))
            .await
            .unwrap();
        let UiAction::Invite(ids) = ui
            .line(
                "1",
                &remote,
                Some(&call),
                &AudioOptions::default(),
                &mut levels,
            )
            .unwrap()
        else {
            panic!("selection did not produce an invitation")
        };
        assert_eq!(ids, vec!["new-contact"]);
        server.abort();
    }
}
