//! Interactive local audio and video devices. No device is opened until dial
//! or an accepted call, and cameras are only captured while enabled in a call.
use super::*;
use anyhow::{bail, ensure, Context};
use std::{io::IsTerminal, time::Instant};
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines, Stdin},
    time::timeout,
};
use tokio_tungstenite::tungstenite::Message;
use wechat_audio::AudioError;
use wechat_call_client::{audio::LocalAudio, media::MediaClient};
use wechat_calls::audio::AudioFormat;
use wechat_core::{
    call_types::{
        CallConferenceInvitation, CallReadyScope, CallState, CallTarget, CallTrack,
        CallTrackDirection, CallType,
    },
    calls::pcm_envelope::{MediaPayloadKind, WireMediaFrame},
};

type Input = Lines<BufReader<Stdin>>;
const CALL_KEYS: &str = "m + Enter    Microphone          v + Enter    Camera\n\
                        s + Enter    Speaker             i + Enter    Invite (conference)\n\
                        p + Enter    Participants        d + Enter    Devices\n\
                        ? + Enter    Help                l/h + Enter  Leave / hangup\n\
                        Ctrl-C       Leave and exit";
const MONITOR_WAITING: &str =
    "Waiting for calls. Toggle m (mic) or v (camera) before accepting; Ctrl-C exits.";

#[derive(Clone, Debug)]
struct ResolvedMediaPrefs {
    camera_source: super::video::CameraSource,
    camera_off: bool,
    muted: bool,
}

impl ResolvedMediaPrefs {
    fn from_incoming(options: &IncomingMediaOptions) -> Result<Self> {
        let camera_source = match options.camera.as_deref() {
            Some(raw) => super::video::CameraSource::parse(raw)?,
            None => super::video::CameraSource::Auto,
        };
        Ok(Self {
            camera_off: options.camera_off || camera_source.is_off(),
            camera_source,
            muted: options.muted,
        })
    }

    fn from_outgoing(options: &OutgoingMediaOptions) -> Result<Self> {
        let camera_source = match options.camera.as_deref() {
            Some(raw) => super::video::CameraSource::parse(raw)?,
            None => super::video::CameraSource::Auto,
        };
        Ok(Self {
            camera_off: options.camera_off || camera_source.is_off(),
            camera_source,
            muted: options.muted,
        })
    }
}

/// Pre-answer media toggle state for interactive `wx calls monitor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MonitorStaging {
    pub(super) direct_muted: bool,
    pub(super) direct_camera: bool,
    pub(super) conference_muted: bool,
    pub(super) conference_camera: bool,
    pub(super) camera_source_off: bool,
}

impl MonitorStaging {
    pub(super) fn new(default_muted: bool, default_camera_off: bool, source_off: bool) -> Self {
        Self {
            direct_muted: default_muted,
            direct_camera: !default_camera_off && !source_off,
            conference_muted: default_muted,
            conference_camera: false,
            camera_source_off: source_off,
        }
    }

    pub(super) fn reset_for_offer(
        &mut self,
        offer: &CallOffer,
        default_muted: bool,
        default_camera_off: bool,
        daemon_direct_video: bool,
    ) {
        self.direct_muted = default_muted;
        self.direct_camera = offer.call_type == CallType::Video
            && daemon_direct_video
            && !default_camera_off
            && !self.camera_source_off;
    }

    pub(super) fn toggle_direct_mic(&mut self) -> bool {
        self.direct_muted = !self.direct_muted;
        self.direct_muted
    }

    pub(super) fn toggle_direct_camera(
        &mut self,
        offer: &CallOffer,
        daemon_direct_video: bool,
    ) -> Result<bool, &'static str> {
        if offer.call_type != CallType::Video {
            return Err("Camera is unavailable on an incoming voice call.");
        }
        if !daemon_direct_video {
            return Err("Video calling is unavailable on this daemon.");
        }
        if self.camera_source_off {
            return Err("Camera source is set to off; restart without --camera off.");
        }
        self.direct_camera = !self.direct_camera;
        Ok(self.direct_camera)
    }

    pub(super) fn toggle_conference_mic(&mut self) -> bool {
        self.conference_muted = !self.conference_muted;
        self.conference_muted
    }

    pub(super) fn toggle_conference_camera(
        &mut self,
        daemon_conference_video: bool,
    ) -> Result<bool, &'static str> {
        if !daemon_conference_video {
            return Err("Conference video is unavailable on this daemon.");
        }
        if self.camera_source_off {
            return Err("Camera source is set to off; restart without --camera off.");
        }
        self.conference_camera = !self.conference_camera;
        Ok(self.conference_camera)
    }
}

