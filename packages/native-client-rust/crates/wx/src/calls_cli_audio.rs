//! Interactive local devices. No stream is opened until dial or an accepted prompt.
use super::*;
use anyhow::ensure;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use std::{
    collections::VecDeque,
    io::IsTerminal,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines, Stdin},
    sync::mpsc,
};
use wechat_audio::{AudioConfig, AudioError, AudioSession, PlaybackHandle, PlaybackToken};
use wechat_calls::audio::AudioFormat;
use wechat_core::call_types::{
    CallConferenceInvitation, CallMediaCommand, CallReadyScope, CallState, CallTarget,
    CallTrackDirection,
};

type Input = Lines<BufReader<Stdin>>;
const CALL_KEYS: &str = "m + Enter  Microphone   s + Enter  Speaker   i + Enter  Invite\n\
                        p + Enter  Participants d + Enter  Devices   l/h + Enter  Leave\n\
                        Ctrl-C     Leave        ? + Enter  Help";

pub(super) async fn run(remote: &Remote, command: CallsCommand, json: bool) -> Result<()> {
    if json {
        bail!("use calls monitor --json for passive observation; dial and answer are interactive audio sessions");
    }
    if !std::io::stdin().is_terminal() {
        bail!("interactive calls require a terminal; use calls monitor --json to observe without devices");
    }
    let (cancellation, _signals) = Cancellation::listen();
    let (mut control, _) = tokio::select! {
        _=cancellation.wait()=>return Err(failure(130,"client_interrupted","call connection cancelled")),
        result=Control::connect(remote)=>result?,
    };
    control.cancellation = Some(cancellation.clone());
    if !control.capabilities.direct_audio && !control.capabilities.conference_audio {
        bail!("voice calling is unavailable on this daemon");
    }
    if control.format != CallAudioFormat::default() {
        bail!("unsupported negotiated call audio format");
    }
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let result = match command {
        CallsCommand::Monitor { audio } => {
            monitor(remote, &mut control, &audio, &mut input, &cancellation).await
        }
        CallsCommand::Dial { contact, audio } => {
            ensure!(
                control.capabilities.direct_audio,
                "direct calling unavailable on this daemon"
            );
            if control.availability.busy {
                bail!("busy: the daemon already has an ongoing or unresolved call");
            }
            participate(
                remote,
                &mut control,
                &audio,
                Participation::Dial(contact),
                &mut input,
                &cancellation,
            )
            .await
        }
        CallsCommand::Answer { call_id, audio } => {
            let offer = pin_answer(&control, call_id.as_deref())?;
            participate(
                remote,
                &mut control,
                &audio,
                Participation::Answer(offer),
                &mut input,
                &cancellation,
            )
            .await
        }
        CallsCommand::Conference {
            command: Some(command),
        } => {
            if control.availability.busy {
                bail!("busy: the account already has an ongoing or unresolved call");
            }
            let (action, options) = match command {
                ConferenceCommand::Start {
                    group,
                    invitees,
                    audio,
                } => {
                    ensure!(
                        control.capabilities.conference_create
                            && control.capabilities.conference_audio,
                        "conference creation/audio unavailable on this daemon"
                    );
                    (Participation::ConferenceStart { group, invitees }, audio)
                }
                ConferenceCommand::Join { group, audio } => {
                    ensure!(
                        control.capabilities.conference_join
                            && control.capabilities.conference_audio,
                        "conference joining/audio unavailable on this daemon"
                    );
                    let invitation = conference_invitation(&control, &group)?;
                    (Participation::ConferenceJoin(invitation), audio)
                }
            };
            participate(
                remote,
                &mut control,
                &options,
                action,
                &mut input,
                &cancellation,
            )
            .await
        }
        CallsCommand::Conference { command: None }
        | CallsCommand::Status
        | CallsCommand::Devices => {
            unreachable!("status is handled without devices")
        }
    };
    control.close().await;
    result
}

fn exact_offer(offer: Option<&CallOffer>, requested: Option<&str>) -> Result<CallOffer> {
    let offer = offer.context("no current incoming call; answer never waits for a new caller")?;
    if requested.is_some_and(|id| id != offer.call_id) {
        bail!("offer_expired: that call is no longer offered");
    }
    Ok(offer.clone())
}

fn conference_invitation(control: &Control, group: &str) -> Result<CallConferenceInvitation> {
    let mut matches = control
        .conference_invitations
        .iter()
        .filter(|item| item.group_id == group);
    let selected = matches
        .next()
        .context("no current invitation for this group; join never creates a room")?
        .clone();
    ensure!(
        matches.next().is_none(),
        "more than one invitation for this group; wait for a current unambiguous invitation"
    );
    Ok(selected)
}

fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
async fn ready(control: &mut Control, enabled: bool) -> Result<()> {
    control
        .command(CallCommand::Ready {
            request_id: request_id(),
            ready: enabled,
            format: control.format,
            scope: Some(CallReadyScope::All),
        })
        .await?;
    Ok(())
}

