//! Source-selected personal voice acceptance. The embedding owner authenticates
//! and retains the live carrier and grants the broker winner before `accept`.

use super::*;
use crate::calls::incoming::{CancelNotification, InviteNotification};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingPhase {
    Invited,
    Acknowledged,
    Joining,
    Joined,
    Ending,
    TerminalPending,
    Uncertain,
    Finished,
}
struct Invite {
    room: VoiceRoom,
    caller: String,
    caller_openid: Vec<u8>,
    callee_openid: Vec<u8>,
    caller_member: Option<u16>,
}

/// One incoming direct voice call. ACK is ringing setup, not acceptance. Only
/// the embedding broker's winning owner may request Join. These methods do not
/// verify carrier freshness, consume account sync, retry or create history.
pub struct NativeIncomingSession {
    base: NativePersonalSession,
    invite: Invite,
    phase: IncomingPhase,
    ack_spent: bool,
    join_spent: bool,
    reject_spent: bool,
}
impl NativeIncomingSession {
    /// Call only after a current account-owned live notification has been
    /// retained and a listening controller has reserved the global call slot.
    #[allow(clippy::too_many_arguments)]
    pub fn authenticate(
        client: &impl PersonalTransport,
        state: &mut AccountState,
        authority: AppAuthority,
        host_task: u32,
        device32: [u8; 32],
        mut parameters: VoiceParameters,
        notification: &InviteNotification,
        expected_caller: &str,
    ) -> Result<Self> {
        ensure!(
            notification.unknown_fields.is_empty(),
            "Unknown incoming invitation fields"
        );
        ensure!(
            notification.room_type == Some(2)
                && notification.app_id.as_deref() == Some("wechat")
                && notification.sub_app_id.as_deref() == Some("voip-2p"),
            "Unsupported incoming call family"
        );
        ensure!(
            notification.callee_username.as_deref() == Some(state.own_username.as_str())
                && notification.caller_username.as_deref() == Some(expected_caller)
                && expected_caller != state.own_username
                && !expected_caller.is_empty()
                && expected_caller.len() <= 128,
            "Incoming account or caller mismatch"
        );
        ensure!(
            notification.usernames.len() <= 2
                && notification
                    .usernames
                    .iter()
                    .all(|name| name == expected_caller || name == &state.own_username),
            "Unsupported incoming participants"
        );
        let room = VoiceRoom {
            room: notification
                .room_id
                .ok_or_else(|| anyhow::anyhow!("Missing incoming room"))?,
            room_key: notification
                .room_key
                .ok_or_else(|| anyhow::anyhow!("Missing incoming room key"))?,
            own_member: notification
                .member_id
                .ok_or_else(|| anyhow::anyhow!("Missing incoming member"))?,
        };
        ensure!(
            room.room != 0 && room.room_key != 0 && u16::try_from(room.own_member).is_ok(),
            "Invalid incoming room identity"
        );
        let caller_openid = notification
            .caller_openid
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing incoming caller identity"))?
            .as_bytes()
            .to_vec();
        let callee_openid = notification
            .callee_openid
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing incoming recipient identity"))?
            .as_bytes()
            .to_vec();
        ensure!(
            !caller_openid.is_empty()
                && caller_openid.len() <= 128
                && !callee_openid.is_empty()
                && callee_openid.len() <= 128
                && caller_openid != callee_openid,
            "Invalid incoming app identities"
        );
        let owner = notification.wechat_group_id.as_deref().unwrap_or("");
        ensure!(owner.len() <= 256, "Incoming owner group too large");
        let group = fields(&parameters.group)?;
        ensure!(
            number(&group, 5, None)? == 1
                && number(&group, 2, None)? == 0
                && data(&group, 4)?.is_empty(),
            "Incoming session requires fresh selected group mode"
        );
        parameters.group = bind_owner_group(&parameters.group, owner.as_bytes())?;
        let caller_member = notification
            .caller_member_id
            .map(u16::try_from)
            .transpose()?;
        if let Some(member) = caller_member {
            ensure!(
                u32::from(member) != room.own_member,
                "Incoming member collision"
            );
        }
        let mut base = NativePersonalSession::authenticate(
            client, state, authority, host_task, device32, parameters,
        )?;
        base.control = Some(PersonalControl {
            identity: room,
            peer: expected_caller.to_owned(),
            origin: ControlOrigin::Incoming,
            peer_joined: None,
            terminal: None,
            invitation_pending: false,
            pending_recipients: vec![],
        });
        base.phase = PersonalPhase::Ringing;
        Ok(Self {
            base,
            invite: Invite {
                room,
                caller: expected_caller.to_owned(),
                caller_openid,
                callee_openid,
                caller_member,
            },
            phase: IncomingPhase::Invited,
            ack_spent: false,
            join_spent: false,
            reject_spent: false,
        })
    }
    pub fn phase(&self) -> IncomingPhase {
        self.phase
    }
    /// Latest authenticated local room/member binding. This is private call
    /// routing data and must not be exposed as a public event or diagnostic.
    pub fn room(&self) -> VoiceRoom {
        self.invite.room
    }
    /// Perform singleton ringing ACK8069. A successful ACK may be offered to
    /// controllers, but it grants neither a media stream nor permission to Join.
    pub fn ringing_ack(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<()> {
        self.base.check(state, current)?;
        ensure!(
            self.phase == IncomingPhase::Invited && !self.ack_spent,
            "Incoming ACK already spent"
        );
        self.base.reserve(tasks)?;
        self.ack_spent = true;
        self.phase = IncomingPhase::Uncertain;
        let result = self.ringing_ack_once(client, state, tasks);
        if result
            .as_ref()
            .is_err_and(|error| error.is::<CallRpcNotSubmitted>())
        {
            // Preserve the source phase for a separately authenticated Cancel;
            // the spent action is not retried with a new task.
            self.phase = IncomingPhase::Invited;
        }
        result
    }
    fn ringing_ack_once(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        tasks: TaskIds,
    ) -> Result<()> {
        let request = personal::acknowledge_voice(
            &self.base.parameters.inputs(),
            self.invite.room,
            tasks.sdk,
        )?;
        let body = self.base.host(
            client,
            state,
            CallHostRoute::RingingAck,
            request.body(),
            tasks.host,
        )?;
        let outer = fields(&body)?;
        only(&outer, &[1, 2])?;
        ensure!(
            p::optional_data(&outer, 2)?.is_none_or(|auth| auth.is_empty()),
            "Unexpected incoming auth rotation"
        );
        let ack = fields(data(&outer, 1)?)?;
        only(&ack, &[1, 2, 3, 4, 5, 6, 7, 8, 10])?;
        ensure!(
            number(&ack, 1, None)? == self.invite.room.room
                && number(&ack, 2, None)? == self.invite.room.room_key,
            "Incoming ACK room mismatch"
        );
        let member = u16::try_from(number(&ack, 3, None)?)?;
        let live_id = number(&ack, 4, Some(0))?;
        for tag in [5, 6, 7, 8, 10] {
            p::optional_data(&ack, tag)?;
        }
        self.invite.room.own_member = u32::from(member);
        self.base
            .control
            .as_mut()
            .expect("incoming control")
            .identity = self.invite.room;
        let group = fields(&self.base.parameters.group)?;
        let cloud_group = std::str::from_utf8(data(&group, 4)?)?;
        self.base.parameters.group =
            replace_group(&self.base.parameters.group, live_id, cloud_group)?;
        // ACK6 is MemList(seq1/member2); it is not LiveSync item10(seq3/member4).
        let (sequence, members) = if let Some(raw) = p::optional_data(&ack, 6)? {
            let list = fields(raw)?;
            only(&list, &[1, 2, 3])?;
            p::optional_data(&list, 3)?;
            let sequence = u32::try_from(number(&list, 1, Some(0))?)?;
            let members = if sequence == 0 {
                BTreeMap::new()
            } else {
                parse_roster_entries(
                    repeated(&list, 2, 16)?,
                    &self.base.own_username,
                    &self.invite.caller,
                )?
            };
            (sequence, members)
        } else {
            (0, BTreeMap::new())
        };
        for (name, entry) in &members {
            let expected = if name == &self.base.own_username {
                &self.invite.callee_openid
            } else {
                &self.invite.caller_openid
            };
            ensure!(
                &entry.openid == expected,
                "Incoming ACK app identity mismatch"
            );
            if name == &self.base.own_username {
                ensure!(entry.member == member, "Incoming ACK own member mismatch");
            } else if let Some(expected) = self.invite.caller_member {
                ensure!(
                    entry.member == expected,
                    "Incoming ACK caller member mismatch"
                );
            }
        }
        if sequence != 0 {
            if let Some(action) = termination::classify(
                termination::Origin::Callee,
                3,
                members
                    .get(&self.base.own_username)
                    .map(|entry| entry.status),
                members.get(&self.invite.caller).map(|entry| entry.status),
                false,
            )? {
                ensure!(
                    matches!(action, RemoteTermination::LocalStop(_)),
                    "Unsupported incoming ACK terminal operation"
                );
                self.base
                    .control
                    .as_mut()
                    .expect("incoming control")
                    .terminal = Some(action);
                self.base.phase = PersonalPhase::Finished;
                self.phase = IncomingPhase::Finished;
                return Ok(());
            }
        }
        if self.invite.caller_member.is_none() {
            self.invite.caller_member = members.get(&self.invite.caller).map(|entry| entry.member);
        }
        ensure!(
            self.invite
                .caller_member
                .is_none_or(|caller| caller != member)
                && live_id != 0,
            "Incomplete incoming ACK control context"
        );
        let strategy = p::optional_data(&ack, 7)?.unwrap_or_default();
        ensure!(
            number(&fields(strategy)?, 14, Some(0))? & (1 << 36) == 0,
            "Alternate incoming sync unsupported"
        );
        // Original entries below bind identities only. They do not fabricate an
        // accepted roster when the initial ACK sequence is absent/stale.
        let mut original_members = BTreeMap::from([(
            self.base.own_username.clone(),
            Member {
                member,
                status: 0,
                openid: self.invite.callee_openid.clone(),
            },
        )]);
        if let Some(caller_member) = self.invite.caller_member {
            original_members.insert(
                self.invite.caller.clone(),
                Member {
                    member: caller_member,
                    status: 0,
                    openid: self.invite.caller_openid.clone(),
                },
            );
        }
        self.base.active = Some(ActiveRoom {
            identity: self.invite.room,
            peer: self.invite.caller.clone(),
            live_id,
            cookies: p::optional_data(&ack, 5)?.unwrap_or_default().to_vec(),
            strategy: Zeroizing::new(strategy.to_vec()),
            members,
            original_members,
            unbound_peer_openid: self
                .invite
                .caller_member
                .is_none()
                .then(|| self.invite.caller_openid.clone()),
            member_sequence: sequence,
            cap_sequence: 0,
            engine: vec![],
            channel: vec![],
            av_sequence: 0,
            av: BTreeMap::new(),
            media_taken: false,
        });
        self.base.phase = PersonalPhase::Ringing;
        self.phase = IncomingPhase::Acknowledged;
        Ok(())
    }
    /// The broker winner accepts with actual callee Join3361. Timestamp8 is the
    /// source-selected literal0; this does not reuse a caller Invite response.
    pub fn accept(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<()> {
        self.base.check(state, current)?;
        ensure!(
            self.phase == IncomingPhase::Acknowledged && !self.join_spent,
            "Incoming call not acknowledged or Join spent"
        );
        self.base.reserve(tasks)?;
        self.join_spent = true;
        self.phase = IncomingPhase::Joining;
        let result = self.accept_once(client, state, current, tasks);
        if let Err(error) = &result {
            if error.is::<CallRpcNotSubmitted>() {
                self.phase = IncomingPhase::Acknowledged;
                self.base.phase = PersonalPhase::Ringing;
            } else if !error.is::<UnsupportedCallMedia>() {
                self.phase = IncomingPhase::Uncertain;
                self.base.phase = PersonalPhase::Uncertain;
            }
        }
        result
    }
    fn accept_once(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<()> {
        let request = personal::join(
            &self.base.parameters.inputs(),
            self.invite.room,
            0,
            tasks.sdk,
        )?;
        let raw = self.transfer(
            client,
            state,
            current,
            CallOperation::Join,
            request.body.as_deref().expect("prepared Join"),
            tasks.host,
        )?;
        let joined = fields(&raw)?;
        only(
            &joined,
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 18],
        )?;
        ensure!(
            number(&joined, 1, None)? == self.invite.room.room
                && number(&joined, 2, None)? == self.invite.room.room_key,
            "Incoming Join room mismatch"
        );
        ensure!(
            matches!(number(&joined, 8, Some(2))?, 2) && number(&joined, 18, Some(0))? == 0,
            "Unsupported incoming Join mode"
        );
        // Tags5/6 are opaque compatibility fields in this selected message.
        if let Some(value) = p::optional(&joined, 5)? {
            ensure!(
                matches!(value, Value::Varint(_)),
                "Invalid incoming opaque field"
            );
        }
        repeated(&joined, 6, 16)?;
        let own_member = u16::try_from(number(&joined, 3, None)?)?;
        let room = VoiceRoom {
            room: self.invite.room.room,
            room_key: self.invite.room.room_key,
            own_member: u32::from(own_member),
        };
        let raw_members = data(&joined, 13)?;
        let members = roster(
            raw_members,
            &self.base.own_username,
            &self.invite.caller,
            own_member,
        )?;
        ensure!(
            members[&self.invite.caller].status == 1
                && members[&self.invite.caller].openid == self.invite.caller_openid
                && members[&self.base.own_username].openid == self.invite.callee_openid
                && self
                    .invite
                    .caller_member
                    .is_none_or(|member| member == members[&self.invite.caller].member),
            "Incoming Join participant mismatch"
        );
        let control = self.base.control.as_mut().expect("incoming control");
        control.identity = room;
        control.peer_joined = Some(true);
        self.invite.room = room;
        self.base.phase = PersonalPhase::Joined;
        self.phase = IncomingPhase::Joined;
        let strategy = data(&joined, 4)?;
        let parsed_strategy = fields(strategy)?;
        ensure!(
            number(&parsed_strategy, 14, None)? & (1 << 36) == 0,
            "Alternate incoming sync unsupported"
        );
        let live_id = number(&joined, 11, None)?;
        ensure!(live_id != 0, "Missing incoming live identity");
        let old_group = fields(&self.base.parameters.group)?;
        let cloud_group = std::str::from_utf8(data(&old_group, 4)?)?;
        self.base.parameters.group =
            replace_group(&self.base.parameters.group, live_id, cloud_group)?;
        let (cap_sequence, engine, channel) = cap(data(&parsed_strategy, 13)?)?;
        let (av_sequence, av) = av(
            data(&joined, 9)?,
            own_member,
            members[&self.invite.caller].member,
        )?;
        let member_sequence = u32::try_from(number(&fields(raw_members)?, 1, Some(0))?)?;
        if let Some(acked) = self.base.active.as_ref() {
            ensure!(
                member_sequence >= acked.member_sequence,
                "Incoming Join roster sequence regressed"
            );
            if member_sequence == acked.member_sequence && member_sequence != 0 {
                ensure!(
                    members
                        .iter()
                        .all(|(name, entry)| acked
                            .members
                            .get(name)
                            .is_some_and(|old| old.member == entry.member
                                && old.openid == entry.openid
                                && old.status == entry.status)),
                    "Incoming Join changed a stale roster"
                );
            }
        }

        self.base.active = Some(ActiveRoom {
            identity: room,
            peer: self.invite.caller.clone(),
            live_id,
            cookies: data(&joined, 12)?.to_vec(),
            strategy: Zeroizing::new(strategy.to_vec()),
            original_members: members.clone(),
            unbound_peer_openid: None,
            members,
            member_sequence,
            cap_sequence,
            engine,
            channel,
            av_sequence,
            av,
            media_taken: false,
        });
        let _ = self.base.media_ready(state, current)?;
        Ok(())
    }
    fn transfer(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        operation: CallOperation,
        inner: &[u8],
        host_task: u32,
    ) -> Result<Vec<u8>> {
        let parameters = AppFrameParameters {
            sdk_version: self.base.parameters.sdk_version()?,
            cloud_device_id: &self.base.parameters.cloud_device_id,
            host_client_version: state.device.client_version as u32,
            app_config_version: self.base.parameters.app_config_version,
        };
        let body =
            app_frame::prepare_transfer(&self.base.app, current, operation, inner, &parameters)?;
        let reply = self
            .base
            .host(client, state, CallHostRoute::Transfer, &body, host_task)?;
        app_frame::decode_transfer_reply(&self.base.app, current, operation, &reply)
    }
    /// Decline only the source ACKed-callee state. Pre-ACK busy/other-device
    /// paths have separate operations and are not substituted here.
    pub fn reject(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<()> {
        self.base.check(state, current)?;
        ensure!(
            self.phase == IncomingPhase::Acknowledged && !self.reject_spent,
            "Incoming rejection unavailable"
        );
        self.base.reserve(tasks)?;
        self.reject_spent = true;
        self.phase = IncomingPhase::Ending;
        let result: Result<()> = (|| {
            let request =
                personal::reject(&self.base.parameters.inputs(), self.invite.room, tasks.sdk)?;
            let raw = self.transfer(
                client,
                state,
                current,
                CallOperation::Reject,
                request.body.as_deref().expect("prepared Reject"),
                tasks.host,
            )?;
            let reply = fields(&raw)?;
            only(&reply, &[1, 2, 3])?;
            if let Some(members) = p::optional_data(&reply, 3)? {
                fields(members)?;
            }
            ensure!(
                number(&reply, 1, None)? == self.invite.room.room
                    && number(&reply, 2, None)? == self.invite.room.room_key,
                "Incoming Reject room mismatch"
            );
            Ok(())
        })();
        let unsent = result
            .as_ref()
            .is_err_and(|error| error.is::<CallRpcNotSubmitted>());
        self.phase = if result.is_ok() {
            IncomingPhase::Finished
        } else if unsent {
            IncomingPhase::Acknowledged
        } else {
            IncomingPhase::Uncertain
        };
        self.base.phase = if result.is_ok() {
            PersonalPhase::Finished
        } else if unsent {
            PersonalPhase::Ringing
        } else {
            PersonalPhase::Uncertain
        };
        result
    }
    pub fn poll(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<SyncProgress> {
        ensure!(
            matches!(
                self.phase,
                IncomingPhase::Acknowledged | IncomingPhase::Joined
            ),
            "Incoming sync unavailable"
        );
        let result = self.base.poll(client, state, current, tasks);
        if result.is_ok() && self.invite.caller_member.is_none() {
            self.invite.caller_member = self
                .base
                .active
                .as_ref()
                .and_then(|active| active.original_members.get(&self.invite.caller))
                .map(|member| member.member);
        }
        if self.base.phase == PersonalPhase::Uncertain {
            self.phase = IncomingPhase::Uncertain;
        }
        if let Ok(SyncProgress::Terminal(action)) = result.as_ref() {
            self.phase = if matches!(action, RemoteTermination::LocalStop(_)) {
                IncomingPhase::Finished
            } else {
                IncomingPhase::TerminalPending
            };
        }
        result
    }
    /// Source-classified remote cleanup, separate from local user hangup.
    pub fn finish_remote(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
        talk_success: bool,
    ) -> Result<()> {
        ensure!(
            self.phase == IncomingPhase::TerminalPending,
            "No incoming remote teardown"
        );
        self.phase = IncomingPhase::Ending;
        let result = self
            .base
            .finish_remote(client, state, current, tasks, talk_success);
        self.phase = if result.is_ok() {
            IncomingPhase::Finished
        } else {
            IncomingPhase::Uncertain
        };
        result
    }
    pub fn media_ready(&self, state: &AccountState, current: AppAuthority) -> Result<bool> {
        ensure!(
            self.phase == IncomingPhase::Joined,
            "Incoming media unavailable"
        );
        self.base.media_ready(state, current)
    }
    pub fn take_media(
        &mut self,
        state: &AccountState,
        current: AppAuthority,
        connection_index: usize,
        udp_index: usize,
    ) -> Result<NegotiatedMedia> {
        ensure!(
            self.phase == IncomingPhase::Joined,
            "Incoming media unavailable"
        );
        self.base
            .take_media(state, current, connection_index, udp_index)
    }
    pub fn exit(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
        talk_success: bool,
    ) -> Result<()> {
        ensure!(
            self.phase == IncomingPhase::Joined,
            "Incoming exit unavailable"
        );
        self.phase = IncomingPhase::Ending;
        let result = self.base.exit(client, state, current, tasks, talk_success);
        self.phase = if result.is_ok() {
            IncomingPhase::Finished
        } else {
            IncomingPhase::Uncertain
        };
        result
    }
    /// Only a separately authenticated, matching cancellation while invited or
    /// ACKed retires this callee. Joined calls need their own terminal evidence.
    pub fn retire_cancel(
        &mut self,
        state: &AccountState,
        current: AppAuthority,
        cancel: &CancelNotification,
    ) -> Result<bool> {
        self.base.check(state, current)?;
        if !matches!(
            self.phase,
            IncomingPhase::Invited | IncomingPhase::Acknowledged
        ) {
            return Ok(false);
        }
        ensure!(
            cancel.unknown_fields.is_empty(),
            "Unknown incoming cancellation fields"
        );
        if cancel.room_id != Some(self.invite.room.room)
            || cancel.room_key != Some(self.invite.room.room_key)
            || cancel.member_id != Some(self.invite.room.own_member)
        {
            return Ok(false);
        }
        self.phase = IncomingPhase::Finished;
        self.base.phase = PersonalPhase::Finished;
        Ok(true)
    }
}
fn bind_owner_group(raw: &[u8], owner: &[u8]) -> Result<Vec<u8>> {
    let values = fields(raw)?;
    let old = p::optional_data(&values, 6)?.unwrap_or_default();
    ensure!(
        old.is_empty() || old == owner,
        "Incoming owner-group mismatch"
    );
    let mut encoded = Vec::new();
    for field in values {
        if field.number == 6 {
            continue;
        }
        encoded.push(match field.value {
            Value::Varint(value) => p::uint(field.number, value),
            Value::Bytes(value) => p::bytes(field.number, &value),
            _ => bail!("Unsupported incoming group wire type"),
        });
    }
    encoded.push(p::bytes(6, owner));
    p::message(&encoded)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        auth_reply, authority, av_status, capabilities_for_codec, exit_reply, host_reply, member,
        parameters, state, FakeTransport,
    };
    use super::*;
    fn notification() -> InviteNotification {
        InviteNotification::decode(
            &p::message(&[
                p::uint(1, 7),
                p::uint(2, 9),
                p::uint(3, 0),
                p::uint(4, 2),
                p::bytes(5, b"peer"),
                p::bytes(6, b"own"),
                p::bytes(7, b"peer"),
                p::bytes(8, b"own"),
                p::bytes(9, b"own"),
                p::bytes(9, b"peer"),
                p::bytes(10, b"wechat"),
                p::bytes(11, b"voip-2p"),
                p::uint(12, 1),
                p::bytes(13, b"owner-group"),
            ])
            .unwrap(),
        )
        .unwrap()
    }
    fn ack_reply() -> Vec<u8> {
        host_reply(&p::bytes(
            1,
            &p::message(&[
                p::uint(1, 7),
                p::uint(2, 9),
                p::uint(3, 0),
                p::uint(4, 99),
                p::bytes(5, b"ack-cursor"),
                p::bytes(
                    6,
                    &p::message(&[
                        p::uint(1, 1),
                        p::bytes(2, &member("own", 0, 3)),
                        p::bytes(2, &member("peer", 1, 1)),
                    ])
                    .unwrap(),
                ),
                p::bytes(7, &p::uint(14, 0)),
            ])
            .unwrap(),
        ))
    }
    fn wrapped_reply(command: u32, body: &[u8]) -> Vec<u8> {
        let frame = app_frame::TaskFrame {
            command,
            integers: BTreeMap::from([
                (4, 0),
                (7, 0),
                (8, 0),
                (21, 0),
                (22, 17),
                (23, 0),
                (25, 0),
                (26, 0),
            ]),
            strings: BTreeMap::new(),
            payload: p::message(&[p::bytes(1, &[8, 0]), p::uint(2, 17), p::bytes(3, body)])
                .unwrap(),
        }
        .encode()
        .unwrap();
        host_reply(&p::message(&[p::uint(1, 0), p::bytes(2, &frame)]).unwrap())
    }
    fn join_reply(codec: u64, forged_openid: bool) -> Vec<u8> {
        let (engine, channel) = capabilities_for_codec(codec);
        let cap = p::message(&[
            p::uint(1, 1),
            p::bytes(
                2,
                &p::message(&[p::bytes(1, &engine), p::bytes(2, &channel)]).unwrap(),
            ),
        ])
        .unwrap();
        let address = p::message(&[p::bytes(1, b"127.0.0.1"), p::uint(2, 6000)]).unwrap();
        let strategy = p::message(&[
            p::bytes(10, &[7; 32]),
            p::bytes(13, &cap),
            p::uint(14, 0),
            p::bytes(
                15,
                &p::message(&[p::uint(1, 1), p::bytes(4, &address)]).unwrap(),
            ),
        ])
        .unwrap();
        let mut peer = member("peer", 1, 1);
        if forged_openid {
            let mut values = fields(&peer).unwrap();
            values.retain(|field| field.number != 1);
            peer = p::message(
                &values
                    .into_iter()
                    .map(|field| match field.value {
                        Value::Varint(value) => p::uint(field.number, value),
                        Value::Bytes(value) => p::bytes(field.number, &value),
                        _ => unreachable!(),
                    })
                    .chain([p::bytes(1, b"wrong")])
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        }
        let members = p::message(&[
            p::uint(1, 2),
            p::bytes(2, &member("own", 0, 1)),
            p::bytes(2, &peer),
        ])
        .unwrap();
        // This is actual callee JoinRsp: Strategy4, AV9, live11/cookies12,
        // MemList13. Caller JoinClientRsp uses different numbered boundaries.
        wrapped_reply(
            3361,
            &p::message(&[
                p::uint(1, 7),
                p::uint(2, 9),
                p::uint(3, 0),
                p::bytes(4, &strategy),
                p::bytes(6, b"opaque-one"),
                p::bytes(6, b"opaque-two"),
                p::uint(5, 0),
                p::bytes(6, &[]),
                p::uint(8, 2),
                p::bytes(9, &av_status(1, 1)),
                p::uint(11, 99),
                p::bytes(12, b"joined-cursor"),
                p::bytes(13, &members),
            ])
            .unwrap(),
        )
    }
    fn transport(replies: Vec<(CallHostRoute, Vec<u8>)>) -> FakeTransport {
        FakeTransport(std::cell::RefCell::new(replies.into()))
    }
    fn session(transport: &FakeTransport, state: &mut AccountState) -> NativeIncomingSession {
        NativeIncomingSession::authenticate(
            transport,
            state,
            authority(),
            1,
            [2; 32],
            parameters(),
            &notification(),
            "peer",
        )
        .unwrap()
    }
    fn tasks(number: u32) -> TaskIds {
        TaskIds {
            sdk: number,
            host: number + 1,
        }
    }
    struct Unsent<'a> {
        inner: &'a FakeTransport,
        route: CallHostRoute,
        attempts: std::cell::Cell<usize>,
    }
    impl PersonalTransport for Unsent<'_> {
        fn call_rpc(
            &self,
            state: &mut AccountState,
            route: CallHostRoute,
            request: &[u8],
        ) -> Result<Vec<u8>> {
            if route == self.route {
                self.attempts.set(self.attempts.get() + 1);
                return Err(CallRpcNotSubmitted.into());
            }
            self.inner.call_rpc(state, route, request)
        }
    }
    #[test]
    fn known_unsent_callee_actions_keep_cancel_phase_but_are_never_retried() {
        for operation in 0..3 {
            let transport = transport(if operation == 0 {
                vec![(CallHostRoute::AppAuth, auth_reply())]
            } else {
                vec![
                    (CallHostRoute::AppAuth, auth_reply()),
                    (CallHostRoute::RingingAck, ack_reply()),
                ]
            });
            let mut state = state();
            let mut session = session(&transport, &mut state);
            if operation != 0 {
                session
                    .ringing_ack(&transport, &mut state, authority(), tasks(1))
                    .unwrap();
            }
            let unsent = Unsent {
                inner: &transport,
                route: if operation == 0 {
                    CallHostRoute::RingingAck
                } else {
                    CallHostRoute::Transfer
                },
                attempts: std::cell::Cell::new(0),
            };
            for task in [2, 3] {
                let result = match operation {
                    0 => session.ringing_ack(&unsent, &mut state, authority(), tasks(task)),
                    1 => session.accept(&unsent, &mut state, authority(), tasks(task)),
                    _ => session.reject(&unsent, &mut state, authority(), tasks(task)),
                };
                assert!(result.is_err());
                if task == 2 {
                    assert!(result.unwrap_err().is::<CallRpcNotSubmitted>());
                }
            }
            assert_eq!(unsent.attempts.get(), 1);
            assert_eq!(
                session.phase(),
                if operation == 0 {
                    IncomingPhase::Invited
                } else {
                    IncomingPhase::Acknowledged
                }
            );
            let cancel = CancelNotification::decode(
                &p::message(&[p::uint(1, 7), p::uint(2, 9), p::uint(3, 0)]).unwrap(),
            )
            .unwrap();
            assert!(session.retire_cancel(&state, authority(), &cancel).unwrap());
            assert_eq!(session.phase(), IncomingPhase::Finished);
            assert!(transport.0.borrow().is_empty());
        }
    }
    #[test]
    fn incoming_reject_uses_its_own_operation_and_cannot_be_replayed() {
        let reply = wrapped_reply(
            3095,
            &p::message(&[p::uint(1, 7), p::uint(2, 9), p::bytes(3, &[])]).unwrap(),
        );
        let transport = transport(vec![
            (CallHostRoute::AppAuth, auth_reply()),
            (CallHostRoute::RingingAck, ack_reply()),
            (CallHostRoute::Transfer, reply),
        ]);
        let mut state = state();
        let mut session = session(&transport, &mut state);
        assert!(session
            .reject(&transport, &mut state, authority(), tasks(1))
            .is_err());
        session
            .ringing_ack(&transport, &mut state, authority(), tasks(1))
            .unwrap();
        session
            .reject(&transport, &mut state, authority(), tasks(2))
            .unwrap();
        assert_eq!(session.phase(), IncomingPhase::Finished);
        assert!(session
            .reject(&transport, &mut state, authority(), tasks(3))
            .is_err());
        assert!(transport.0.borrow().is_empty());
    }
    #[test]
    fn incoming_known_unselected_media_keeps_joined_control_for_one_exit() {
        let transport = transport(vec![
            (CallHostRoute::AppAuth, auth_reply()),
            (CallHostRoute::RingingAck, ack_reply()),
            (CallHostRoute::Transfer, join_reply(0, false)),
            (CallHostRoute::Transfer, exit_reply()),
        ]);
        let mut state = state();
        let mut session = session(&transport, &mut state);
        session
            .ringing_ack(&transport, &mut state, authority(), tasks(1))
            .unwrap();
        assert!(session
            .accept(&transport, &mut state, authority(), tasks(2))
            .unwrap_err()
            .is::<UnsupportedCallMedia>());
        assert_eq!(session.phase(), IncomingPhase::Joined);
        assert!(session.take_media(&state, authority(), 0, 0).is_err());
        session
            .exit(&transport, &mut state, authority(), tasks(3), false)
            .unwrap();
        assert!(transport.0.borrow().is_empty());
    }
    #[test]
    fn incoming_participant_mismatch_is_uncertain_and_cannot_be_canceled_as_caller() {
        let transport = transport(vec![
            (CallHostRoute::AppAuth, auth_reply()),
            (CallHostRoute::RingingAck, ack_reply()),
            (CallHostRoute::Transfer, join_reply(17, true)),
        ]);
        let mut state = state();
        let mut session = session(&transport, &mut state);
        session
            .ringing_ack(&transport, &mut state, authority(), tasks(1))
            .unwrap();
        assert!(session
            .accept(&transport, &mut state, authority(), tasks(2))
            .is_err());
        assert_eq!(session.phase(), IncomingPhase::Uncertain);
        assert!(session
            .exit(&transport, &mut state, authority(), tasks(3), false)
            .is_err());
        assert!(transport.0.borrow().is_empty());
    }
    #[test]
    fn incoming_builders_have_source_selected_ack_and_reject_tags() {
        let parameters = parameters();
        let room = VoiceRoom {
            room: 7,
            room_key: 9,
            own_member: 0,
        };
        let request = personal::acknowledge_voice(&parameters.inputs(), room, 1).unwrap();
        assert_eq!(request.operation, personal::HostOperation::RingingAck);
        let wrapper = fields(request.body()).unwrap();
        assert_eq!(
            wrapper.iter().map(|field| field.number).collect::<Vec<_>>(),
            vec![1, 3]
        );
        let ack = fields(data(&wrapper, 1).unwrap()).unwrap();
        assert_eq!(
            ack.iter().map(|field| field.number).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 7, 9, 11, 12, 13, 14, 15]
        );
        assert_eq!(number(&ack, 4, None).unwrap(), 1);
        assert_eq!(number(&ack, 13, None).unwrap(), 1);
        let reject = personal::reject(&parameters.inputs(), room, 2).unwrap();
        assert_eq!(reject.command_id, Some(3095));
        let body = fields(reject.body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body.iter().map(|field| field.number).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
        assert_eq!(number(&body, 6, None).unwrap(), 1);
        let join = personal::join(&parameters.inputs(), room, 0, 3).unwrap();
        assert_eq!(
            number(&fields(join.body.as_deref().unwrap()).unwrap(), 8, None).unwrap(),
            0
        );
    }

    #[test]
    fn acked_callee_polls_without_join_and_only_newer_full_roster_handles_elsewhere() {
        use super::super::tests::roster_sync;
        let transport = transport(vec![
            (CallHostRoute::AppAuth, auth_reply()),
            (CallHostRoute::RingingAck, ack_reply()),
            (
                CallHostRoute::LiveSync,
                roster_sync(1, Some(1), Some(1), false),
            ),
            (
                CallHostRoute::LiveSync,
                roster_sync(2, Some(1), Some(1), true),
            ),
        ]);
        let mut state = state();
        let mut session = session(&transport, &mut state);
        session
            .ringing_ack(&transport, &mut state, authority(), tasks(1))
            .unwrap();
        assert_eq!(
            session
                .poll(&transport, &mut state, authority(), tasks(2))
                .unwrap(),
            SyncProgress::Waiting
        );
        assert_eq!(session.phase(), IncomingPhase::Acknowledged);
        assert_eq!(session.base.active.as_ref().unwrap().member_sequence, 1);
        assert_eq!(
            session
                .poll(&transport, &mut state, authority(), tasks(3))
                .unwrap(),
            SyncProgress::Terminal(RemoteTermination::LocalStop(
                LocalStopReason::HandledElsewhere { sdk_end: 13 }
            ))
        );
        assert_eq!(session.phase(), IncomingPhase::Finished);
        assert!(transport.0.borrow().is_empty());
        // A zero-sequence ACK may omit both the roster and caller member.
        // Bind once from the next authenticated roster, using the invitation's
        // expected app identity; a different identity must not establish it.
        for wrong_identity in [false, true] {
            let empty_ack = host_reply(&p::bytes(
                1,
                &p::message(&[
                    p::uint(1, 7),
                    p::uint(2, 9),
                    p::uint(3, 0),
                    p::uint(4, 99),
                    p::bytes(5, b"ack-cursor"),
                    p::bytes(6, &p::message(&[p::uint(1, 0), p::bytes(3, b"")]).unwrap()),
                ])
                .unwrap(),
            ));
            let wire = self::transport(vec![
                (CallHostRoute::AppAuth, auth_reply()),
                (CallHostRoute::RingingAck, empty_ack),
                (
                    CallHostRoute::LiveSync,
                    roster_sync(1, Some(3), Some(1), false),
                ),
            ]);
            let mut next_state = super::super::tests::state();
            let mut next = self::session(&wire, &mut next_state);
            next.invite.caller_member = None;
            if wrong_identity {
                next.invite.caller_openid = b"different-peer".to_vec();
            }
            next.ringing_ack(&wire, &mut next_state, authority(), tasks(1))
                .unwrap();
            assert!(!next
                .base
                .active
                .as_ref()
                .unwrap()
                .original_members
                .contains_key("peer"));
            let update = next.poll(&wire, &mut next_state, authority(), tasks(2));
            if wrong_identity {
                assert!(update.is_err());
                assert!(next.invite.caller_member.is_none());
            } else {
                assert_eq!(update.unwrap(), SyncProgress::Waiting);
                assert_eq!(next.invite.caller_member, Some(1));
                assert!(next
                    .base
                    .active
                    .as_ref()
                    .unwrap()
                    .unbound_peer_openid
                    .is_none());
            }
        }
    }
    #[test]
    fn joined_callee_remote_hangup_requests_exit_even_after_peer_is_not_joined() {
        use super::super::tests::{recorded_exit_reason, roster_sync, RecordingTransport};
        let transport = RecordingTransport {
            inner: transport(vec![
                (CallHostRoute::AppAuth, auth_reply()),
                (CallHostRoute::RingingAck, ack_reply()),
                (CallHostRoute::Transfer, join_reply(17, false)),
                (
                    CallHostRoute::LiveSync,
                    roster_sync(3, Some(1), Some(2), false),
                ),
                (
                    CallHostRoute::LiveSync,
                    roster_sync(4, Some(1), Some(3), false),
                ),
                (
                    CallHostRoute::LiveSync,
                    roster_sync(5, Some(1), Some(1), false),
                ),
                (
                    CallHostRoute::LiveSync,
                    roster_sync(6, Some(1), Some(4), false),
                ),
                (CallHostRoute::Transfer, exit_reply()),
            ]),
            requests: Default::default(),
        };
        let mut state = state();
        let mut session = NativeIncomingSession::authenticate(
            &transport,
            &mut state,
            authority(),
            1,
            [2; 32],
            parameters(),
            &notification(),
            "peer",
        )
        .unwrap();
        session
            .ringing_ack(&transport, &mut state, authority(), tasks(1))
            .unwrap();
        session
            .accept(&transport, &mut state, authority(), tasks(2))
            .unwrap();
        for task in [3, 4] {
            assert_eq!(
                session
                    .poll(&transport, &mut state, authority(), tasks(task))
                    .unwrap(),
                SyncProgress::Waiting
            );
            assert_eq!(session.phase(), IncomingPhase::Joined);
            assert_eq!(session.base.phase(), PersonalPhase::Joined);
            assert!(!session.media_ready(&state, authority()).unwrap());
        }
        assert_eq!(
            session
                .poll(&transport, &mut state, authority(), tasks(5))
                .unwrap(),
            SyncProgress::MediaReady
        );
        assert_eq!(session.phase(), IncomingPhase::Joined);
        assert!(matches!(
            session
                .poll(&transport, &mut state, authority(), tasks(6))
                .unwrap(),
            SyncProgress::Terminal(RemoteTermination::Exit(RemoteExitReason::LastPeerLeft))
        ));
        session
            .finish_remote(&transport, &mut state, authority(), tasks(7), false)
            .unwrap();
        assert_eq!(session.phase(), IncomingPhase::Finished);
        assert_eq!(
            recorded_exit_reason(&transport.requests.borrow().last().unwrap().1),
            5
        );
    }
}