pub(super) async fn run(remote: &Remote, command: CallsCommand, json: bool) -> Result<()> {
    if json {
        bail!("use calls monitor --json for passive observation; dial and answer are interactive sessions");
    }
    if !std::io::stdin().is_terminal() {
        bail!("interactive calls require a terminal; use calls monitor --json to observe without devices");
    }
    let (cancellation, _signals) = listen_cancellation();
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
        CallsCommand::Monitor { audio, media } => {
            let prefs = ResolvedMediaPrefs::from_incoming(&media)?;
            monitor(
                remote,
                &mut control,
                &audio,
                &prefs,
                &mut input,
                &cancellation,
            )
            .await
        }
        CallsCommand::Dial {
            contact,
            audio,
            media,
        } => {
            let prefs = ResolvedMediaPrefs::from_outgoing(&media)?;
            ensure!(
                control.capabilities.direct_audio,
                "direct calling unavailable on this daemon"
            );
            let call_type = if media.video {
                ensure!(
                    control.capabilities.direct_video,
                    "direct video calling unavailable on this daemon"
                );
                CallType::Video
            } else {
                CallType::Voice
            };
            if control.availability.busy {
                bail!("busy: the daemon already has an ongoing or unresolved call");
            }
            let camera_enabled = call_type == CallType::Video && !prefs.camera_off;
            participate(
                remote,
                &mut control,
                &audio,
                Participation::Dial {
                    contact,
                    call_type,
                    camera_enabled,
                    muted: prefs.muted,
                },
                &prefs.camera_source,
                &mut input,
                &cancellation,
            )
            .await
        }
        CallsCommand::Answer {
            call_id,
            audio,
            media,
        } => {
            let prefs = ResolvedMediaPrefs::from_incoming(&media)?;
            let target = pin_answer(&control, call_id.as_deref())?;
            let camera_enabled = target.call_type == CallType::Video
                && control.capabilities.direct_video
                && !prefs.camera_off;
            participate(
                remote,
                &mut control,
                &audio,
                Participation::Answer {
                    target,
                    camera_enabled,
                    muted: prefs.muted,
                },
                &prefs.camera_source,
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
            let (action, options, source) = match command {
                ConferenceCommand::Start {
                    group,
                    invitees,
                    audio,
                    media,
                } => {
                    let prefs = ResolvedMediaPrefs::from_outgoing(&media)?;
                    ensure!(
                        control.capabilities.conference_create
                            && control.capabilities.conference_audio,
                        "conference creation/audio unavailable on this daemon"
                    );
                    let call_type = if media.video {
                        ensure!(
                            control.capabilities.conference_video,
                            "conference video unavailable on this daemon"
                        );
                        CallType::Video
                    } else {
                        CallType::Voice
                    };
                    let camera_enabled = call_type == CallType::Video && !prefs.camera_off;
                    (
                        Participation::ConferenceStart {
                            group,
                            invitees,
                            call_type,
                            camera_enabled,
                            muted: prefs.muted,
                        },
                        audio,
                        prefs.camera_source,
                    )
                }
                ConferenceCommand::Join {
                    group,
                    audio,
                    media,
                } => {
                    let prefs = ResolvedMediaPrefs::from_outgoing(&media)?;
                    ensure!(
                        control.capabilities.conference_join
                            && control.capabilities.conference_audio,
                        "conference joining/audio unavailable on this daemon"
                    );
                    if media.video {
                        ensure!(
                            control.capabilities.conference_video,
                            "conference video unavailable on this daemon"
                        );
                    }
                    let invitation = conference_invitation(&control, &group)?;
                    let camera_enabled = media.video && !prefs.camera_off;
                    (
                        Participation::ConferenceJoin {
                            invitation,
                            camera_enabled,
                            muted: prefs.muted,
                        },
                        audio,
                        prefs.camera_source,
                    )
                }
            };
            participate(
                remote,
                &mut control,
                &options,
                action,
                &source,
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

fn daemon_video_available(control: &Control) -> bool {
    control.capabilities.direct_video || control.capabilities.conference_video
}

async fn ready(control: &mut Control, enabled: bool) -> Result<()> {
    let video = if enabled {
        let daemon_video = daemon_video_available(control);
        tokio::task::spawn_blocking(move || super::video::client_video_capabilities(daemon_video))
            .await
            .context("video capability probe failed")?
    } else {
        None
    };
    control
        .command(CallCommand::Ready {
            request_id: request_id(),
            ready: enabled,
            format: control.format,
            scope: Some(CallReadyScope::All),
            video,
        })
        .await?;
    Ok(())
}

fn print_offer_prompt(
    offer: &CallOffer,
    staging: &MonitorStaging,
    camera_source: &super::video::CameraSource,
    daemon_direct_video: bool,
) {
    let kind_label = match offer.call_type {
        CallType::Video => "video",
        CallType::Voice => "voice",
    };
    let mic = if staging.direct_muted { "MUTED" } else { "ON" };
    let cam = if offer.call_type == CallType::Video && daemon_direct_video {
        if staging.direct_camera {
            format!("ON ({})", camera_source.label())
        } else {
            "OFF".into()
        }
    } else if offer.call_type == CallType::Video {
        "OFF (daemon video unavailable)".into()
    } else {
        "N/A (voice call)".into()
    };
    let controls = if offer.call_type == CallType::Video && daemon_direct_video {
        "[a] Accept  [m] Toggle mic  [v] Toggle camera  [d] Dismiss here  [r] Reject call"
    } else {
        "[a] Accept  [m] Toggle mic  [d] Dismiss here  [r] Reject call"
    };
    eprintln!(
        "Incoming {kind_label} call from {} ({})\nStaged: Mic {mic} | Camera {cam}\n{controls}",
        super::levels::clean_label(&offer.caller_id, 64),
        super::levels::clean_label(&offer.call_id, 64),
    );
}

fn print_conference_staging(
    staging: &MonitorStaging,
    camera_source: &super::video::CameraSource,
    daemon_conference_video: bool,
) {
    let mic = if staging.conference_muted {
        "MUTED"
    } else {
        "ON"
    };
    let cam = if daemon_conference_video {
        if staging.conference_camera {
            format!("ON ({})", camera_source.label())
        } else {
            "OFF".into()
        }
    } else {
        "N/A".into()
    };
    let controls = if daemon_conference_video {
        "[j N] Join  [m] Toggle mic  [v] Toggle camera"
    } else {
        "[j N] Join  [m] Toggle mic"
    };
    eprintln!("Staged join: Mic {mic} | Camera {cam}\n{controls}");
}

async fn monitor(
    remote: &Remote,
    control: &mut Control,
    options: &AudioOptions,
    prefs: &ResolvedMediaPrefs,
    input: &mut Input,
    cancellation: &Cancellation,
) -> Result<()> {
    // Readiness declares this interactive client's ability to prepare devices;
    // it does not open or continuously capture microphone or camera while waiting.
    ready(control, true).await?;
    eprintln!("{MONITOR_WAITING}");
    let mut shown = None;
    let mut dismissed = None;
    let mut renew_after_terminal = false;
    let mut shown_invitations = Vec::new();
    let mut staging =
        MonitorStaging::new(prefs.muted, prefs.camera_off, prefs.camera_source.is_off());
    loop {
        let invitations = control
            .conference_invitations
            .iter()
            .map(|i| (i.invitation_id.clone(), i.revision))
            .collect::<Vec<_>>();
        if invitations != shown_invitations {
            shown_invitations = invitations;
            if !control.conference_invitations.is_empty() {
                staging.conference_muted = prefs.muted;
                staging.conference_camera = false;
                for (index, invitation) in control.conference_invitations.iter().enumerate() {
                    eprintln!(
                        "Conference invitation {}: {} from {}.",
                        index + 1,
                        super::levels::clean_label(&invitation.group_id, 64),
                        super::levels::clean_label(&invitation.caller_id, 64),
                    );
                }
                print_conference_staging(
                    &staging,
                    &prefs.camera_source,
                    control.capabilities.conference_video,
                );
            }
        }
        if let Some(offer) = &control.offer {
            let identity = (offer.call_id.clone(), offer.revision);
            if shown.as_ref() != Some(&identity) && dismissed.as_ref() != Some(&identity) {
                staging.reset_for_offer(
                    offer,
                    prefs.muted,
                    prefs.camera_off,
                    control.capabilities.direct_video,
                );
                print_offer_prompt(
                    offer,
                    &staging,
                    &prefs.camera_source,
                    control.capabilities.direct_video,
                );
                shown = Some(identity);
            }
        }
        tokio::select! {
            _ = cancellation.wait() => return Ok(()),
            event = control.next_event() => {
                let event = event?;
                control.update(&event);
                if renew_after_terminal
                    && matches!(event, CallEvent::Availability { availability } if !availability.busy)
                {
                    renew_after_terminal = false;
                    restore_monitor_readiness(control).await?;
                }
            }
            line = input.next_line() => {
                let Some(line) = line? else { return Ok(()); };
                let trimmed = line.trim();
                let lower = trimmed.to_ascii_lowercase();
                if lower == "j" || lower.starts_with("j ") {
                    let selected = lower
                        .strip_prefix("j ")
                        .unwrap_or("1")
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| index.checked_sub(1))
                        .and_then(|index| control.conference_invitations.get(index))
                        .cloned();
                    if let Some(invitation) = selected {
                        if !control.capabilities.conference_join || !control.capabilities.conference_audio {
                            eprintln!("Conference joining/audio unavailable.");
                            continue;
                        }
                        if let Err(error) = participate(
                            remote,
                            control,
                            options,
                            Participation::ConferenceJoin {
                                invitation,
                                camera_enabled: staging.conference_camera
                                    && control.capabilities.conference_video,
                                muted: staging.conference_muted,
                            },
                            &prefs.camera_source,
                            input,
                            cancellation,
                        )
                        .await
                        {
                            eprintln!("{error}");
                        }
                        if cancellation.is_cancelled() {
                            return Ok(());
                        }
                        restore_monitor_readiness(control).await?;
                        shown_invitations.clear();
                        eprintln!("{MONITOR_WAITING}");
                    } else {
                        eprintln!("No matching current conference invitation.");
                    }
                    continue;
                }
                if matches!(lower.as_str(), "m" | "mute") {
                    if let Some(offer) = control.offer.as_ref() {
                        staging.toggle_direct_mic();
                        print_offer_prompt(
                            offer,
                            &staging,
                            &prefs.camera_source,
                            control.capabilities.direct_video,
                        );
                    } else if !control.conference_invitations.is_empty() {
                        staging.toggle_conference_mic();
                        print_conference_staging(
                            &staging,
                            &prefs.camera_source,
                            control.capabilities.conference_video,
                        );
                    } else {
                        eprintln!("No incoming call or conference invitation to stage.");
                    }
                    continue;
                }
                if matches!(lower.as_str(), "v" | "video" | "camera") {
                    if let Some(offer) = control.offer.as_ref() {
                        match staging.toggle_direct_camera(offer, control.capabilities.direct_video) {
                            Ok(_) => print_offer_prompt(
                                offer,
                                &staging,
                                &prefs.camera_source,
                                control.capabilities.direct_video,
                            ),
                            Err(msg) => eprintln!("{msg}"),
                        }
                    } else if !control.conference_invitations.is_empty() {
                        match staging.toggle_conference_camera(control.capabilities.conference_video) {
                            Ok(_) => print_conference_staging(
                                &staging,
                                &prefs.camera_source,
                                control.capabilities.conference_video,
                            ),
                            Err(msg) => eprintln!("{msg}"),
                        }
                    } else {
                        eprintln!("No incoming call or conference invitation to stage.");
                    }
                    continue;
                }
                let Some(offer) = control.offer.clone() else {
                    continue;
                };
                match lower.as_str() {
                    "a" | "accept" => {
                        renew_after_terminal = true;
                        let camera_enabled = offer.call_type == CallType::Video
                            && control.capabilities.direct_video
                            && staging.direct_camera;
                        let result = participate(
                            remote,
                            control,
                            options,
                            Participation::Answer {
                                target: offer.into(),
                                camera_enabled,
                                muted: staging.direct_muted,
                            },
                            &prefs.camera_source,
                            input,
                            cancellation,
                        )
                        .await;
                        let completed = result.is_ok();
                        if let Err(error) = result {
                            if cancellation.is_cancelled() {
                                return Err(error);
                            }
                            eprintln!("{error}");
                        }
                        if cancellation.is_cancelled() {
                            return Ok(());
                        }
                        if completed || !control.availability.busy {
                            restore_monitor_readiness(control).await?;
                            renew_after_terminal = false;
                        }
                        shown = None;
                        eprintln!("{MONITOR_WAITING}");
                    }
                    "d" | "dismiss" => {
                        dismissed = Some((offer.call_id, offer.revision));
                    }
                    "r" | "reject" => {
                        let result = control
                            .command(CallCommand::Reject {
                                request_id: request_id(),
                                call_id: offer.call_id,
                                expected_revision: offer.revision,
                            })
                            .await;
                        if let Err(error) = result {
                            eprintln!("{error}");
                        }
                    }
                    _ => eprintln!("Choose a, m, v, d or r; Ctrl-C exits."),
                }
            }
        }
    }
}

pub(super) async fn restore_monitor_readiness(control: &mut Control) -> Result<()> {
    ready(control, true).await
}

enum Participation {
    Dial {
        contact: String,
        call_type: CallType,
        camera_enabled: bool,
        muted: bool,
    },
    ConferenceStart {
        group: String,
        invitees: Vec<String>,
        call_type: CallType,
        camera_enabled: bool,
        muted: bool,
    },
    ConferenceJoin {
        invitation: CallConferenceInvitation,
        camera_enabled: bool,
        muted: bool,
    },
    Answer {
        target: AnswerTarget,
        camera_enabled: bool,
        muted: bool,
    },
}

struct AnswerTarget {
    call_id: String,
    revision: u32,
    call_type: CallType,
}
impl From<CallOffer> for AnswerTarget {
    fn from(offer: CallOffer) -> Self {
        Self {
            call_id: offer.call_id,
            revision: offer.revision,
            call_type: offer.call_type,
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
            call_type: offer.call_type,
        });
    }
    bail!("offer_expired: no matching current call; answer never waits for a new caller")
}
async fn await_exact_offer(
    control: &mut Control,
    target: &AnswerTarget,
    cancellation: &Cancellation,
) -> Result<CallOffer> {
    timeout(Duration::from_secs(30), async {
        loop {
            if let Some(offer) = control
                .offer
                .as_ref()
                .filter(|offer| offer.call_id == target.call_id)
            {
                return Ok(offer.clone());
            }
            if control
                .pending_offer
                .as_ref()
                .is_none_or(|offer| offer.call_id != target.call_id)
            {
                bail!("offer_expired: the selected call ended or was replaced");
            }
            let event = tokio::select! {
                _=cancellation.wait()=>return Err(failure(130,"client_interrupted","call preparation cancelled before answer")),
                event=control.next_event()=>event?,
            };
            control.update(&event);
            if let CallEvent::Error { code } = event {
                bail!("call preparation failed: {}", serde_json::to_value(code)?);
            }
        }
    })
    .await
    .context("the selected call did not become ready; no answer was submitted")?
}

struct ActiveCallIntent {
    header_title: String,
    peer_label: String,
    is_conference: bool,
    video_allowed: bool,
    initial_camera: bool,
    initial_muted: bool,
    camera_source: super::video::CameraSource,
}

async fn participate(
    remote: &Remote,
    control: &mut Control,
    options: &AudioOptions,
    action: Participation,
    camera_source: &super::video::CameraSource,
    input: &mut Input,
    cancellation: &Cancellation,
) -> Result<()> {
    // For late answers the exact ID/revision was captured before device startup.
    // The server revalidates it atomically; failure never targets the next offer.
    let device_options = AudioDeviceOptions::from(options);
    let mut audio = tokio::select! {
        _=cancellation.wait()=>return Err(failure(130,"client_interrupted","device preparation cancelled; no call was placed or answered")),
        audio=LocalAudio::open(&device_options)=>audio?,
    };
    eprintln!("{}", audio.startup_summary);
    let scope = match &action {
        Participation::Dial { .. }
        | Participation::ConferenceStart { .. }
        | Participation::ConferenceJoin { .. } => CallReadyScope::Outgoing,
        Participation::Answer { target, .. } => CallReadyScope::Offer {
            call_id: target.call_id.clone(),
            expected_revision: target.revision,
        },
    };
    let daemon_video = daemon_video_available(control);
    let video_caps =
        tokio::task::spawn_blocking(move || super::video::client_video_capabilities(daemon_video))
            .await
            .context("video capability probe failed")?;
    control
        .command(CallCommand::Ready {
            request_id: request_id(),
            ready: true,
            format: control.format,
            scope: Some(scope),
            video: video_caps,
        })
        .await?;
    let (command, intent) = match action {
        Participation::Dial {
            contact,
            call_type,
            camera_enabled,
            muted,
        } => {
            let title = format!(
                "Direct {} Call • {}",
                if call_type == CallType::Video {
                    "Video"
                } else {
                    "Voice"
                },
                super::levels::clean_label(&contact, 48)
            );
            let intent = ActiveCallIntent {
                header_title: title,
                peer_label: contact.clone(),
                is_conference: false,
                video_allowed: call_type == CallType::Video && control.capabilities.direct_video,
                initial_camera: camera_enabled,
                initial_muted: muted,
                camera_source: camera_source.clone(),
            };
            let cmd = CallCommand::Dial {
                request_id: request_id(),
                target: CallTarget::Direct {
                    contact_id: contact,
                },
                call_type,
                camera_enabled: (call_type == CallType::Video).then_some(camera_enabled),
                muted: muted.then_some(true),
            };
            (cmd, intent)
        }
        Participation::ConferenceStart {
            group,
            invitees,
            call_type,
            camera_enabled,
            muted,
        } => {
            let title = format!(
                "Conference {} • {}",
                if call_type == CallType::Video {
                    "Video"
                } else {
                    "Voice"
                },
                super::levels::clean_label(&group, 48)
            );
            let intent = ActiveCallIntent {
                header_title: title,
                peer_label: group.clone(),
                is_conference: true,
                video_allowed: call_type == CallType::Video
                    && control.capabilities.conference_video,
                initial_camera: camera_enabled,
                initial_muted: muted,
                camera_source: camera_source.clone(),
            };
            let cmd = CallCommand::Dial {
                request_id: request_id(),
                target: CallTarget::Conference {
                    group_id: group,
                    invitees,
                },
                call_type,
                camera_enabled: (call_type == CallType::Video).then_some(camera_enabled),
                muted: muted.then_some(true),
            };
            (cmd, intent)
        }
        Participation::ConferenceJoin {
            invitation,
            camera_enabled,
            muted,
        } => {
            let title = format!(
                "Conference • {}",
                super::levels::clean_label(&invitation.group_id, 48)
            );
            let intent = ActiveCallIntent {
                header_title: title,
                peer_label: invitation.group_id.clone(),
                is_conference: true,
                video_allowed: control.capabilities.conference_video,
                initial_camera: camera_enabled,
                initial_muted: muted,
                camera_source: camera_source.clone(),
            };
            let cmd = CallCommand::Join {
                request_id: request_id(),
                invitation_id: invitation.invitation_id,
                invitation_revision: invitation.revision,
                camera_enabled: camera_enabled.then_some(true),
                muted: muted.then_some(true),
            };
            (cmd, intent)
        }
        Participation::Answer {
            target,
            camera_enabled,
            muted,
        } => {
            let offer = await_exact_offer(control, &target, cancellation).await?;
            let is_video = offer.call_type == CallType::Video;
            let title = format!(
                "Direct {} Call • {}",
                if is_video { "Video" } else { "Voice" },
                super::levels::clean_label(&offer.caller_id, 48)
            );
            let intent = ActiveCallIntent {
                header_title: title,
                peer_label: offer.caller_id.clone(),
                is_conference: false,
                video_allowed: is_video && control.capabilities.direct_video,
                initial_camera: is_video && camera_enabled,
                initial_muted: muted,
                camera_source: camera_source.clone(),
            };
            let cmd = CallCommand::Answer {
                request_id: request_id(),
                call_id: offer.call_id,
                expected_revision: offer.revision,
                camera_enabled: is_video.then_some(camera_enabled),
                muted: muted.then_some(true),
            };
            (cmd, intent)
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
    let outcome = owned_call(
        remote,
        control,
        &mut call,
        &mut audio,
        intent,
        input,
        cancellation,
    )
    .await;
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
    pub(super) media: Option<MediaClient>,
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
                    "local socket drop after {} PCM writes and {} video writes",
                    media.pcm_writes, media.video_writes
                ),
            );
        }
        result
    }
}