async fn monitor(
    remote: &Remote,
    control: &mut Control,
    options: &AudioOptions,
    input: &mut Input,
    cancellation: &Cancellation,
) -> Result<()> {
    // Readiness declares this interactive client's ability to prepare devices;
    // it does not open or continuously capture its microphone while waiting.
    ready(control, true).await?;
    eprintln!("Waiting for calls. Ctrl-C exits.");
    let mut shown = None;
    let mut dismissed = None;
    let mut renew_after_terminal = false;
    let mut shown_invitations = Vec::new();
    loop {
        let invitations = control
            .conference_invitations
            .iter()
            .map(|i| (i.invitation_id.clone(), i.revision))
            .collect::<Vec<_>>();
        if invitations != shown_invitations {
            shown_invitations = invitations;
            for (index, invitation) in control.conference_invitations.iter().enumerate() {
                eprintln!(
                    "Conference invitation {}: {} from {}. Type j {} then Enter to join.",
                    index + 1,
                    super::levels::clean_label(&invitation.group_id, 64),
                    super::levels::clean_label(&invitation.caller_id, 64),
                    index + 1
                );
            }
        }
        if let Some(offer) = &control.offer {
            let identity = (offer.call_id.clone(), offer.revision);
            if shown.as_ref() != Some(&identity) && dismissed.as_ref() != Some(&identity) {
                eprintln!("Incoming voice call from {} ({})\n[a] Accept  [d] Dismiss here  [r] Reject call", offer.caller_id, offer.call_id);
                shown = Some(identity);
            }
        }
        tokio::select! {
            _ = cancellation.wait() => return Ok(()),
            event = control.next_event() => {
                let event=event?; control.update(&event);
                if renew_after_terminal&&matches!(event,CallEvent::Availability {availability} if !availability.busy) {
                    renew_after_terminal=false;restore_monitor_readiness(control).await?;
                }
            }
            line = input.next_line() => {
                let Some(line)=line? else { return Ok(()); };
                if line.trim()=="j" || line.trim().starts_with("j ") {
                    let selected=line.trim().strip_prefix("j ").unwrap_or("1").parse::<usize>().ok().and_then(|index|index.checked_sub(1)).and_then(|index|control.conference_invitations.get(index)).cloned();
                    if let Some(invitation)=selected {
                        if !control.capabilities.conference_join || !control.capabilities.conference_audio {eprintln!("Conference joining/audio unavailable.");continue;}
                        if let Err(error)=participate(remote,control,options,Participation::ConferenceJoin(invitation),input,cancellation).await {eprintln!("{error}");}
                        if cancellation.is_cancelled(){return Ok(());}
                        restore_monitor_readiness(control).await?;shown_invitations.clear();eprintln!("Waiting for calls. Ctrl-C exits.");
                    }else{eprintln!("No matching current conference invitation.");}
                    continue;
                }
                let Some(offer)=control.offer.clone() else { continue; };
                match line.trim().to_ascii_lowercase().as_str() {
                    "a" | "accept" => {
                        renew_after_terminal=true;
                        let result=participate(remote, control, options, Participation::Answer(offer.into()), input,cancellation).await;
                        let completed=result.is_ok();
                        if let Err(error)=result {
                            if cancellation.is_cancelled() {return Err(error);}
                            eprintln!("{error}");
                        }
                        if cancellation.is_cancelled() {return Ok(());}
                        if completed {restore_monitor_readiness(control).await?;renew_after_terminal=false;}
                        shown=None;
                        eprintln!("Waiting for calls. Ctrl-C exits.");
                    }
                    "d" | "dismiss" => { dismissed=Some((offer.call_id,offer.revision)); }
                    "r" | "reject" => {
                        let result=control.command(CallCommand::Reject {request_id:request_id(), call_id:offer.call_id, expected_revision:offer.revision}).await;
                        if let Err(error)=result { eprintln!("{error}"); }
                    }
                    _ => eprintln!("Choose a, d or r; Ctrl-C exits."),
                }
            }
        }
    }
}

pub(super) async fn restore_monitor_readiness(control: &mut Control) -> Result<()> {
    ready(control, true).await
}

enum Participation {
    Dial(String),
    ConferenceStart {
        group: String,
        invitees: Vec<String>,
    },
    ConferenceJoin(CallConferenceInvitation),
    Answer(AnswerTarget),
}

