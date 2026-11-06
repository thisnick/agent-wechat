//! Join an existing voice conference through its host room-entry operation.
//! The embedding account actor owns submission journaling and lifetime fences.
use super::*;

#[derive(Clone, PartialEq, Eq)]
pub struct ConferenceMember {
    pub member_id: u16,
    pub username: String,
    pub status: u32,
    pub microphone_enabled: Option<bool>,
}

#[derive(Clone)]
struct ConferenceRoom {
    identity: VoiceRoom,
    group_info: Vec<u8>,
    live_id: u64,
    cookies: Vec<u8>,
    strategy: Zeroizing<Vec<u8>>,
    members: BTreeMap<u16, ConferenceMember>,
    member_sequence: u32,
    av_sequence: u32,
    av_members: BTreeMap<u16, u32>,
    cap_sequence: u32,
    engine: Vec<u8>,
    channel: Vec<u8>,
    async_pending: bool,
}

struct Creator {
    invite_id: u64,
    pending_recipients: Vec<String>,
    invitation_timer_active: bool,
}

/// One authenticated room entry. A Join is never retried, and a known room
/// remains available for one participant Exit after unsupported negotiation.
pub struct ConferenceSession {
    base: NativePersonalSession,
    room: Option<ConferenceRoom>,
    join_spent: bool,
    exit_spent: bool,
    exit_confirmed: bool,
    own_departure_confirmed: bool,
    invitation_ids: BTreeSet<u64>,
    invitation_uncertain: bool,
    creator: Option<Creator>,
    creation_response: Option<wechat_protocol::calls::conference::CreateResponse>,
    entry_supported: bool,
    microphone_timestamp: u64,
    ignored_controls: u64,
}

/// The chat and conference identities are distinct. The caller must bind both
/// to a current invitation before submission; a chat ID alone is insufficient.
#[derive(Clone, Copy)]
pub struct ConferenceTarget<'a> {
    pub group_id: &'a str,
    pub cloud_group_id: &'a str,
    pub join_id: u64,
}

#[derive(Clone, Copy)]
pub struct ConferenceCreateTarget<'a> {
    pub group_id: &'a str,
    pub invitees: &'a [String],
    pub invite_id: u64,
}

impl ConferenceSession {
    pub fn authenticate(
        client: &impl PersonalTransport,
        state: &mut AccountState,
        authority: AppAuthority,
        host_task: u32,
        device: [u8; 32],
        parameters: VoiceParameters,
    ) -> Result<Self> {
        Ok(Self {
            base: NativePersonalSession::authenticate(
                client, state, authority, host_task, device, parameters,
            )?,
            room: None,
            join_spent: false,
            exit_spent: false,
            exit_confirmed: false,
            own_departure_confirmed: false,
            invitation_ids: BTreeSet::new(),
            invitation_uncertain: false,
            creator: None,
            creation_response: None,
            entry_supported: false,
            microphone_timestamp: 0,
            ignored_controls: 0,
        })
    }

    /// Create one group voice conference. Allocation does not imply that any
    /// invitee joined or that media is ready. The authenticated creation receipt
    /// remains available even if later negotiation is unsupported.
    pub fn create(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        target: ConferenceCreateTarget<'_>,
        tasks: TaskIds,
    ) -> Result<()> {
        use wechat_protocol::calls::conference;
        let ConferenceCreateTarget {
            group_id,
            invitees,
            invite_id,
        } = target;
        self.base.check(state, current)?;
        ensure!(!self.join_spent, "Conference entry already spent");
        let mut parameters = self.base.parameters.clone();
        parameters.group = group_info(group_id, b"")?;
        let request = conference::create_voice(
            &parameters.inputs(),
            group_id,
            invitees,
            invite_id,
            tasks.sdk,
            None,
        )?;
        self.base.reserve(tasks)?;
        self.join_spent = true;
        self.creator = Some(Creator {
            invite_id,
            pending_recipients: invitees.to_vec(),
            invitation_timer_active: true,
        });
        let reply = self.base.host(
            client,
            state,
            CallHostRoute::Invite,
            request.body(),
            tasks.host,
        )?;
        self.creation_response = Some(conference::CreateResponse::decode(&reply)?);
        let response = self.creation_response.as_ref().expect("creation receipt");
        let identity = response
            .allocated_room()?
            .context("Conference creation did not allocate a room")?;
        u16::try_from(identity.own_member).context("Unsupported conference member identifier")?;
        let cloud = response
            .cloud_group_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .context("Missing created conference identity")?;
        self.room = Some(empty_room(identity, group_id, cloud.as_bytes())?);
        let client = fields(
            response
                .client_response
                .as_deref()
                .context("Missing created conference client response")?,
        )?;
        let installed =
            install_client_response(self.room.as_mut().expect("allocated conference"), &client);
        self.update_invitation_timer();
        installed?;
        let response = self.creation_response.as_ref().expect("creation receipt");
        ensure!(
            response
                .custom_auth
                .as_ref()
                .is_none_or(|auth| auth.is_empty())
                && response
                    .verification_url
                    .as_ref()
                    .is_none_or(|url| url.is_empty())
                && response.unknown_fields.is_empty(),
            "Conference creation requires unsupported verification or response fields"
        );
        self.entry_supported = true;
        Ok(())
    }

    pub fn ignored_control_count(&self) -> u64 {
        self.ignored_controls
    }

    pub fn creation_response(&self) -> Option<&wechat_protocol::calls::conference::CreateResponse> {
        self.creation_response.as_ref()
    }

    fn update_invitation_timer(&mut self) {
        let (Some(creator), Some(room)) = (&mut self.creator, &self.room) else {
            return;
        };
        // Once stopped, the creator's invitation timer never starts again when
        // peers leave. Pending invitations alone are not the timer predicate.
        if room
            .members
            .values()
            .filter(|member| member.status == 1)
            .count()
            >= 2
        {
            creator.invitation_timer_active = false;
        }
    }

    pub fn join(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        target: ConferenceTarget<'_>,
        tasks: TaskIds,
    ) -> Result<()> {
        self.base.check(state, current)?;
        ensure!(!self.join_spent, "Conference entry already spent");
        let body = join_request(&self.base.parameters, &target)?;
        let group_id = target.group_id;
        let cloud_group_id = target.cloud_group_id;
        self.base.reserve(tasks)?;
        self.join_spent = true;
        let reply = self.base.host(
            client,
            state,
            CallHostRoute::ConferenceJoin,
            &body,
            tasks.host,
        )?;
        let outer = fields(&reply)?;
        let identity = VoiceRoom {
            room: number(&outer, 1, None)?,
            room_key: number(&outer, 2, None)?,
            own_member: u32::from(u16::try_from(number(&outer, 3, None)?)?),
        };
        ensure!(
            identity.room != 0 && identity.room_key != 0,
            "Unallocated conference room"
        );
        let cloud_group = p::optional_data(&outer, 6)?.unwrap_or(cloud_group_id.as_bytes());
        ensure!(
            cloud_group.len() <= 1024 && !cloud_group.contains(&0),
            "Invalid conference group context"
        );
        ensure!(
            cloud_group == cloud_group_id.as_bytes(),
            "Conference identity changed during entry"
        );
        std::str::from_utf8(cloud_group)?;
        self.room = Some(empty_room(identity, group_id, cloud_group)?);
        ensure!(
            p::optional_data(&outer, 5)?.is_none_or(|auth| auth.is_empty()),
            "Conference custom auth unsupported"
        );
        let response = fields(data(&outer, 4)?)?;
        let room = self.room.as_mut().expect("allocated conference");
        room.async_pending = number(&response, 10, Some(0))? == 1;
        ensure!(
            number(&response, 10, Some(0))? <= 1,
            "Unsupported conference join mode"
        );
        install_client_response(room, &response)?;
        self.entry_supported = true;
        Ok(())
    }