pub(super) async fn attach_media(
    remote: &Remote,
    control: &Control,
    call_id: &str,
    owner_token: &str,
) -> Result<MediaClient> {
    MediaClient::attach(&endpoint(remote)?, control, call_id, owner_token)
        .await
        .map_err(map_client_error)
}

async fn next_media(media: &mut Option<MediaClient>) -> Result<Message> {
    match media {
        Some(media) => media.next_message().await,
        None => std::future::pending().await,
    }
}

enum PendingKind {
    Mute(bool),
    Camera(bool),
    Invite,
}

async fn ensure_video_capture(
    capture: &mut Option<super::video::LocalVideoCapture>,
    camera_enabled: &mut bool,
    source: &super::video::CameraSource,
    media: Option<&MediaClient>,
    levels: &mut super::levels::CallLevels,
) -> bool {
    if !*camera_enabled {
        capture.take();
        return false;
    }
    if capture.is_some() {
        return false;
    }
    let Some(media) = media else {
        return false;
    };
    let Some(format) = media
        .video_send_track
        .as_ref()
        .and_then(CallTrack::video_format)
    else {
        *camera_enabled = false;
        levels.set_camera(Some(false));
        levels.notice("Outgoing video track unavailable; staying on audio.");
        return true;
    };
    let source_owned = source.clone();
    let started = tokio::task::spawn_blocking(move || {
        super::video::LocalVideoCapture::start(&source_owned, format)
    })
    .await
    .map_err(|err| anyhow::anyhow!("camera startup worker failed: {err}"))
    .and_then(|res| res);
    match started {
        Ok(started) => {
            *capture = Some(started);
            levels.notice(&format!(
                "Camera active ({}, {:?}).",
                source.label(),
                format.codec
            ));
            false
        }
        Err(error) => {
            *camera_enabled = false;
            levels.set_camera(Some(false));
            levels.notice(&format!("Camera start failed; staying on audio ({error})."));
            true
        }
    }
}