struct AnswerTarget {
    call_id: String,
    revision: u32,
}
impl From<CallOffer> for AnswerTarget {
    fn from(offer: CallOffer) -> Self {
        Self {
            call_id: offer.call_id,
            revision: offer.revision,
        }
    }
}
fn pin_answer(control: &Control, requested: Option<&str>) -> Result<AnswerTarget> {
    if let Some(offer) = control
        .offer
        .as_ref()
        .filter(|offer| requested.is_none_or(|id| id == offer.call_id))
    {
        return Ok(exact_offer(Some(offer), requested)?.into());
    }
    if let Some(offer) = control
        .pending_offer
        .as_ref()
        .filter(|offer| requested.is_none_or(|id| id == offer.call_id))
    {
        return Ok(AnswerTarget {
            call_id: offer.call_id.clone(),
            revision: offer.revision,
        });
    }
    bail!("offer_expired: no matching current call; answer never waits for a new caller")
}
async fn await_exact_offer(
    control: &mut Control,
    target: &AnswerTarget,
    cancellation: &Cancellation,
) -> Result<CallOffer> {
    timeout(Duration::from_secs(30),async {
        loop {
            if let Some(offer)=control.offer.as_ref().filter(|offer|offer.call_id==target.call_id) {return Ok(offer.clone());}
            if control.pending_offer.as_ref().is_none_or(|offer|offer.call_id!=target.call_id) {
                bail!("offer_expired: the selected call ended or was replaced");
            }
            let event=tokio::select! {
                _=cancellation.wait()=>return Err(failure(130,"client_interrupted","call preparation cancelled before answer")),
                event=control.next_event()=>event?,
            };
            control.update(&event);
            if let CallEvent::Error {code}=event {bail!("call preparation failed: {}",serde_json::to_value(code)?);}
        }
    }).await.context("the selected call did not become ready; no answer was submitted")?
}

async fn participate(
    remote: &Remote,
    control: &mut Control,
    options: &AudioOptions,
    action: Participation,
    input: &mut Input,
    cancellation: &Cancellation,
) -> Result<()> {
    // For late answers the exact ID/revision was captured before device startup.
    // The server revalidates it atomically; failure never targets the next offer.
    let mut audio = tokio::select! {
        _=cancellation.wait()=>return Err(failure(130,"client_interrupted","device preparation cancelled; no call was placed or answered")),
        audio=LocalAudio::open(options)=>audio?,
    };
    let scope = match &action {
        Participation::Dial(_)
        | Participation::ConferenceStart { .. }
        | Participation::ConferenceJoin(_) => CallReadyScope::Outgoing,
        Participation::Answer(target) => CallReadyScope::Offer {
            call_id: target.call_id.clone(),
            expected_revision: target.revision,
        },
    };
    control
        .command(CallCommand::Ready {
            request_id: request_id(),
            ready: true,
            format: control.format,
            scope: Some(scope),
        })
        .await?;
    let command = match action {
        Participation::Dial(contact_id) => CallCommand::Dial {
            request_id: request_id(),
            target: CallTarget::Direct { contact_id },
            call_type: wechat_core::call_types::CallType::Voice,
        },
        Participation::ConferenceStart { group, invitees } => CallCommand::Dial {
            request_id: request_id(),
            target: CallTarget::Conference {
                group_id: group,
                invitees,
            },
            call_type: wechat_core::call_types::CallType::Voice,
        },
        Participation::ConferenceJoin(invitation) => CallCommand::Join {
            request_id: request_id(),
            invitation_id: invitation.invitation_id,
            invitation_revision: invitation.revision,
        },
        Participation::Answer(target) => {
            let offer = await_exact_offer(control, &target, cancellation).await?;
            CallCommand::Answer {
                request_id: request_id(),
                call_id: offer.call_id,
                expected_revision: offer.revision,
            }
        }
    };
    let claim = control.command(command).await?;
    let call_id = claim
        .call_id
        .context("call claim did not return a call ID")?;
    let owner_token = claim
        .owner_token
        .context("call claim did not grant media ownership")?;
    let mut call = OwnedCall {
        call_id,
        owner_token,
        media: None,
    };
    let outcome = owned_call(remote, control, &mut call, &mut audio, input, cancellation).await;
    if let Err(error) = &outcome {
        debug_call(
            "session",
            format_args!("call {0} failed: {error:#}", call.call_id),
        );
    }
    audio.request_stop();
    // Any local failure requests termination; a lost connection or response is
    // reported as uncertain. An accepted command alone never proves remote end.
    // Teardown is issued once even when the user signal remains latched. Its
    // existing eight-second limit still bounds an unresponsive remote operation.
    let ended = call.finish(control, matches!(&outcome, Ok(true))).await;
    let stopped = audio.stop().await;
    ended?;
    stopped?;
    outcome.map(|_| ())
}

pub(super) struct OwnedCall {
    pub(super) call_id: String,
    pub(super) owner_token: String,
    pub(super) media: Option<Media>,
}
impl OwnedCall {
    pub(super) async fn finish(&mut self, control: &mut Control, terminal: bool) -> Result<()> {
        // Dropping a live media attachment revokes its control owner. Retain it
        // until explicit end reaches terminal evidence or the bounded attempt
        // reports uncertainty. Capture and playback are stopped by the caller.
        let result = if terminal {
            Ok(())
        } else {
            terminate_owned(control, &self.call_id).await
        };
        if let Some(media) = self.media.take() {
            debug_call(
                "media",
                format_args!(
                    "local socket drop after {} completed PCM writes",
                    media.pcm_writes
                ),
            );
        }
        result
    }
}