    pub fn matches_room(&self, room_id: u64, room_key: u64) -> bool {
        self.room
            .as_ref()
            .is_some_and(|room| room.identity.room == room_id && room.identity.room_key == room_key)
    }
    pub fn own_membership_ready(&self) -> bool {
        self.room.as_ref().is_some_and(|room| {
            !room.async_pending
                && room
                    .members
                    .get(&(room.identity.own_member as u16))
                    .is_some_and(|member| {
                        member.status == 1 && member.username == self.base.own_username
                    })
        })
    }
    pub fn has_room(&self) -> bool {
        self.room.is_some() && !self.exit_spent && !self.own_departure_confirmed
    }

    pub fn join_pending(&self) -> bool {
        self.room.as_ref().is_some_and(|room| room.async_pending)
    }

    pub fn members(&self) -> Vec<ConferenceMember> {
        self.room
            .as_ref()
            .map(|room| {
                room.members
                    .values()
                    .cloned()
                    .map(|mut member| {
                        member.microphone_enabled = room
                            .av_members
                            .get(&member.member_id)
                            .map(|flags| flags & 1 != 0);
                        member
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Invite contacts into this joined room exactly once for the supplied
    /// intent. A successful response reports invitation submission separately
    /// from membership. Unknown and verification results remain in the receipt.
    /// Any failed exchange blocks further invitations; it never repeats through
    /// the inner AddMember route or substitutes a new intent automatically.
    pub fn invite(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        invitees: &[String],
        invite_id: u64,
        tasks: TaskIds,
    ) -> Result<wechat_protocol::calls::conference::AddMembersResponse> {
        use wechat_protocol::calls::conference;
        self.base.check(state, current)?;
        ensure!(
            self.entry_supported
                && self.has_room()
                && self.own_membership_ready()
                && !self.invitation_uncertain,
            "Conference is not ready for invitations"
        );
        ensure!(
            self.invitation_ids.len() < 4096 && !self.invitation_ids.contains(&invite_id),
            "Conference invitation intent already spent or exhausted"
        );
        let room = self.room.as_ref().context("Conference room missing")?;
        let group = fields(&room.group_info)?;
        let group_id = std::str::from_utf8(data(&group, 6)?)?;
        let identity = room.identity;
        let request = conference::add_members(identity, group_id, invitees, invite_id, tasks.sdk)?;
        self.base.reserve(tasks)?;
        self.invitation_ids.insert(invite_id);
        self.invitation_uncertain = true;
        let reply = match self.base.host(
            client,
            state,
            CallHostRoute::ConferenceAddMembers,
            request.body(),
            tasks.host,
        ) {
            Ok(reply) => reply,
            Err(error) => {
                if error.is::<CallRpcNotSubmitted>() {
                    self.invitation_uncertain = false;
                }
                return Err(error);
            }
        };
        let response = conference::AddMembersResponse::decode(&reply)?;
        response.check_room(identity)?;
        self.invitation_uncertain = false;
        Ok(response)
    }

    pub fn media_profile(&self) -> Result<[u64; 6]> {
        let room = self.room.as_ref().context("Conference room missing")?;
        profile_shape(&room.engine, &room.channel)
    }

    pub fn set_muted(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        muted: bool,
        timestamp: u64,
        tasks: TaskIds,
    ) -> Result<()> {
        use wechat_protocol::calls::conference;
        self.base.check(state, current)?;
        ensure!(
            self.entry_supported && self.has_room() && self.own_membership_ready(),
            "Conference microphone unavailable"
        );
        ensure!(
            timestamp > self.microphone_timestamp,
            "Conference microphone timestamp already spent"
        );
        let room = self.room.as_ref().context("Conference room missing")?;
        ensure!(
            room.av_members
                .get(&(room.identity.own_member as u16))
                .is_none_or(|flags| flags & !1 == 0),
            "Nonvoice conference microphone state unsupported"
        );
        let identity = room.identity;
        let mut parameters = self.base.parameters.clone();
        parameters.group.clone_from(&room.group_info);
        let request = conference::switch_microphone(
            &parameters.inputs(),
            identity,
            !muted,
            timestamp,
            tasks.sdk,
        )?;
        let framing = AppFrameParameters {
            sdk_version: parameters.sdk_version()?,
            cloud_device_id: &parameters.cloud_device_id,
            host_client_version: state.device.client_version as u32,
            app_config_version: parameters.app_config_version,
        };
        let delegate = app_frame::prepare_transfer(
            &self.base.app,
            current,
            CallOperation::SwitchAv,
            request.body.as_deref().context("Missing microphone body")?,
            &framing,
        )?;
        self.base.reserve(tasks)?;
        self.microphone_timestamp = timestamp;
        let reply = self.base.host(
            client,
            state,
            CallHostRoute::Transfer,
            &delegate,
            tasks.host,
        )?;
        let reply = app_frame::decode_transfer_reply(
            &self.base.app,
            current,
            CallOperation::SwitchAv,
            &reply,
        )?;
        let fields = fields(&reply)?;
        ensure!(
            number(&fields, 1, None)? == identity.room
                && number(&fields, 2, None)? == identity.room_key
                && number(&fields, 3, None)? == u64::from(identity.own_member),
            "Conference microphone response context mismatch"
        );
        if let Some(av) = p::optional_data(&fields, 4)? {
            install_av(
                self.room.as_mut().expect("conference room"),
                &super::fields(av)?,
            )?;
        }
        Ok(())
    }

    pub fn take_media(
        &self,
        connection: usize,
        udp: usize,
    ) -> Result<wechat_protocol::calls::relay::SelectedRelay> {
        let room = self.room.as_ref().context("Conference room missing")?;
        ensure!(
            self.entry_supported
                && self.has_room()
                && !room.async_pending
                && room
                    .members
                    .get(&(room.identity.own_member as u16))
                    .is_some_and(
                        |member| member.status == 1 && member.username == self.base.own_username
                    ),
            "Conference membership incomplete"
        );
        conference_receive_profile(&room.engine, &room.channel)?;
        wechat_protocol::calls::relay::SelectedRelay::from_strategy(
            room.identity.room,
            room.identity.room_key,
            room.identity.own_member as u16,
            &room.strategy,
            connection,
            udp,
        )
    }

    pub fn poll(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<bool> {
        self.base.check(state, current)?;
        ensure!(
            self.entry_supported && !self.exit_spent,
            "Conference entry unsupported or already left"
        );
        self.base.reserve(tasks)?;
        let room = self.room.as_ref().context("Conference room missing")?;
        ensure!(room.live_id != 0, "Conference live context missing");
        let request = p::message(&[
            p::uint(1, room.live_id),
            p::bytes(2, &room.cookies),
            p::bytes(4, b"voip-mt"),
            p::uint(5, if self.creator.is_some() { 1 } else { 3 }),
        ])?;
        let reply = self
            .base
            .host(client, state, CallHostRoute::LiveSync, &request, tasks.host)?;
        let response = personal::LiveSyncResponse::decode(&reply)?;
        self.ignored_controls = self
            .ignored_controls
            .saturating_add(response.unknown_fields.len() as u64);
        let previously_joined = self.own_membership_ready();
        let mut next = self.room.as_ref().expect("conference room").clone();
        for item in response.items {
            self.ignored_controls = self
                .ignored_controls
                .saturating_add(item.unknown_fields.len() as u64);
            if item.kind != 1 {
                self.ignored_controls = self.ignored_controls.saturating_add(1);
                continue;
            }
            let message = fields(&item.body)?;
            if number(&message, 1, None)? != 10002 {
                self.ignored_controls = self.ignored_controls.saturating_add(1);
                continue;
            }
            let notify = fields(data(&message, 2)?)?;
            ensure!(
                number(&notify, 3, None)? == next.identity.room
                    && matches_room_key(&notify, 4, next.identity.room_key)?,
                "Conference notification identity mismatch"
            );
            for raw in repeated(&notify, 5, 64)? {
                let item = fields(raw)?;
                let kind = number(&item, 1, None)?;
                if !matches!(kind, 10 | 11 | 12 | 19) {
                    self.ignored_controls = self.ignored_controls.saturating_add(1);
                    continue;
                }
                let body = fields(data(&item, 2)?)?;
                match kind {
                    10 => {
                        ensure!(
                            matches_room_key(&body, 1, next.identity.room)?
                                && matches_room_key(&body, 2, next.identity.room_key)?,
                            "Conference roster identity mismatch"
                        );
                        install_roster(&mut next, &body, 3, 4)?;
                    }
                    11 => install_capability(&mut next, data(&item, 2)?)?,
                    12 => {
                        install_av(&mut next, &body)?;
                    }
                    19 => {
                        ensure!(
                            next.async_pending && number(&body, 1, None)? == 0,
                            "Unexpected or failed asynchronous conference join"
                        );
                        let joined = fields(data(&body, 2)?)?;
                        ensure!(
                            number(&joined, 1, None)? == next.identity.room
                                && number(&joined, 2, None)? == next.identity.room_key,
                            "Asynchronous conference identity mismatch"
                        );
                        next.identity.own_member =
                            u32::from(u16::try_from(number(&joined, 3, None)?)?);
                        let strategy = data(&joined, 4)?;
                        install_strategy(&mut next, strategy)?;
                        install_roster(&mut next, &fields(data(&joined, 13)?)?, 1, 2)?;
                        if let Some(av) = p::optional_data(&joined, 9)? {
                            install_av(&mut next, &fields(av)?)?;
                        }
                        next.live_id = number(&joined, 11, None)?;
                        next.cookies = data(&joined, 12)?.to_vec();
                        update_group_live(&mut next)?;
                        next.async_pending = false;
                    }
                    _ => unreachable!("known conference control"),
                }
            }
        }
        let joined = next
            .members
            .get(&(next.identity.own_member as u16))
            .is_some_and(|member| member.status == 1 && member.username == self.base.own_username);
        if previously_joined && !next.async_pending && !joined {
            self.own_departure_confirmed = true;
        }
        next.cookies = response.cookies.unwrap_or_default();
        self.room = Some(next);
        self.update_invitation_timer();
        Ok(joined)
    }

    pub fn exit(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<()> {
        self.base.check(state, current)?;
        if self.exit_confirmed {
            return Ok(());
        }
        ensure!(!self.exit_spent, "Conference Exit already spent");
        if self.own_departure_confirmed {
            self.exit_spent = true;
            self.exit_confirmed = true;
            return Ok(());
        }
        if self
            .creator
            .as_ref()
            .is_some_and(|creator| creator.invitation_timer_active)
        {
            return self.cancel_creation(client, state, tasks);
        }
        self.base.reserve(tasks)?;
        let room = self.room.as_ref().context("Conference room missing")?;
        let identity = room.identity;
        let body = p::message(&[
            p::bytes(1, &self.base.parameters.sdk),
            p::bytes(2, &room.group_info),
            p::uint(3, identity.room),
            p::uint(4, identity.room_key),
            p::uint(5, u64::from(identity.own_member)),
            p::uint(6, 1),
            p::uint(7, u64::from(self.base.parameters.network_type)),
            p::uint(8, 1),
        ])?;
        let parameters = AppFrameParameters {
            sdk_version: self.base.parameters.sdk_version()?,
            cloud_device_id: &self.base.parameters.cloud_device_id,
            host_client_version: state.device.client_version as u32,
            app_config_version: self.base.parameters.app_config_version,
        };
        let request = app_frame::prepare_transfer(
            &self.base.app,
            current,
            CallOperation::Exit,
            &body,
            &parameters,
        )?;
        self.exit_spent = true;
        let delegate =
            self.base
                .host(client, state, CallHostRoute::Transfer, &request, tasks.host)?;
        let response = fields(&app_frame::decode_transfer_reply(
            &self.base.app,
            current,
            CallOperation::Exit,
            &delegate,
        )?)?;
        ensure!(
            number(&response, 1, None)? == identity.room
                && number(&response, 2, None)? == identity.room_key
                && number(&response, 3, None)? == u64::from(identity.own_member),
            "Conference Exit identity mismatch"
        );
        self.exit_confirmed = true;
        Ok(())
    }

    fn cancel_creation(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        tasks: TaskIds,
    ) -> Result<()> {
        use wechat_protocol::calls::conference;
        let creator = self
            .creator
            .as_ref()
            .context("Missing conference creator")?;
        let room = self.room.as_ref().context("Conference room missing")?;
        let identity = room.identity;
        let mut pending = creator.pending_recipients.clone();
        pending.extend(
            room.members
                .values()
                .filter(|member| matches!(member.status, 2 | 3))
                .map(|member| member.username.clone()),
        );
        let mut parameters = self.base.parameters.clone();
        parameters.group.clone_from(&room.group_info);
        let request = conference::cancel_creation(
            &parameters.inputs(),
            identity,
            creator.invite_id,
            &pending,
            personal::CancelReason::Local,
            tasks.sdk,
        )?;
        self.base.reserve(tasks)?;
        self.exit_spent = true;
        let reply = self.base.host(
            client,
            state,
            CallHostRoute::Cancel,
            request.body(),
            tasks.host,
        )?;
        let outer = fields(&reply)?;
        let cancelled = fields(data(&outer, 1)?)?;
        ensure!(
            number(&cancelled, 1, None)? == identity.room
                && number(&cancelled, 2, None)? == identity.room_key,
            "Conference cancellation room mismatch"
        );
        self.exit_confirmed = true;
        Ok(())
    }
}

fn group_info(group_id: &str, cloud_group: &[u8]) -> Result<Vec<u8>> {
    p::message(&[
        p::bytes(1, b"wechat"),
        p::uint(2, 0),
        p::bytes(3, b"voip-mt"),
        p::bytes(4, cloud_group),
        p::uint(5, 1),
        p::bytes(6, group_id.as_bytes()),
        p::uint(7, 0),
    ])
}

fn empty_room(identity: VoiceRoom, group_id: &str, cloud_group: &[u8]) -> Result<ConferenceRoom> {
    ensure!(
        !cloud_group.is_empty() && cloud_group.len() <= 1024 && !cloud_group.contains(&0),
        "Invalid conference group context"
    );
    std::str::from_utf8(cloud_group)?;
    Ok(ConferenceRoom {
        identity,
        group_info: group_info(group_id, cloud_group)?,
        live_id: 0,
        cookies: vec![],
        strategy: Zeroizing::new(vec![]),
        members: BTreeMap::new(),
        member_sequence: 0,
        av_sequence: 0,
        av_members: BTreeMap::new(),
        cap_sequence: 0,
        engine: vec![],
        channel: vec![],
        async_pending: false,
    })
}

// Receiving is limited to this negotiated codec/layout. Personal-call mode is
// deliberately excluded, and this does not admit a conference transmitter.
fn conference_receive_profile(engine: &[u8], channel: &[u8]) -> Result<()> {
    if !matches!(
        profile_shape(engine, channel)?,
        [17, 0x106 | 0x206, 0, 0, 2, 3] | [17, 0x206, 0, 0, 2, 0]
    ) {
        return Err(UnsupportedCallMedia.into());
    }
    Ok(())
}

fn install_client_response(room: &mut ConferenceRoom, response: &[Field]) -> Result<()> {
    room.live_id = number(response, 5, None)?;
    ensure!(room.live_id != 0, "Missing conference live identity");
    room.cookies = data(response, 6)?.to_vec();
    update_group_live(room)?;
    if let Some(roster) = p::optional_data(response, 2)? {
        install_roster(room, &fields(roster)?, 1, 2)?;
    }
    if let Some(av) = p::optional_data(response, 3)? {
        install_av(room, &fields(av)?)?;
    }
    if let Some(strategy) = p::optional_data(response, 1)? {
        install_strategy(room, strategy)?;
    }
    Ok(())
}

fn update_group_live(room: &mut ConferenceRoom) -> Result<()> {
    let group = fields(&room.group_info)?;
    let name = std::str::from_utf8(data(&group, 4)?)?;
    room.group_info = replace_group(&room.group_info, room.live_id, name)?;
    Ok(())
}

fn install_strategy(room: &mut ConferenceRoom, raw: &[u8]) -> Result<()> {
    let strategy = fields(raw)?;
    ensure!(
        number(&strategy, 14, Some(0))? & (1 << 36) == 0,
        "Alternate conference sync unsupported"
    );
    if let Some(capability) = p::optional_data(&strategy, 13)? {
        install_capability(room, capability)?;
    }
    room.strategy = Zeroizing::new(raw.to_vec());
    Ok(())
}

fn install_capability(room: &mut ConferenceRoom, raw: &[u8]) -> Result<()> {
    let (sequence, engine, channel) = cap(raw)?;
    if sequence > room.cap_sequence {
        room.cap_sequence = sequence;
        room.engine = engine;
        room.channel = channel;
    }
    Ok(())
}

fn install_roster(
    room: &mut ConferenceRoom,
    values: &[Field],
    sequence_field: u32,
    members_field: u32,
) -> Result<()> {
    let sequence = u32::try_from(number(values, sequence_field, Some(0))?)?;
    if sequence <= room.member_sequence {
        return Ok(());
    }
    let mut members = BTreeMap::new();
    let mut names = BTreeSet::new();
    for entry in repeated(values, members_field, 128)? {
        let entry = fields(entry)?;
        let member = u16::try_from(number(&entry, 2, None)?)?;
        let username = std::str::from_utf8(data(&entry, 7)?)?;
        ensure!(
            !username.is_empty()
                && username.len() <= 128
                && !username.chars().any(char::is_control)
                && names.insert(username.to_owned())
                && !members.contains_key(&member),
            "Invalid conference member identity"
        );
        members.insert(
            member,
            ConferenceMember {
                member_id: member,
                username: username.to_owned(),
                status: u32::try_from(number(&entry, 3, None)?)?,
                microphone_enabled: None,
            },
        );
    }
    // A new identity under a reused member ID cannot inherit its predecessor's
    // AV state. Membership and microphone state have independent sequences.
    room.av_members.retain(|id, _| {
        room.members.get(id).is_some_and(|old| {
            members
                .get(id)
                .is_some_and(|new| new.username == old.username)
        })
    });
    room.members = members;
    room.member_sequence = sequence;
    Ok(())
}

fn install_av(room: &mut ConferenceRoom, values: &[Field]) -> Result<()> {
    let sequence = u32::try_from(number(values, 3, Some(0))?)?;
    let mut members = BTreeMap::new();
    for entry in repeated(values, 4, 128)? {
        let entry = fields(entry)?;
        let member = u16::try_from(number(&entry, 1, None)?)?;
        let flags = u32::try_from(number(&entry, 2, None)?)?;
        ensure!(
            members.insert(member, flags).is_none(),
            "Duplicate conference AV member"
        );
        p::optional_data(&entry, 3)?;
    }
    if sequence > room.av_sequence {
        room.av_members = members;
        room.av_sequence = sequence;
    }
    Ok(())
}

fn join_request(parameters: &VoiceParameters, target: &ConferenceTarget<'_>) -> Result<Vec<u8>> {
    let ConferenceTarget {
        group_id,
        cloud_group_id,
        join_id,
    } = *target;
    ensure!(join_id != 0, "Invalid conference entry");
    ensure!(
        group_id.ends_with("@chatroom")
            && group_id.len() <= 128
            && !group_id.chars().any(char::is_control),
        "Invalid conference group"
    );
    ensure!(
        !cloud_group_id.is_empty()
            && cloud_group_id.len() <= 1024
            && !cloud_group_id.chars().any(char::is_control),
        "Missing or invalid conference identity"
    );
    let sdk = fields(&parameters.sdk)?;
    let capabilities = p::message(&[
        p::bytes(1, &parameters.engine_capability),
        p::bytes(2, &parameters.channel_capability),
    ])?;
    let mut parts = vec![
        p::uint(1, number(&sdk, 1, None)?),
        p::bytes(2, data(&sdk, 2)?),
        p::uint(3, u64::from(parameters.network_type)),
        p::uint(4, 1),
        p::bytes(5, &capabilities),
        p::bytes(6, &parameters.device),
        p::uint(7, u64::from(parameters.video_length)),
        p::uint(8, u64::from(parameters.video_ratio)),
    ];
    parts.extend(parameters.nics.iter().map(|nic| p::bytes(9, nic)));
    parts.extend([
        p::uint(10, u64::from(parameters.sim_type)),
        p::bytes(11, b""),
    ]);
    p::message(&[
        p::bytes(1, group_id.as_bytes()),
        p::bytes(2, cloud_group_id.as_bytes()),
        p::bytes(3, &p::message(&parts)?),
        p::uint(4, join_id),
        p::bytes(6, b"voip-mt"),
    ])
}

#[cfg(test)]
mod tests {
    use super::super::tests::parameters;
    use super::*;

    #[test]
    fn conference_receive_admission_keeps_personal_and_other_profiles_separate() {
        let engine = |codec, packet, extension| {
            p::message(&[p::uint(5, codec), p::uint(6, packet), p::uint(7, extension)]).unwrap()
        };
        let channel = p::message(&[p::uint(1, 2), p::uint(2, 3)]).unwrap();
        for packet in [0x106, 0x206] {
            let group = engine(17, packet, 0);
            assert!(conference_receive_profile(&group, &channel).is_ok());
            assert!(profile(&group, &channel).is_err());
            let personal = engine(17, packet, 0x800000);
            assert!(conference_receive_profile(&personal, &channel).is_err());
            assert!(profile(&personal, &channel).is_ok());
        }
        assert!(conference_receive_profile(&engine(0, 0x106, 0), &channel).is_err());
        assert!(conference_receive_profile(&engine(17, 0x306, 0), &channel).is_err());
        assert!(conference_receive_profile(&engine(17, 0x106, 0x100), &channel).is_err());
    }

    #[test]
    fn creator_channel_health_mode_does_not_require_another_capability_update() {
        let engine =
            |packet| p::message(&[p::uint(5, 17), p::uint(6, packet), p::uint(7, 0)]).unwrap();
        let channel = |flags, health| p::message(&[p::uint(1, flags), p::uint(2, health)]).unwrap();
        assert!(conference_receive_profile(&engine(0x206), &channel(2, 0)).is_ok());
        assert!(profile(&engine(0x206), &channel(2, 0)).is_err());
        assert!(conference_receive_profile(&engine(0x106), &channel(2, 0)).is_err());
        for health in [1, 2, 4, 32] {
            assert!(conference_receive_profile(&engine(0x206), &channel(2, health)).is_err());
        }
        for flags in [0, 1, 3, 6] {
            assert!(conference_receive_profile(&engine(0x206), &channel(flags, 0)).is_err());
        }
    }

    struct Host {
        replies: std::cell::RefCell<std::collections::VecDeque<(CallHostRoute, Vec<u8>)>>,
        requests: std::cell::RefCell<Vec<(CallHostRoute, Vec<u8>)>>,
    }
    impl PersonalTransport for Host {
        fn call_rpc(
            &self,
            _state: &mut AccountState,
            route: CallHostRoute,
            request: &[u8],
        ) -> Result<Vec<u8>> {
            let (expected, reply) = self
                .replies
                .borrow_mut()
                .pop_front()
                .context("Unexpected conference RPC")?;
            ensure!(route == expected, "Unexpected conference route");
            self.requests.borrow_mut().push((route, request.to_vec()));
            Ok(reply)
        }
    }
    fn roster() -> Vec<u8> {
        p::message(&[
            p::uint(1, 1),
            p::bytes(2, &super::super::tests::member("own", 0, 1)),
            p::bytes(2, &super::super::tests::member("peer", 1, 1)),
            p::bytes(2, &super::super::tests::member("third", 2, 1)),
        ])
        .unwrap()
    }
    fn strategy() -> Vec<u8> {
        let (engine, channel) = super::super::tests::capabilities_for_codec(17);
        let content = p::message(&[p::bytes(1, &engine), p::bytes(2, &channel)]).unwrap();
        let cap = p::message(&[p::uint(1, 1), p::bytes(2, &content)]).unwrap();
        p::message(&[p::bytes(13, &cap)]).unwrap()
    }
    fn join_reply(asynchronous: bool) -> Vec<u8> {
        let mut client = vec![
            p::uint(5, 99),
            p::bytes(6, b"initial"),
            p::uint(10, u64::from(asynchronous)),
        ];
        if !asynchronous {
            client.extend([p::bytes(1, &strategy()), p::bytes(2, &roster())]);
        }
        super::super::tests::host_reply(
            &p::message(&[
                p::uint(1, 7),
                p::uint(2, 9),
                p::uint(3, 0),
                p::bytes(4, &p::message(&client).unwrap()),
                p::bytes(6, b"opaque-context"),
            ])
            .unwrap(),
        )
    }
    fn async_reply(room: u64) -> Vec<u8> {
        let joined = p::message(&[
            p::uint(1, room),
            p::uint(2, 9),
            p::uint(3, 0),
            p::bytes(4, &strategy()),
            p::uint(11, 99),
            p::bytes(12, b"joined"),
            p::bytes(13, &roster()),
        ])
        .unwrap();
        let response = p::message(&[p::uint(1, 0), p::bytes(2, &joined)]).unwrap();
        let item = p::message(&[p::uint(1, 19), p::bytes(2, &response)]).unwrap();
        let notify = p::message(&[p::uint(3, 7), p::uint(4, 9), p::bytes(5, &item)]).unwrap();
        let command = p::message(&[p::uint(1, 10002), p::bytes(2, &notify)]).unwrap();
        let message = p::message(&[p::uint(1, 1), p::bytes(2, &command)]).unwrap();
        super::super::tests::host_reply(
            &p::message(&[p::bytes(1, &message), p::bytes(2, b"next")]).unwrap(),
        )
    }
    fn session(host: &Host) -> (ConferenceSession, AccountState, AppAuthority) {
        let mut state = super::super::tests::state();
        let authority = super::super::tests::authority();
        let session =
            ConferenceSession::authenticate(host, &mut state, authority, 1, [5; 32], parameters())
                .unwrap();
        (session, state, authority)
    }
    fn joined(host: &Host) -> (ConferenceSession, AccountState, AppAuthority) {
        let (mut session, mut state, authority) = session(host);
        session
            .join(
                host,
                &mut state,
                authority,
                ConferenceTarget {
                    group_id: "test@chatroom",
                    cloud_group_id: "opaque-context",
                    join_id: 123,
                },
                TaskIds { host: 2, sdk: 1 },
            )
            .unwrap();
        (session, state, authority)
    }

    fn creation_reply(peer_status: u32, custom_auth: bool) -> Vec<u8> {
        let roster = p::message(&[
            p::uint(1, 1),
            p::bytes(2, &super::super::tests::member("own", 0, 1)),
            p::bytes(2, &super::super::tests::member("peer", 1, peer_status)),
            p::bytes(2, &super::super::tests::member("later", 2, 2)),
        ])
        .unwrap();
        let client = p::message(&[
            p::bytes(1, &strategy()),
            p::bytes(2, &roster),
            p::uint(5, 99),
            p::bytes(6, b"initial"),
        ])
        .unwrap();
        let mut values = vec![
            p::uint(1, 7),
            p::uint(2, 9),
            p::uint(3, 0),
            p::bytes(4, b"created-cloud"),
            p::bytes(5, &client),
            p::bytes(
                8,
                &p::message(&[p::bytes(1, b"peer"), p::uint(2, 0)]).unwrap(),
            ),
            p::bytes(
                8,
                &p::message(&[p::bytes(1, b"later"), p::uint(2, 5)]).unwrap(),
            ),
        ];
        if custom_auth {
            values.push(p::bytes(7, b"unsupported-auth"));
        }
        super::super::tests::host_reply(&p::message(&values).unwrap())
    }

    #[test]
    fn creator_leaves_after_a_peer_joins_even_with_other_invitations_pending() {
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::Invite, creation_reply(1, false)),
                (
                    CallHostRoute::LiveSync,
                    super::super::tests::host_reply(&p::bytes(2, b"next")),
                ),
                (CallHostRoute::Transfer, super::super::tests::exit_reply()),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = session(&host);
        session
            .create(
                &host,
                &mut state,
                authority,
                ConferenceCreateTarget {
                    group_id: "test@chatroom",
                    invitees: &["peer".into(), "later".into()],
                    invite_id: 123,
                },
                TaskIds { host: 2, sdk: 1 },
            )
            .unwrap();
        assert!(session.own_membership_ready());
        assert!(!session.creator.as_ref().unwrap().invitation_timer_active);
        let receipt = session.creation_response().unwrap();
        assert_eq!(receipt.invite_results[1].service_code, Some(5));
        // The timer stays stopped even if the accepted peer later leaves.
        let newer = p::message(&[
            p::uint(1, 2),
            p::bytes(2, &super::super::tests::member("own", 0, 1)),
            p::bytes(2, &super::super::tests::member("later", 2, 2)),
        ])
        .unwrap();
        install_roster(
            session.room.as_mut().unwrap(),
            &fields(&newer).unwrap(),
            1,
            2,
        )
        .unwrap();
        session.update_invitation_timer();
        session
            .poll(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .unwrap();
        session
            .exit(&host, &mut state, authority, TaskIds { host: 4, sdk: 3 })
            .unwrap();
        assert!(!session.has_room());
        let requests = host.requests.borrow();
        assert_eq!(
            requests
                .iter()
                .map(|(r, _)| r.command())
                .collect::<Vec<_>>(),
            [7994, 10342, 9262, 11421]
        );
        let poll = CommonRequest::decode(&requests[2].1).unwrap();
        assert_eq!(
            number(&fields(poll.body.as_deref().unwrap()).unwrap(), 5, None).unwrap(),
            1
        );
        let common = CommonRequest::decode(&requests[1].1).unwrap();
        let body = fields(common.body.as_deref().unwrap()).unwrap();
        assert_eq!(data(&body, 7).unwrap(), b"voip-mt");
        assert_eq!(number(&body, 5, None).unwrap(), 123);
        assert_eq!(data(&body, 3).unwrap(), b"test@chatroom");
        assert_eq!(
            number(&fields(data(&body, 4).unwrap()).unwrap(), 4, None).unwrap(),
            1
        );
    }

    #[test]
    fn unanswered_creator_cancels_the_original_invitation_once() {
        let cancel = super::super::tests::host_reply(&p::bytes(
            1,
            &p::message(&[p::uint(1, 7), p::uint(2, 9)]).unwrap(),
        ));
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::Invite, creation_reply(2, false)),
                (CallHostRoute::Cancel, cancel),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = session(&host);
        session
            .create(
                &host,
                &mut state,
                authority,
                ConferenceCreateTarget {
                    group_id: "test@chatroom",
                    invitees: &["peer".into()],
                    invite_id: 123,
                },
                TaskIds { host: 2, sdk: 1 },
            )
            .unwrap();
        assert!(session.creator.as_ref().unwrap().invitation_timer_active);
        session
            .exit(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .unwrap();
        session
            .exit(&host, &mut state, authority, TaskIds { host: 4, sdk: 3 })
            .unwrap();
        let requests = host.requests.borrow();
        assert_eq!(
            requests
                .iter()
                .map(|(r, _)| r.command())
                .collect::<Vec<_>>(),
            [7994, 10342, 9332]
        );
        let common = CommonRequest::decode(&requests[2].1).unwrap();
        let body = fields(common.body.as_deref().unwrap()).unwrap();
        assert_eq!(number(&body, 2, None).unwrap(), 123);
        assert_eq!(data(&body, 5).unwrap(), b"voip-mt");
        let pending = repeated(&body, 4, 100).unwrap();
        assert_eq!(
            pending,
            [b"peer".as_slice(), b"peer".as_slice(), b"later".as_slice()]
        );
    }

    #[test]
    fn unsupported_creation_preserves_receipt_and_known_membership_for_cleanup() {
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::Invite, creation_reply(1, true)),
                (CallHostRoute::Transfer, super::super::tests::exit_reply()),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = session(&host);
        assert!(session
            .create(
                &host,
                &mut state,
                authority,
                ConferenceCreateTarget {
                    group_id: "test@chatroom",
                    invitees: &["peer".into()],
                    invite_id: 123
                },
                TaskIds { host: 2, sdk: 1 }
            )
            .is_err());
        assert_eq!(
            session.creation_response().unwrap().custom_auth.as_deref(),
            Some(b"unsupported-auth".as_slice())
        );
        assert!(session.matches_room(7, 9));
        assert!(session
            .invite(
                &host,
                &mut state,
                authority,
                &["other".into()],
                456,
                TaskIds { host: 3, sdk: 2 }
            )
            .is_err());
        assert!(session
            .create(
                &host,
                &mut state,
                authority,
                ConferenceCreateTarget {
                    group_id: "test@chatroom",
                    invitees: &["peer".into()],
                    invite_id: 124
                },
                TaskIds { host: 3, sdk: 2 }
            )
            .is_err());
        session
            .exit(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .unwrap();
        assert_eq!(host.requests.borrow().len(), 3);
    }
    #[test]
    fn added_invitations_preserve_partial_receipts_without_creating_membership() {
        let result = |name: &[u8], code| {
            p::bytes(
                4,
                &p::message(&[p::bytes(1, name), p::uint(2, code)]).unwrap(),
            )
        };
        let body = p::message(&[
            p::uint(1, 7),
            p::uint(2, 9),
            p::uint(3, 0),
            result(b"new-a", 0),
            result(b"new-b", 5),
            p::bytes(5, b"https://verification.invalid/test"),
            p::uint(99, 8),
        ])
        .unwrap();
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(false)),
                (
                    CallHostRoute::ConferenceAddMembers,
                    super::super::tests::host_reply(&body),
                ),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = joined(&host);
        let result = session
            .invite(
                &host,
                &mut state,
                authority,
                &["new-a".into(), "new-b".into()],
                456,
                TaskIds { host: 3, sdk: 2 },
            )
            .unwrap();
        assert_eq!(result.original, body);
        assert_eq!(result.invite_results[0].service_code, Some(0));
        assert_eq!(result.invite_results[1].service_code, Some(5));
        assert_eq!(
            result.verification_url.as_deref(),
            Some("https://verification.invalid/test")
        );
        assert_eq!(result.unknown_fields.len(), 1);
        assert_eq!(session.members().len(), 3);
        assert!(session
            .members()
            .iter()
            .all(|member| !member.username.starts_with("new-")));
        assert!(session
            .invite(
                &host,
                &mut state,
                authority,
                &["new-a".into()],
                456,
                TaskIds { host: 4, sdk: 3 },
            )
            .is_err());
        let requests = host.requests.borrow();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[2].0.command(), 10857);
        let common = CommonRequest::decode(&requests[2].1).unwrap();
        let body = fields(common.body.as_deref().unwrap()).unwrap();
        assert_eq!(number(&body, 4, None).unwrap(), 456);
        assert_eq!(data(&body, 5).unwrap(), b"test@chatroom");
        assert_eq!(data(&body, 7).unwrap(), b"voip-mt");
    }
    #[test]
    fn ambiguous_addition_blocks_new_intents_and_never_changes_the_current_room() {
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(false)),
                (
                    CallHostRoute::ConferenceAddMembers,
                    super::super::tests::host_reply(&p::uint(1, 8)),
                ),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = joined(&host);
        assert!(session
            .invite(
                &host,
                &mut state,
                authority,
                &["new-a".into()],
                456,
                TaskIds { host: 3, sdk: 2 },
            )
            .is_err());
        assert!(session
            .invite(
                &host,
                &mut state,
                authority,
                &["new-b".into()],
                457,
                TaskIds { host: 4, sdk: 3 },
            )
            .is_err());
        assert!(session.matches_room(7, 9));
        assert_eq!(session.members().len(), 3);
        assert_eq!(host.requests.borrow().len(), 3);
    }
    #[test]
    fn pending_entry_cannot_send_additional_invitations() {
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(true)),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = joined(&host);
        assert!(session
            .invite(
                &host,
                &mut state,
                authority,
                &["new-a".into()],
                456,
                TaskIds { host: 3, sdk: 2 },
            )
            .is_err());
        assert_eq!(host.requests.borrow().len(), 2);
    }
    #[test]
    fn conference_microphone_snapshot_uses_newer_sequence_and_keeps_membership_separate() {
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(false)),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = session(&host);
        session
            .join(
                &host,
                &mut state,
                authority,
                ConferenceTarget {
                    group_id: "test@chatroom",
                    cloud_group_id: "opaque-context",
                    join_id: 123,
                },
                TaskIds { host: 2, sdk: 1 },
            )
            .unwrap();
        let snapshot = |sequence, entries: &[(u64, u64)]| {
            let mut parts = vec![p::uint(3, sequence)];
            parts.extend(entries.iter().map(|(id, flags)| {
                p::bytes(
                    4,
                    &p::message(&[p::uint(1, *id), p::uint(2, *flags)]).unwrap(),
                )
            }));
            p::message(&parts).unwrap()
        };
        assert!(session
            .members()
            .iter()
            .all(|member| member.microphone_enabled.is_none()));
        install_av(
            session.room.as_mut().unwrap(),
            &fields(&snapshot(2, &[(1, 1), (2, 0)])).unwrap(),
        )
        .unwrap();
        let members = session.members();
        assert_eq!(members[1].microphone_enabled, Some(true));
        assert_eq!(members[2].microphone_enabled, Some(false));
        assert!(members.iter().all(|member| member.status == 1));
        install_av(
            session.room.as_mut().unwrap(),
            &fields(&snapshot(1, &[(1, 0)])).unwrap(),
        )
        .unwrap();
        assert_eq!(session.members()[1].microphone_enabled, Some(true));
        assert!(install_av(
            session.room.as_mut().unwrap(),
            &fields(&snapshot(3, &[(1, 0), (1, 1)])).unwrap()
        )
        .is_err());
        assert_eq!(session.room.as_ref().unwrap().av_sequence, 2);
        install_av(
            session.room.as_mut().unwrap(),
            &fields(&snapshot(3, &[(1, 0), (2, 1)])).unwrap(),
        )
        .unwrap();
        assert_eq!(session.members()[1].microphone_enabled, Some(false));
        assert_eq!(session.members()[2].microphone_enabled, Some(true));
        let replacement = p::message(&[
            p::uint(1, 2),
            p::bytes(2, &super::super::tests::member("replacement", 1, 1)),
            p::bytes(2, &super::super::tests::member("third", 2, 1)),
        ])
        .unwrap();
        install_roster(
            session.room.as_mut().unwrap(),
            &fields(&replacement).unwrap(),
            1,
            2,
        )
        .unwrap();
        assert_eq!(session.members()[0].microphone_enabled, None);
        assert_eq!(session.members()[1].microphone_enabled, Some(true));
    }
    #[test]
    fn unknown_control_payloads_stay_opaque_and_do_not_end_the_conference() {
        let av = p::message(&[
            p::uint(3, 1),
            p::bytes(4, &p::message(&[p::uint(1, 1), p::uint(2, 0)]).unwrap()),
        ])
        .unwrap();
        let notify = p::message(&[
            p::uint(3, 7),
            p::uint(4, 9),
            // Unknown control bytes need not be a protobuf understood by us.
            p::bytes(
                5,
                &p::message(&[p::uint(1, 999), p::bytes(2, &[0xff])]).unwrap(),
            ),
            p::bytes(5, &p::message(&[p::uint(1, 12), p::bytes(2, &av)]).unwrap()),
        ])
        .unwrap();
        let command = p::message(&[p::uint(1, 10002), p::bytes(2, &notify)]).unwrap();
        let response = super::super::tests::host_reply(
            &p::message(&[
                p::bytes(
                    1,
                    &p::message(&[p::uint(1, 77), p::bytes(2, &[0xff])]).unwrap(),
                ),
                p::bytes(
                    1,
                    &p::message(&[p::uint(1, 1), p::bytes(2, &command)]).unwrap(),
                ),
                p::bytes(2, b"accepted-next"),
                p::uint(99, 1),
            ])
            .unwrap(),
        );
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(false)),
                (CallHostRoute::LiveSync, response),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = joined(&host);
        assert!(session
            .poll(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .unwrap());
        assert_eq!(session.ignored_control_count(), 3);
        assert!(session.has_room() && session.own_membership_ready());
        assert_eq!(session.members()[1].microphone_enabled, Some(false));
        assert_eq!(session.room.as_ref().unwrap().cookies, b"accepted-next");
        assert_eq!(host.requests.borrow().len(), 3);
    }

    #[test]
    fn microphone_submission_is_context_bound_and_never_reuses_a_timestamp() {
        let av = p::message(&[
            p::uint(3, 2),
            p::bytes(4, &p::message(&[p::uint(1, 0), p::uint(2, 0)]).unwrap()),
        ])
        .unwrap();
        let result = p::message(&[
            p::uint(1, 7),
            p::uint(2, 9),
            p::uint(3, 0),
            p::bytes(4, &av),
        ])
        .unwrap();
        let cloud =
            p::message(&[p::bytes(1, &[8, 0]), p::uint(2, 17), p::bytes(3, &result)]).unwrap();
        let frame = app_frame::TaskFrame {
            command: 3197,
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
            payload: cloud,
        }
        .encode()
        .unwrap();
        let response = super::super::tests::host_reply(
            &p::message(&[p::uint(1, 0), p::bytes(2, &frame)]).unwrap(),
        );
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(false)),
                (CallHostRoute::Transfer, response),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = joined(&host);
        session
            .set_muted(
                &host,
                &mut state,
                authority,
                true,
                100,
                TaskIds { host: 3, sdk: 2 },
            )
            .unwrap();
        assert_eq!(session.members()[0].microphone_enabled, Some(false));
        assert!(session
            .set_muted(
                &host,
                &mut state,
                authority,
                false,
                100,
                TaskIds { host: 4, sdk: 3 }
            )
            .is_err());
        assert!(session.has_room());
        assert_eq!(host.requests.borrow().len(), 3);
    }

    #[test]
    fn confirmed_own_removal_completes_leave_locally_without_an_exit_rpc() {
        let roster = p::message(&[
            p::uint(1, 7),
            p::uint(2, 9),
            p::uint(3, 3),
            p::bytes(4, &super::super::tests::member("third", 2, 1)),
        ])
        .unwrap();
        let notify = p::message(&[
            p::uint(3, 7),
            p::uint(4, 9),
            p::bytes(
                5,
                &p::message(&[p::uint(1, 10), p::bytes(2, &roster)]).unwrap(),
            ),
        ])
        .unwrap();
        let command = p::message(&[p::uint(1, 10002), p::bytes(2, &notify)]).unwrap();
        let reply = super::super::tests::host_reply(
            &p::message(&[
                p::bytes(
                    1,
                    &p::message(&[p::uint(1, 1), p::bytes(2, &command)]).unwrap(),
                ),
                p::bytes(2, b"left"),
            ])
            .unwrap(),
        );
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(false)),
                (CallHostRoute::LiveSync, reply),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = joined(&host);
        assert!(!session
            .poll(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .unwrap());
        assert!(!session.has_room());
        session
            .exit(&host, &mut state, authority, TaskIds { host: 4, sdk: 3 })
            .unwrap();
        session
            .exit(&host, &mut state, authority, TaskIds { host: 5, sdk: 4 })
            .unwrap();
        assert!(session.exit_confirmed);
        assert_eq!(host.requests.borrow().len(), 3);
    }

    #[test]
    fn lost_exit_response_is_uncertain_and_never_retried() {
        struct LostExit {
            host: Host,
            attempts: std::cell::Cell<usize>,
        }
        impl PersonalTransport for LostExit {
            fn call_rpc(
                &self,
                state: &mut AccountState,
                route: CallHostRoute,
                request: &[u8],
            ) -> Result<Vec<u8>> {
                if route == CallHostRoute::Transfer {
                    self.attempts.set(self.attempts.get() + 1);
                    bail!("Reply lost");
                }
                self.host.call_rpc(state, route, request)
            }
        }
        let host = LostExit {
            host: Host {
                replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                    (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                    (CallHostRoute::ConferenceJoin, join_reply(false)),
                ])),
                requests: Default::default(),
            },
            attempts: std::cell::Cell::new(0),
        };
        let mut state = super::super::tests::state();
        let authority = super::super::tests::authority();
        let mut session =
            ConferenceSession::authenticate(&host, &mut state, authority, 1, [5; 32], parameters())
                .unwrap();
        session
            .join(
                &host,
                &mut state,
                authority,
                ConferenceTarget {
                    group_id: "test@chatroom",
                    cloud_group_id: "opaque-context",
                    join_id: 123,
                },
                TaskIds { host: 2, sdk: 1 },
            )
            .unwrap();
        assert!(session
            .exit(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .is_err());
        assert!(session
            .exit(&host, &mut state, authority, TaskIds { host: 4, sdk: 3 })
            .is_err());
        assert!(!session.exit_confirmed);
        assert_eq!(host.attempts.get(), 1);
    }

