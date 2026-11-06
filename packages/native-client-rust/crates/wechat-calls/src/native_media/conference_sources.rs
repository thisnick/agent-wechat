//! Bind decoder state to authenticated conference membership, retaining replay
//! history across temporary departures and quarantining recycled member IDs.
use crate::{conference_media::ConferenceReceiver as Receiver, session::ConferenceMember};
use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;
use wechat_protocol::calls::{audio_packet::MediaEpoch, relay::SelectedRelay};

pub struct BoundConferenceReceiver {
    pub identity: String,
    pub active: bool,
    pub microphone_enabled: Option<bool>,
    pub receiver: Receiver,
    pub generation: u32,
}
pub fn reconcile_roster(
    selected: &SelectedRelay,
    epoch: MediaEpoch,
    members: &[ConferenceMember],
    receivers: &mut BTreeMap<u16, BoundConferenceReceiver>,
    unattributed: &mut BTreeMap<u16, Receiver>,
) -> Result<()> {
    for (id, bound) in receivers.iter_mut() {
        if bound.active
            && !members.iter().any(|member| {
                member.member_id == *id && member.status == 1 && member.username == bound.identity
            })
        {
            bound.active = false;
            bound.receiver.report.left_events += 1;
        }
    }
    for member in members
        .iter()
        .filter(|member| member.status == 1 && member.member_id != selected.own_member)
    {
        if let Some(bound) = receivers.get_mut(&member.member_id) {
            bound.microphone_enabled = member.microphone_enabled;
            if bound.identity != member.username {
                bound.active = false;
                bound.receiver.report.identity_quarantined = true;
            } else if !bound.active && !bound.receiver.report.identity_quarantined {
                bound.generation = bound
                    .generation
                    .checked_add(1)
                    .context("Conference track generation exhausted")?;
                bound.receiver.reset_decoder()?;
                bound.active = true;
                bound.receiver.report.joined_events += 1;
            }
        } else {
            ensure!(
                receivers.len() < 128,
                "Conference receiver history limit reached"
            );
            let mut receiver = match unattributed.remove(&member.member_id) {
                Some(receiver) => receiver,
                None => Receiver::new(selected, member.member_id, epoch)?,
            };
            receiver.report.joined_events += 1;
            receivers.insert(
                member.member_id,
                BoundConferenceReceiver {
                    generation: 1,
                    identity: member.username.clone(),
                    active: true,
                    microphone_enabled: member.microphone_enabled,
                    receiver,
                },
            );
        }
    }
    Ok(())
}