pub(super) struct Media {
    socket: Socket,
    send_track: wechat_core::call_types::CallAudioTrack,
    receive: super::receive_audio::ReceiveAudio,
    attached: Instant,
    started_generation: Option<u32>,
    pcm_writes: u64,
}
impl Media {
    pub(super) async fn attach(
        remote: &Remote,
        control: &Control,
        call_id: &str,
        owner_token: &str,
    ) -> Result<Self> {
        let suffix = format!("calls/{}/media", super::super::encode(call_id));
        let mut socket = open_socket(remote, &suffix).await?;
        send_json(
            &mut socket,
            &CallMediaCommand::Attach {
                supported_protocol_versions: vec![wechat_core::call_types::CALL_PROTOCOL_VERSION],
                client_id: control.client_id.clone(),
                owner_token: owner_token.into(),
            },
        )
        .await?;
        let event = timeout(STARTUP_TIMEOUT, receive_event(&mut socket, "media"))
            .await
            .context("media attachment timed out")??;
        if let CallEvent::Error { code } = &event {
            bail!("media attachment rejected: {}", serde_json::to_value(code)?);
        }
        let CallEvent::MediaReady {
            protocol_version,
            send_track,
            receive_tracks,
        } = event
        else {
            bail!("media attachment was not accepted");
        };
        ensure!(
            protocol_version == wechat_core::call_types::CALL_PROTOCOL_VERSION,
            "unsupported call media version"
        );
        ensure!(
            send_track.format == control.format
                && send_track.direction == CallTrackDirection::Send
                && send_track.id != 0
                && send_track.generation != 0,
            "unsupported send audio track"
        );
        ensure!(
            receive_tracks.iter().all(|track| track.id != send_track.id),
            "overlapping send and receive tracks"
        );
        let format = AudioFormat::new(
            control.format.sample_rate,
            control.format.channels,
            control.format.frame_ms,
        )?;
        let receive = super::receive_audio::ReceiveAudio::new(format, receive_tracks)?;
        debug_call(
            "media",
            format_args!(
                "media.ready call={call_id} generation={}",
                send_track.generation
            ),
        );
        Ok(Self {
            socket,
            receive,
            attached: Instant::now(),
            send_track,
            started_generation: None,
            pcm_writes: 0,
        })
    }
    pub(super) fn synchronize_tracks(
        &mut self,
        control: &Control,
        call_id: &str,
    ) -> Result<(bool, bool)> {
        let mut send_changed = false;
        let receive_changed = false;
        for ((id, _), track) in &control.tracks {
            if id != call_id {
                continue;
            }
            if track.format != control.format {
                bail!("unsupported updated audio format");
            }
            match track.direction {
                CallTrackDirection::Send
                    if track.id == self.send_track.id
                        && track.generation > self.send_track.generation =>
                {
                    self.send_track = track.clone();
                    send_changed = true;
                }
                _ => {}
            }
        }
        Ok((send_changed, receive_changed))
    }
    pub(super) async fn send(&mut self, samples: &[i16], format: AudioFormat) -> Result<()> {
        format.validate_frame(samples)?;
        let generation = self.send_track.generation;
        let first = self.started_generation != Some(generation);
        if first {
            debug_call(
                "media",
                format_args!("audio.start write attempted generation={generation}"),
            );
            if let Err(error) = send_json(
                &mut self.socket,
                &CallMediaCommand::AudioStart {
                    track_id: self.send_track.id,
                    generation,
                },
            )
            .await
            {
                debug_call("media", format_args!("audio.start write failed: {error:#}"));
                return Err(error);
            }
            debug_call(
                "media",
                format_args!("audio.start write completed generation={generation}"),
            );
            self.started_generation = Some(generation);
        }
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        if first {
            debug_call(
                "media",
                format_args!(
                    "first PCM write attempted generation={generation} bytes={}",
                    bytes.len()
                ),
            );
        }
        if let Err(error) = timeout(
            WRITE_TIMEOUT,
            self.socket.send(Message::Text(
                serde_json::to_string(&CallMediaCommand::AudioChunk {
                    track_id: self.send_track.id,
                    generation,
                    sequence: None,
                    sample_offset: None,
                    pcm_base64: BASE64.encode(&bytes),
                })?
                .into(),
            )),
        )
        .await
        .context("call media write timed out")
        .and_then(|result| result.map_err(Into::into))
        {
            debug_call(
                "media",
                format_args!(
                    "PCM write failed after {} completed writes: {error:#}",
                    self.pcm_writes
                ),
            );
            return Err(error);
        }
        self.pcm_writes += 1;
        if first {
            debug_call(
                "media",
                format_args!("first PCM write completed generation={generation}"),
            );
        }
        Ok(())
    }
}