async fn owned_call(
    remote: &Remote,
    control: &mut Control,
    call: &mut OwnedCall,
    audio: &mut LocalAudio,
    intent: ActiveCallIntent,
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
    let mut muted = intent.initial_muted;
    let mut camera_enabled = intent.initial_camera && intent.video_allowed;
    let mut speaker_enabled = true;
    let mut video_capture: Option<super::video::LocalVideoCapture> = None;
    let mut media_closed_at: Option<Instant> = None;
    let mut media_closed_err: Option<anyhow::Error> = None;
    let mut pending = std::collections::BTreeMap::<String, (Instant, PendingKind)>::new();
    let mut interaction = super::interaction::Interaction::new();
    let mut device_change = DeviceChange(None);
    let mut tick = tokio::time::interval(Duration::from_millis(u64::from(control.format.frame_ms)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut levels = super::levels::CallLevels::new();
    levels.configure_header(
        intent.header_title.clone(),
        intent.video_allowed.then_some(camera_enabled),
        intent.video_allowed.then(|| intent.camera_source.label()),
        intent.is_conference,
    );
    levels.microphone(&[], muted);
    if !levels.is_interactive() {
        eprintln!(
            "Call {}\n{CALL_KEYS}",
            super::levels::clean_label(call_id, 80)
        );
    } else {
        levels.notice(&format!(
            "Connecting {}...",
            super::levels::clean_label(&intent.peer_label, 48)
        ));
    }
    let mut meter_tick = tokio::time::interval(Duration::from_millis(80));
    meter_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        if let Some(view) = control.call.as_ref().filter(|view| view.call_id == call_id) {
            levels.roster(view);
            if let Some(media) = media {
                media.receive.roster(view)?;
                media.receive_video.roster(view);
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
            let (audio_changed, video_changed) = media.synchronize_tracks(control, call_id)?;
            if audio_changed {
                audio.discard_capture();
                levels.clear_microphone();
            }
            if video_changed && camera_enabled {
                if let Some(capture) = video_capture.as_mut() {
                    let _ = capture.request_keyframe();
                }
            }
        }
        if control
            .call
            .as_ref()
            .is_some_and(|view| view.call_id == call_id && view.state == CallState::Connected)
            && media.is_none()
            && media_closed_at.is_none()
        {
            let mut attached = tokio::select! {
                _=cancellation.wait()=>return Ok(false),
                result=attach_media(remote,control,call_id,owner_token)=>result?,
            };
            attached.receive_video.set_peer_label(&intent.peer_label);
            if let Some(view) = control.call.as_ref().filter(|view| view.call_id == call_id) {
                attached.receive.roster(view)?;
                attached.receive_video.roster(view);
            }
            let audio_sources = attached.receive.track_count();
            let video_sources = attached.receive_video.active_track_count();
            audio.discard_capture();
            levels.clear_microphone();
            *media = Some(attached);
            let failed_start = ensure_video_capture(
                &mut video_capture,
                &mut camera_enabled,
                &intent.camera_source,
                media.as_ref(),
                &mut levels,
            )
            .await;
            if failed_start {
                let id = request_id();
                let _ = send_json(
                    &mut control.socket,
                    &CallCommand::Camera {
                        request_id: id,
                        call_id: call_id.into(),
                        enabled: false,
                    },
                )
                .await;
            }
            if video_sources > 0 || intent.video_allowed {
                levels.notice(&format!(
                    "Connected. Audio tracks: {audio_sources}, video tracks: {video_sources}."
                ));
            } else {
                levels.notice(&format!(
                    "Connected. Microphone and speakers active; {audio_sources} receive tracks."
                ));
            }
        }
        tokio::select! {
            _=cancellation.wait()=>return Ok(false),
            line=input.next_line()=>{
                let Some(line)=line? else{return Ok(false);};
                levels.input_received();
                let trimmed=line.trim();
                match trimmed {
                    "l"|"leave"|"h"|"hangup"|"q"=>return Ok(false),
                    "m"|"mute"=>{
                        if pending.values().any(|(_,kind)|matches!(kind,PendingKind::Mute(_))){
                            levels.notice("A microphone request is pending; audio continues.");
                            continue;
                        }
                        let old=muted;
                        muted = !muted;
                        audio.discard_capture();
                        levels.microphone(&[],muted);
                        let id=request_id();
                        send_json(&mut control.socket,&CallCommand::Mute {request_id:id.clone(),call_id:call_id.into(),muted}).await?;
                        pending.insert(id,(Instant::now(),PendingKind::Mute(old)));
                        levels.notice(if muted {"Microphone muted."}else{"Microphone unmuted."});
                    }
                    "v"|"video"|"camera"=>{
                        if !intent.video_allowed {
                            levels.notice("Camera is unavailable in this voice-only call.");
                            continue;
                        }
                        if intent.camera_source.is_off() {
                            levels.notice("Camera source is set to off; start without --camera off.");
                            continue;
                        }
                        if pending.values().any(|(_,kind)|matches!(kind,PendingKind::Camera(_))){
                            levels.notice("A camera request is pending; call continues.");
                            continue;
                        }
                        let old = camera_enabled;
                        let next = !camera_enabled;
                        camera_enabled = next;
                        if !next {
                            video_capture.take();
                            levels.set_camera(Some(false));
                        } else {
                            levels.set_camera(Some(true));
                            let failed = ensure_video_capture(
                                &mut video_capture,
                                &mut camera_enabled,
                                &intent.camera_source,
                                media.as_ref(),
                                &mut levels,
                            )
                            .await;
                            if failed {
                                continue;
                            }
                        }
                        let id = request_id();
                        send_json(
                            &mut control.socket,
                            &CallCommand::Camera {
                                request_id: id.clone(),
                                call_id: call_id.into(),
                                enabled: camera_enabled,
                            },
                        )
                        .await?;
                        pending.insert(id, (Instant::now(), PendingKind::Camera(old)));
                        levels.notice(if camera_enabled {
                            "Camera enabled."
                        } else {
                            "Camera disabled."
                        });
                    }
                    "s"|"speaker"=>{
                        speaker_enabled = !speaker_enabled;
                        audio.playback_token=audio.playback.clear()?;
                        levels.playback_enabled(speaker_enabled);
                        levels.notice(if speaker_enabled {"Local speaker enabled."}else{"Local speaker disabled; participant meters still measure incoming audio."});
                    }
                    "p"|"participants"=>{
                        let mut text=String::from("Participants:\n");
                        if let Some(view)=control.call.as_ref().filter(|view|view.call_id==call_id) {
                            for p in &view.participants {
                                let mic = match p.microphone_enabled {Some(true)=>"on",Some(false)=>"off",None=>"unknown"};
                                let cam = match p.camera_enabled {Some(true)=>", cam on",Some(false)=>", cam off",None=>""};
                                text.push_str(&format!("{}: {:?}; mic {mic}{cam}\n",super::levels::clean_label(&p.contact_id,64),p.membership));
                            }
                        }
                        levels.notice(text.trim_end());
                    }
                    "?"|"help"=>levels.notice(&format!("{CALL_KEYS}\ni searches contacts; invite CONTACT_ID [...] sends selected IDs.\nd lists devices; d input NUMBER / d output NUMBER selects one.")),
                    _=>match interaction.line(trimmed,remote,control.call.as_ref().filter(|view|view.call_id==call_id),&AudioOptions::from(&audio.options),&mut levels) {
                        Ok(super::interaction::UiAction::None)=>{},
                        Ok(super::interaction::UiAction::Invite(contact_ids))=>{
                            if !control.capabilities.conference_invite {levels.notice("Conference invitations unavailable on this daemon.");continue;}
                            let id=request_id();
                            send_json(&mut control.socket,&CallCommand::Invite {request_id:id.clone(),call_id:call_id.into(),contact_ids}).await?;
                            pending.insert(id,(Instant::now(),PendingKind::Invite));
                            levels.notice("Invitation request submitted; media continues while it completes.");
                        }
                        Ok(super::interaction::UiAction::Devices(options))=>{
                            if device_change.0.is_some(){levels.notice("A device change is already preparing.");continue;}
                            let opts = AudioDeviceOptions::from(&options);
                            device_change.0=Some(tokio::spawn(async move{LocalAudio::open(&opts).await}));
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
                        replacement.discard_capture();
                        let summary = replacement.startup_summary.replace('\n', "; ");
                        let mut previous=std::mem::replace(audio,replacement);
                        previous.request_stop();
                        tokio::spawn(async move{let _=previous.stop().await;});
                        levels.clear_microphone();
                        levels.clear_speaker();
                        levels.notice(&format!("Local audio devices changed. {summary}"));
                    }
                    Err(error)=>levels.notice(&format!("Device change failed; current devices retained: {error}")),
                }
            }
            event=control.next_event()=>{
                let event=event?;
                control.update(&event);
                match event {
                    CallEvent::State {call:view} if view.call_id==call_id=>match view.state {
                        CallState::Ended=>{levels.notice("Call ended.");return Ok(true);},
                        CallState::Uncertain=>bail!("call outcome unknown; check calls status before retrying"),
                        _=>{}
                    },
                    CallEvent::Control { call_id: id, participant_id, control: peer_ctrl } if id == call_id => {
                        match peer_ctrl {
                            wechat_core::call_types::CallControl::Camera { enabled } => {
                                levels.notice(&format!(
                                    "{} turned camera {}.",
                                    super::levels::clean_label(&participant_id, 32),
                                    if enabled { "on" } else { "off" }
                                ));
                            }
                            wechat_core::call_types::CallControl::Microphone { muted: peer_muted } => {
                                levels.notice(&format!(
                                    "{} {} microphone.",
                                    super::levels::clean_label(&participant_id, 32),
                                    if peer_muted { "muted" } else { "unmuted" }
                                ));
                            }
                        }
                    }
                    CallEvent::Availability {availability} if !availability.busy=>return Ok(true),
                    CallEvent::CommandResult {request_id,status,code,..}=>{
                        if let Some((_,kind))=pending.remove(&request_id) {
                            if status==wechat_core::call_types::CallCommandStatus::Error {
                                match kind {
                                    PendingKind::Mute(old)=>{muted=old;audio.discard_capture();levels.microphone(&[],muted);}
                                    PendingKind::Camera(old)=>{
                                        camera_enabled=old;
                                        levels.set_camera(Some(old));
                                        if !old {
                                            video_capture.take();
                                        } else {
                                            ensure_video_capture(
                                                &mut video_capture,
                                                &mut camera_enabled,
                                                &intent.camera_source,
                                                media.as_ref(),
                                                &mut levels,
                                            )
                                            .await;
                                        }
                                    }
                                    PendingKind::Invite=>{}
                                }
                                levels.notice(&format!("Command rejected: {}",serde_json::to_value(code)?));
                            }
                        }
                    }
                    CallEvent::Error {code}=>bail!("call error: {}",serde_json::to_value(code)?),
                    _=>{},
                }
            }
            message=next_media(media)=>{
                let message = match message {
                    Ok(msg) => msg,
                    Err(error) => {
                        // A remote hangup closes /media and emits CallState::Ended on /socket
                        // concurrently; wait briefly for /socket to report the authoritative end.
                        *media = None;
                        video_capture.take();
                        media_closed_at.get_or_insert_with(Instant::now);
                        media_closed_err.get_or_insert(error);
                        continue;
                    }
                };
                let current=media.as_mut().context("media detached")?;
                match message {
                    Message::Binary(bytes)=>{
                        let wire = WireMediaFrame::decode(&bytes)?;
                        if wire.kind == MediaPayloadKind::PcmS16le {
                            current.receive.wire_frame(
                                &wire,
                                current.attached_elapsed(),
                                |track_id, participant_id, samples| {
                                    levels.participant_audio(track_id, participant_id, samples);
                                },
                            )?;
                        } else {
                            let _ = current.receive_video.binary_frame(&wire);
                        }
                    }
                    Message::Text(text)=>{
                        let event:CallEvent=serde_json::from_str(&text)?;
                        match &event {
                            CallEvent::TrackAdded {track,..}|CallEvent::TrackUpdated {track,..} if track.direction()==CallTrackDirection::Send=>{
                                control.update(&event);
                            }
                            CallEvent::TrackAdded {..}|CallEvent::TrackUpdated {..}|CallEvent::TrackRemoved {..}=>{
                                current.receive_video.event(&event, call_id);
                                if let Some(view) = control.call.as_ref().filter(|v| v.call_id == call_id) {
                                    current.receive_video.roster(view);
                                }
                                if current.receive.event(&event,call_id)? && matches!(event,CallEvent::TrackRemoved {..}) {
                                    audio.playback_token=audio.playback.clear()?;
                                    levels.clear_speaker();
                                }
                            }
                            CallEvent::VideoKeyframeRequested {..}=>{
                                if let Some(capture) = video_capture.as_mut() {
                                    let _ = capture.request_keyframe();
                                }
                            }
                            CallEvent::Error {code}=>bail!("media error: {}",serde_json::to_value(code)?),
                            _=>bail!("unexpected media lifecycle event"),
                        }
                    }
                    _=>bail!("unexpected call media message"),
                }
            }
            _=meter_tick.tick()=>{
                if media_closed_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(2)) {
                    return Err(media_closed_err
                        .take()
                        .unwrap_or_else(|| anyhow::anyhow!("call media socket closed")));
                }
                if let Some(current) = media.as_mut() {
                    let _ = current.receive_video.poll_decoded();
                }
                levels.refresh_with_video(media.as_ref().map(|m| &m.receive_video));
                let expired=pending.iter().filter(|(_, (started,_))|started.elapsed()>Duration::from_secs(30)).map(|(id,_)|id.clone()).collect::<Vec<_>>();
                for id in expired {
                    pending.remove(&id);
                    levels.notice("Command outcome uncertain; no retry was made. Check the roster/status before submitting again.");
                }
            }
            _=tick.tick()=>{
                let samples=audio.frame(format.samples_per_frame(),muted)?;
                if let Some(current_media) = media.as_mut() {
                    for mixed in current_media.receive.render_due(current_media.attached_elapsed())? {
                        if speaker_enabled {
                            let mixed=mixed.iter().map(|s|f32::from(*s)/32768.0).collect::<Vec<_>>();
                            match audio.playback.enqueue(audio.playback_token,&mixed,format.sample_rate()) {
                                Ok(())=>levels.speaker(&mixed),
                                Err(AudioError::QueueFull)=>{audio.playback_token=audio.playback.clear()?;levels.clear_speaker();},
                                Err(error)=>return Err(error.into()),
                            }
                        }
                    }
                    levels.microphone(&samples,muted);
                    if let Err(error) = current_media.send_pcm(&samples,format).await {
                        *media = None;
                        video_capture.take();
                        media_closed_at.get_or_insert_with(Instant::now);
                        media_closed_err.get_or_insert(error);
                        continue;
                    }
                    let camera_pending = pending
                        .values()
                        .any(|(_, kind)| matches!(kind, PendingKind::Camera(_)));
                    if camera_enabled && !camera_pending {
                        if let Some(frame) = video_capture.as_mut().and_then(|c| c.try_recv()) {
                            if let Err(error) = current_media.send_video(&frame).await {
                                *media = None;
                                video_capture.take();
                                media_closed_at.get_or_insert_with(Instant::now);
                                media_closed_err.get_or_insert(error);
                                continue;
                            }
                        }
                    }
                    if current_media.receive_video.poll_decoded() {
                        levels.mark_video_dirty();
                    }
                    levels.draw_video_if_due(&current_media.receive_video);
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

#[cfg(test)]
mod tests {
    use super::super::tests::{hello, remote};
    use super::*;
    use tokio::sync::oneshot;
    use wechat_core::call_types::{CallCommandStatus, CallKind};

    #[test]
    fn monitor_staging_toggles_mic_and_camera_independently_before_answer_or_join() {
        let mut staging = MonitorStaging::new(false, false, false);
        let video_offer = CallOffer {
            call_id: "video-1".into(),
            revision: 1,
            kind: CallKind::Direct,
            call_type: CallType::Video,
            caller_id: "alice".into(),
        };
        staging.reset_for_offer(&video_offer, false, false, true);
        assert!(!staging.direct_muted);
        assert!(staging.direct_camera);
        assert!(staging.toggle_direct_mic());
        assert_eq!(staging.toggle_direct_camera(&video_offer, true), Ok(false));
        assert_eq!(staging.toggle_direct_camera(&video_offer, true), Ok(true));

        let voice_offer = CallOffer {
            call_type: CallType::Voice,
            ..video_offer.clone()
        };
        staging.reset_for_offer(&voice_offer, false, false, true);
        assert!(!staging.direct_camera);
        assert!(staging.toggle_direct_camera(&voice_offer, true).is_err());
        assert!(staging.toggle_direct_mic());

        // Conference join defaults to camera off and allows toggling both independently.
        assert!(!staging.conference_camera);
        assert_eq!(staging.toggle_conference_camera(true), Ok(true));
        assert!(staging.toggle_conference_mic());
        assert!(staging.toggle_conference_camera(false).is_err());
    }

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
                camera_enabled: None,
                muted: None,
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