    #[test]
    fn synchronous_conference_entry_reads_three_members_and_exits_once_without_cancel() {
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(false)),
                (CallHostRoute::Transfer, super::super::tests::exit_reply()),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = session(&host);
        session
            .join(
                &host,
                &mut state,
                authority,
                ConferenceTarget {
                    group_id: "test@chatroom",
                    cloud_group_id: "opaque-context",
                    join_id: 123,
                },
                TaskIds { host: 2, sdk: 1 },
            )
            .unwrap();
        assert!(session.matches_room(7, 9));
        assert!(session.own_membership_ready());
        assert_eq!(session.members().len(), 3);
        assert_eq!(
            session.media_profile().unwrap(),
            [17, 0x206, 0x800000, 0, 2, 3]
        );
        session
            .exit(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .unwrap();
        assert!(!session.has_room());
        session
            .exit(&host, &mut state, authority, TaskIds { host: 4, sdk: 3 })
            .unwrap();
        let requests = host.requests.borrow();
        assert_eq!(
            requests
                .iter()
                .map(|(route, _)| route.command())
                .collect::<Vec<_>>(),
            [7994, 14754, 11421]
        );
        assert!(wechat_protocol::calls::host_policy::is_teardown_request(
            CallHostRoute::Transfer,
            &requests[2].1
        )
        .unwrap());
        assert_eq!(super::super::tests::recorded_exit_reason(&requests[2].1), 1);
    }
    #[test]
    fn asynchronous_entry_waits_for_its_bound_completion_before_installing_profile() {
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(true)),
                (CallHostRoute::LiveSync, async_reply(7)),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = session(&host);
        session
            .join(
                &host,
                &mut state,
                authority,
                ConferenceTarget {
                    group_id: "test@chatroom",
                    cloud_group_id: "opaque-context",
                    join_id: 123,
                },
                TaskIds { host: 2, sdk: 1 },
            )
            .unwrap();
        assert!(session.join_pending());
        assert!(!session.own_membership_ready());
        session
            .poll(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .unwrap();
        assert!(!session.join_pending());
        assert!(session.own_membership_ready());
        assert_eq!(session.members().len(), 3);
        assert_eq!(session.room.as_ref().unwrap().cookies, b"next");
    }
    #[test]
    fn foreign_async_room_preserves_pending_context_and_cursor() {
        let host = Host {
            replies: std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, super::super::tests::auth_reply()),
                (CallHostRoute::ConferenceJoin, join_reply(true)),
                (CallHostRoute::LiveSync, async_reply(8)),
            ])),
            requests: Default::default(),
        };
        let (mut session, mut state, authority) = session(&host);
        session
            .join(
                &host,
                &mut state,
                authority,
                ConferenceTarget {
                    group_id: "test@chatroom",
                    cloud_group_id: "opaque-context",
                    join_id: 123,
                },
                TaskIds { host: 2, sdk: 1 },
            )
            .unwrap();
        assert!(session
            .poll(&host, &mut state, authority, TaskIds { host: 3, sdk: 2 })
            .is_err());
        assert!(session.join_pending());
        assert!(session.matches_room(7, 9));
        assert_eq!(session.room.as_ref().unwrap().cookies, b"initial");
        assert!(session.members().is_empty());
    }

    #[test]
    fn join_keeps_chat_and_conference_identities_distinct() {
        let parameters = parameters();
        let target = ConferenceTarget {
            group_id: "test@chatroom",
            cloud_group_id: "synthetic-conference",
            join_id: 123,
        };
        let request = join_request(&parameters, &target).unwrap();
        let wrapper = fields(&request).unwrap();
        // Separate host fields, no personal invitation or custom-auth request.
        assert_eq!(data(&wrapper, 1).unwrap(), b"test@chatroom");
        assert_eq!(data(&wrapper, 2).unwrap(), b"synthetic-conference");
        assert_eq!(number(&wrapper, 4, None).unwrap(), 123);
        assert!(p::optional(&wrapper, 5).unwrap().is_none());
        assert_eq!(data(&wrapper, 6).unwrap(), b"voip-mt");
        assert_eq!(CallHostRoute::ConferenceJoin.command(), 14754);
        assert_eq!(
            CallHostRoute::ConferenceJoin.path(),
            "/cgi-bin/micromsg-bin/voipilinkjoin"
        );
        let client = fields(data(&wrapper, 3).unwrap()).unwrap();
        assert_eq!(number(&client, 1, None).unwrap(), 0x050000cb);
        assert_eq!(data(&client, 2).unwrap(), b"test");
        assert_eq!(number(&client, 4, None).unwrap(), 1);
        let caps = fields(data(&client, 5).unwrap()).unwrap();
        assert_eq!(data(&caps, 1).unwrap(), parameters.engine_capability);
        assert_eq!(data(&caps, 2).unwrap(), parameters.channel_capability);
        assert!(data(&client, 6).unwrap().is_empty());
        assert_eq!(number(&client, 7, None).unwrap(), 0);
        assert_eq!(number(&client, 8, None).unwrap(), 0);
        assert!(p::optional(&client, 9).unwrap().is_none());
        assert_eq!(number(&client, 10, None).unwrap(), 0);
        assert!(data(&client, 11).unwrap().is_empty());
    }

    #[test]
    fn chat_id_alone_cannot_submit_a_conference_join() {
        let parameters = parameters();
        for target in [
            ConferenceTarget {
                group_id: "test@chatroom",
                cloud_group_id: "",
                join_id: 123,
            },
            ConferenceTarget {
                group_id: "test",
                cloud_group_id: "synthetic-conference",
                join_id: 123,
            },
            ConferenceTarget {
                group_id: "test@chatroom",
                cloud_group_id: "synthetic-conference",
                join_id: 0,
            },
        ] {
            assert!(join_request(&parameters, &target).is_err());
        }
    }
}