async fn next_media(media: &mut Option<Media>) -> Result<Message> {
    match media {
        Some(media) => receive(&mut media.socket, "media").await,
        None => std::future::pending().await,
    }
}

async fn owned_call(
    remote: &Remote,
    control: &mut Control,
    call: &mut OwnedCall,
    audio: &mut LocalAudio,
    input: &mut Input,
    cancellation: &Cancellation,
) -> Result<bool> {
    let call_id = call.call_id.as_str();
    let owner_token = call.owner_token.as_str();
    let format = AudioFormat::new(
        control.format.sample_rate,
        control.format.channels,
        control.format.frame_ms,
    )?;
    let media = &mut call.media;
    let mut muted = false;
    let mut speaker_enabled = true;
    let mut pending = std::collections::BTreeMap::<String, (Instant, Option<bool>)>::new();
    let mut interaction = super::interaction::Interaction::new();
    let mut device_change = DeviceChange(None);
    let mut tick = tokio::time::interval(Duration::from_millis(u64::from(control.format.frame_ms)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    eprintln!(
        "Call {}\n{CALL_KEYS}",
        super::levels::clean_label(call_id, 80)
    );
    let mut levels = super::levels::CallLevels::new();
    let mut meter_tick = tokio::time::interval(Duration::from_millis(100));
    meter_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        if let Some(view) = control.call.as_ref().filter(|view| view.call_id == call_id) {
            levels.roster(view);
            if let Some(media) = media {
                media.receive.roster(view)?;
            }
            match view.state {
                CallState::Ended => return Ok(true),
                CallState::Uncertain => {
                    bail!("call outcome unknown; check calls status before retrying")
                }
                _ => {}
            }
        }
        while let Some((id, results)) = control.invitation_results.pop_front() {
            if id == call_id {
                for result in results {
                    let state = match result.status {
                        wechat_core::call_types::CallInvitationStatus::Submitted => {
                            "submitted; waiting for acceptance"
                        }
                        wechat_core::call_types::CallInvitationStatus::Rejected => "rejected",
                        wechat_core::call_types::CallInvitationStatus::Unknown => "unknown",
                        wechat_core::call_types::CallInvitationStatus::Uncertain => {
                            "outcome uncertain; do not resend"
                        }
                    };
                    levels.notice(&format!(
                        "Invitation to {}: {state}{}",
                        super::levels::clean_label(&result.contact_id, 64),
                        if result.verification_required {
                            "; contact verification required"
                        } else {
                            ""
                        }
                    ));
                }
            }
        }
        if let Some(media) = media {
            let (changed, _) = media.synchronize_tracks(control, call_id)?;
            if changed {
                audio.discard_capture();
                levels.clear_microphone();
            }
        }
        if control
            .call
            .as_ref()
            .is_some_and(|view| view.call_id == call_id && view.state == CallState::Connected)
            && media.is_none()
        {
            let attached = tokio::select! {_=cancellation.wait()=>return Ok(false),result=Media::attach(remote,control,call_id,owner_token)=>result?};
            let sources = attached.receive.track_count();
            audio.discard_capture();
            levels.clear_microphone();
            *media = Some(attached);
            levels.notice(&format!("Connected. Microphone and mixed speakers active; {sources} receive tracks.\n{CALL_KEYS}"));
        }
        tokio::select! {
            _=cancellation.wait()=>return Ok(false),
            line=input.next_line()=>{
                let Some(line)=line? else{return Ok(false);};levels.input_received();
                let trimmed=line.trim();
                match trimmed {
                    "l"|"leave"|"h"|"hangup"|"q"=>return Ok(false),
                    "m"|"mute"=>{
                        if pending.values().any(|(_,old)|old.is_some()){levels.notice("A microphone request is pending; audio continues.");continue;}
                        let old=muted;muted = !muted;audio.discard_capture();levels.microphone(&[],muted);
                        let id=request_id();
                        send_json(&mut control.socket,&CallCommand::Mute {request_id:id.clone(),call_id:call_id.into(),muted}).await?;
                        pending.insert(id,(Instant::now(),Some(old)));
                        levels.notice(if muted {"Microphone locally muted; signaling pending."}else{"Microphone locally unmuted; signaling pending."});
                    }
                    "s"|"speaker"=>{
                        speaker_enabled = !speaker_enabled;audio.playback_token=audio.playback.clear()?;levels.playback_enabled(speaker_enabled);
                        levels.notice(if speaker_enabled {"Local speaker enabled."}else{"Local speaker disabled; participant meters still measure incoming audio."});
                    }
                    "p"|"participants"=>{
                        let mut text=String::from("Participants (microphone state is independent of signal level):\n");
                        if let Some(view)=control.call.as_ref().filter(|view|view.call_id==call_id) {
                            for p in &view.participants {text.push_str(&format!("{}: {:?}; microphone {}\n",super::levels::clean_label(&p.contact_id,64),p.membership,match p.microphone_enabled {Some(true)=>"on",Some(false)=>"off",None=>"unknown"}));}
                        }
                        levels.notice(text.trim_end());
                    }
                    "?"|"help"=>levels.notice(&format!("{CALL_KEYS}\ni searches contacts; invite CONTACT_ID [...] sends selected IDs.\nd lists devices; d input NUMBER / d output NUMBER selects one.\nInvitations never promote a direct call. Enter cancel to close a picker.")),
                    _=>match interaction.line(trimmed,remote,control.call.as_ref().filter(|view|view.call_id==call_id),&audio.options,&mut levels) {
                        Ok(super::interaction::UiAction::None)=>{},
                        Ok(super::interaction::UiAction::Invite(contact_ids))=>{
                            if !control.capabilities.conference_invite {levels.notice("Conference invitations unavailable on this daemon.");continue;}
                            let id=request_id();
                            send_json(&mut control.socket,&CallCommand::Invite {request_id:id.clone(),call_id:call_id.into(),contact_ids}).await?;
                            pending.insert(id,(Instant::now(),None));levels.notice("Invitation request submitted; media continues while it completes.");
                        }
                        Ok(super::interaction::UiAction::Devices(options))=>{
                            if device_change.0.is_some(){levels.notice("A device change is already preparing.");continue;}
                            device_change.0=Some(tokio::spawn(async move{LocalAudio::open(&options).await}));
                            levels.notice("Preparing new devices; current audio remains active.");
                        }
                        Err(error)=>levels.notice(&error.to_string()),
                    },
                }
            }
            _=interaction.completed(&mut levels)=>{},
            replacement=device_change.completed()=>{
                device_change.0=None;
                match replacement {
                    Ok(mut replacement)=>{
                        replacement.discard_capture();let mut previous=std::mem::replace(audio,replacement);previous.request_stop();
                        tokio::spawn(async move{let _=previous.stop().await;});
                        levels.clear_microphone();levels.clear_speaker();levels.notice("Local audio devices changed.");
                    }
                    Err(error)=>levels.notice(&format!("Device change failed; current devices retained: {error}")),
                }
            }
            event=control.next_event()=>{
                let event=event?;control.update(&event);
                match event {
                    CallEvent::State {call:view} if view.call_id==call_id=>match view.state {CallState::Ended=>{levels.notice("Call ended.");return Ok(true);},CallState::Uncertain=>bail!("call outcome unknown; check calls status before retrying"),_=>{}},
                    CallEvent::Availability {availability} if !availability.busy=>return Ok(true),
                    CallEvent::CommandResult {request_id,status,code,..}=>{
                        if let Some((_,old))=pending.remove(&request_id) {
                            if status==wechat_core::call_types::CallCommandStatus::Error {
                                if let Some(old)=old {muted=old;audio.discard_capture();levels.microphone(&[],muted);}
                                levels.notice(&format!("Command rejected: {}",serde_json::to_value(code)?));
                            }else if old.is_some(){levels.notice(if muted {"Microphone muted."}else{"Microphone unmuted."});}
                        }
                    }
                    CallEvent::Error {code}=>bail!("call error: {}",serde_json::to_value(code)?),
                    _=>{},
                }
            }
            message=next_media(media)=>{
                let Message::Text(text)=message? else {bail!("unexpected binary call media message");};
                let current=media.as_mut().context("media detached")?;
                let value:serde_json::Value=serde_json::from_str(&text)?;
                if value["type"]=="audio.chunk" {
                    current.receive.chunk(serde_json::from_value(value)?,current.attached.elapsed(),&mut levels)?;
                }else{
                    let event:CallEvent=serde_json::from_value(value)?;
                    match &event {
                        CallEvent::TrackAdded {track,..}|CallEvent::TrackUpdated {track,..} if track.direction==CallTrackDirection::Send=>{control.update(&event);},
                        CallEvent::TrackAdded {..}|CallEvent::TrackUpdated {..}|CallEvent::TrackRemoved {..}=>{
                            if current.receive.event(&event,call_id)? && matches!(event,CallEvent::TrackRemoved {..}) {audio.playback_token=audio.playback.clear()?;levels.clear_speaker();}
                        }
                        CallEvent::Error {code}=>bail!("media error: {}",serde_json::to_value(code)?),
                        _=>bail!("unexpected media lifecycle event"),
                    }
                }
            }
            _=meter_tick.tick()=>{
                levels.refresh();
                let expired=pending.iter().filter(|(_, (started,_))|started.elapsed()>Duration::from_secs(30)).map(|(id,_)|id.clone()).collect::<Vec<_>>();
                for id in expired {pending.remove(&id);levels.notice("Command outcome uncertain; no retry was made. Check the roster/status before submitting again.");}
            }
            _=tick.tick()=>{
                let samples=audio.frame(format.samples_per_frame(),muted)?;
                if let Some(media)=media {
                    if let Some(mixed)=media.receive.render(media.attached.elapsed())? {
                        if speaker_enabled {
                            let mixed=mixed.iter().map(|s|f32::from(*s)/32768.0).collect::<Vec<_>>();
                            match audio.playback.enqueue(audio.playback_token,&mixed,format.sample_rate()) {Ok(())=>levels.speaker(&mixed),Err(AudioError::QueueFull)=>{audio.playback_token=audio.playback.clear()?;levels.clear_speaker();},Err(error)=>return Err(error.into())}
                        }
                    }
                    levels.microphone(&samples,muted);media.send(&samples,format).await?;
                }
            }
        }
    }
}

struct DeviceChange(Option<tokio::task::JoinHandle<Result<LocalAudio>>>);
impl DeviceChange {
    async fn completed(&mut self) -> Result<LocalAudio> {
        match self.0.as_mut() {
            Some(task) => task.await.context("device worker stopped")?,
            None => std::future::pending().await,
        }
    }
}
impl Drop for DeviceChange {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

pub(super) async fn terminate_owned(control: &mut Control, call_id: &str) -> Result<()> {
    let cancellation = control.cancellation.take();
    let result = terminate(control, call_id).await;
    control.cancellation = cancellation;
    result
}

async fn terminate(control: &mut Control, call_id: &str) -> Result<()> {
    timeout(Duration::from_secs(8), async {
        // A claim result can precede its owned state event. Keep the exact call
        // identity while waiting for the revision needed by cancel or hangup.
        let call = loop {
            if let Some(call) = control.call.as_ref().filter(|call| call.call_id == call_id) {
                match call.state {
                    CallState::Ended => return Ok(()),
                    CallState::Uncertain => {
                        bail!("termination outcome unknown; check calls status before retrying")
                    }
                    _ => break call.clone(),
                }
            }
            let event = control.next_event().await?;
            control.update(&event);
            if matches!(event, CallEvent::Availability { availability } if !availability.busy) {
                return Ok(());
            }
        };
        if call.state != CallState::Ending {
            let command = if call.direction == wechat_core::call_types::CallDirection::Outgoing
                && matches!(call.state, CallState::Dialing | CallState::Ringing)
            {
                CallCommand::Cancel {
                    request_id: request_id(),
                    call_id: call_id.into(),
                    expected_revision: call.revision,
                }
            } else {
                CallCommand::Hangup {
                    request_id: request_id(),
                    call_id: call_id.into(),
                    expected_revision: call.revision,
                }
            };
            control.command(command).await?;
        }
        loop {
            if control
                .call
                .as_ref()
                .is_some_and(|call| call.call_id == call_id && call.state == CallState::Ended)
            {
                return Ok(());
            }
            let event = control.next_event().await?;
            control.update(&event);
            match event {
                CallEvent::State { call }
                    if call.call_id == call_id && call.state == CallState::Uncertain =>
                {
                    bail!("termination outcome unknown; check calls status before retrying")
                }
                CallEvent::Availability { availability } if !availability.busy => return Ok(()),
                _ => {}
            }
        }
    })
    .await
    .context("termination outcome unknown; check calls status before retrying")?
}

struct LocalAudio {
    options: AudioOptions,
    playback: PlaybackHandle,
    playback_token: PlaybackToken,
    capture: mpsc::Receiver<Vec<f32>>,
    queued: VecDeque<i16>,
    last_capture: Instant,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<()>>>,
}
impl LocalAudio {
    async fn open(options: &AudioOptions) -> Result<Self> {
        let config = AudioConfig {
            input_device: options.input_device.clone(),
            output_device: options.output_device.clone(),
            ..AudioConfig::default()
        };
        let session = timeout(
            Duration::from_secs(12),
            tokio::task::spawn_blocking(move || -> Result<_> {
                let mut session = AudioSession::open(config)?;
                let until = Instant::now() + Duration::from_secs(3);
                loop {
                    match session.recv_timeout(Duration::from_millis(100)) {
                        Ok(samples) if !samples.is_empty() => break,
                        Ok(_) | Err(AudioError::Timeout) if Instant::now() < until => {}
                        Ok(_) | Err(AudioError::Timeout) => {
                            bail!("microphone produced no samples; no call was placed or answered")
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(session)
            }),
        )
        .await
        .context("audio device startup timed out; no call was placed or answered")???;
        eprintln!(
            "Microphone: {}\nSpeaker: {}",
            options.input_device.as_deref().unwrap_or("OS default"),
            options.output_device.as_deref().unwrap_or("OS default")
        );
        eprintln!(
            "Audio AEC status: {:?}; cancellation quality is unmeasured. A headset is recommended.",
            session.aec_status()
        );
        let playback = session.playback();
        let playback_token = playback.token()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, capture) = mpsc::channel(8);
        let worker = std::thread::Builder::new()
            .name("wx-call-audio".into())
            .spawn(move || {
                let mut session = session;
                while !worker_stop.load(Ordering::Acquire) {
                    match session.recv_timeout(Duration::from_millis(20)) {
                        Ok(samples) if !samples.is_empty() => {
                            let _ = sender.try_send(samples);
                        }
                        Ok(_) | Err(AudioError::Timeout) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                session.stop()?;
                Ok(())
            })?;
        Ok(Self {
            options: options.clone(),
            playback,
            playback_token,
            capture,
            queued: VecDeque::new(),
            last_capture: Instant::now(),
            stop,
            worker: Some(worker),
        })
    }
    fn request_stop(&self) {
        if !self.stop.swap(true, Ordering::AcqRel) {
            let _ = self.playback.clear();
        }
    }
    fn discard_capture(&mut self) {
        while self.capture.try_recv().is_ok() {}
        self.queued.clear();
        self.last_capture = Instant::now();
    }
    fn frame(&mut self, count: usize, muted: bool) -> Result<Vec<i16>> {
        loop {
            match self.capture.try_recv() {
                Ok(samples) => {
                    self.last_capture = Instant::now();
                    for sample in samples {
                        if !sample.is_finite() {
                            bail!("invalid microphone sample");
                        }
                        self.queued
                            .push_back((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16);
                    }
                    while self.queued.len() > 3200 {
                        self.queued.pop_front();
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => bail!("microphone disconnected"),
            }
        }
        if self.last_capture.elapsed() > Duration::from_secs(2) {
            bail!("microphone stopped producing samples");
        }
        let mut frame = vec![0; count];
        for sample in &mut frame {
            *sample = self.queued.pop_front().unwrap_or(0);
        }
        if muted {
            frame.fill(0);
        }
        Ok(frame)
    }
    async fn stop(&mut self) -> Result<()> {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            timeout(
                Duration::from_secs(3),
                tokio::task::spawn_blocking(move || {
                    worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("audio worker panicked"))?
                }),
            )
            .await
            .context("audio device shutdown timed out")???;
        }
        Ok(())
    }
}
impl Drop for LocalAudio {
    fn drop(&mut self) {
        self.request_stop();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{hello, remote};
    use super::*;
    use tokio::sync::oneshot;
    use wechat_core::call_types::CallCommandStatus;

    #[tokio::test]
    async fn losing_answer_preserves_exact_offer_and_does_not_claim_a_new_caller() {
        use wechat_core::call_types::{CallErrorCode, CallKind, CallOffer};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (checked, wait_checked) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            super::super::tests::assert_client_hello(&mut socket).await;
            let mut initial = hello();
            if let CallEvent::Hello {
                capabilities,
                current_offer,
                ..
            } = &mut initial
            {
                capabilities.direct_audio = true;
                *current_offer = Some(CallOffer {
                    call_id: "original-offer".into(),
                    revision: 7,
                    kind: CallKind::Direct,
                    call_type: wechat_core::call_types::CallType::Voice,
                    caller_id: "alice".into(),
                });
            }
            socket
                .send(Message::Text(
                    serde_json::to_string(&initial).unwrap().into(),
                ))
                .await
                .unwrap();
            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected control command");
            };
            let command: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(command["type"], "call.answer");
            assert_eq!(command["callId"], "original-offer");
            assert_eq!(command["expectedRevision"], 7);
            let result = CallEvent::CommandResult {
                request_id: command["requestId"].as_str().unwrap().into(),
                status: CallCommandStatus::Error,
                call_id: None,
                owner_token: None,
                code: Some(CallErrorCode::AlreadyClaimed),
            };
            socket
                .send(Message::Text(
                    serde_json::to_string(&result).unwrap().into(),
                ))
                .await
                .unwrap();
            let offer = CallEvent::Offered {
                offer: CallOffer {
                    call_id: "different-offer".into(),
                    revision: 1,
                    kind: CallKind::Direct,
                    call_type: wechat_core::call_types::CallType::Voice,
                    caller_id: "bob".into(),
                },
            };
            socket
                .send(Message::Text(serde_json::to_string(&offer).unwrap().into()))
                .await
                .unwrap();
            assert!(timeout(Duration::from_millis(50), socket.next())
                .await
                .is_err());
            checked.send(()).unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let (mut control, _) = Control::connect(&remote(address)).await.unwrap();
        assert!(super::exact_offer(None, None).is_err());
        assert!(super::exact_offer(control.offer.as_ref(), Some("different-offer")).is_err());
        let offer = super::exact_offer(control.offer.as_ref(), Some("original-offer"))
            .unwrap()
            .clone();
        let error = match control
            .command(CallCommand::Answer {
                request_id: "claim-one".into(),
                call_id: offer.call_id,
                expected_revision: offer.revision,
            })
            .await
        {
            Ok(_) => panic!("losing claim was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already_claimed"));
        wait_checked.await.unwrap();
        control.close().await;
        server.await.unwrap();
    }
}
