//! Interactive local devices. No stream is opened until dial or an accepted prompt.
use super::*;
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
use wechat_calls::{
    audio::AudioFormat,
    frame::{encode_pcm, MediaFrame, MediaHeader, MediaStream},
};
use wechat_core::call_types::{
    CallMedia, CallMediaCommand, CallReadyScope, CallState, CallTarget, CallTrackDirection,
};

type Input = Lines<BufReader<Stdin>>;

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
    if !control.capabilities.direct_audio {
        bail!("direct voice calling is unavailable on this daemon");
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
        CallsCommand::Status => unreachable!("status is handled without devices"),
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
    eprintln!("Waiting for calls. Microphone is closed. Ctrl-C exits.");
    let mut shown = None;
    let mut dismissed = None;
    let mut renew_after_terminal = false;
    loop {
        if let Some(offer) = &control.offer {
            let identity = (offer.call_id.clone(), offer.revision);
            if shown.as_ref() != Some(&identity) && dismissed.as_ref() != Some(&identity) {
                eprintln!("Incoming voice call from {} ({})\n[a] Accept  [d] Dismiss here  [r] Reject call", offer.caller_id, offer.call_id);
                shown = Some(identity);
            }
        }
        tokio::select! {
            _ = cancellation.wait() => return Ok(()),
            event = receive_event(&mut control.socket) => {
                let event=event?; control.update(&event);
                if renew_after_terminal&&matches!(event,CallEvent::Availability {availability} if !availability.busy) {
                    renew_after_terminal=false;restore_monitor_readiness(control).await?;
                }
            }
            line = input.next_line() => {
                let Some(line)=line? else { return Ok(()); };
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
                        eprintln!("Waiting for calls. Microphone is closed. Ctrl-C exits.");
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
                event=receive_event(&mut control.socket)=>event?,
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
    eprintln!("Opening this computer's microphone and speakers…");
    let mut audio = tokio::select! {
        _=cancellation.wait()=>return Err(failure(130,"client_interrupted","device preparation cancelled; no call was placed or answered")),
        audio=LocalAudio::open(options)=>audio?,
    };
    let scope = match &action {
        Participation::Dial(_) => CallReadyScope::Outgoing,
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
            media: CallMedia::default(),
        },
        Participation::Answer(target) => {
            let offer = await_exact_offer(control, &target, cancellation).await?;
            CallCommand::Answer {
                request_id: request_id(),
                call_id: offer.call_id,
                expected_revision: offer.revision,
                media: CallMedia::default(),
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
    let outcome = owned_call(
        remote,
        control,
        &call_id,
        &owner_token,
        &mut audio,
        input,
        cancellation,
    )
    .await;
    audio.request_stop();
    // Any local failure requests termination; a lost connection or response is
    // reported as uncertain. An accepted command alone never proves remote end.
    // Teardown is issued once even when the user signal remains latched. Its
    // existing eight-second limit still bounds an unresponsive remote operation.
    let ended = if matches!(&outcome, Ok(true)) {
        Ok(())
    } else {
        terminate_owned(control, &call_id).await
    };
    let stopped = audio.stop().await;
    ended?;
    stopped?;
    outcome.map(|_| ())
}

pub(super) struct Media {
    socket: Socket,
    send_track: wechat_core::call_types::CallAudioTrack,
    receive: MediaStream,
    receive_track: u32,
    receive_generation: u32,
    sequence: u64,
    timestamp: u64,
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
                client_id: control.client_id.clone(),
                owner_token: owner_token.into(),
            },
        )
        .await?;
        let event = timeout(STARTUP_TIMEOUT, receive_event(&mut socket))
            .await
            .context("media attachment timed out")??;
        if let CallEvent::Error { code } = &event {
            bail!("media attachment rejected: {}", serde_json::to_value(code)?);
        }
        let CallEvent::MediaReady {
            send_track,
            receive_track,
        } = event
        else {
            bail!("media attachment was not accepted");
        };
        if send_track.format != control.format
            || receive_track.format != control.format
            || send_track.direction != CallTrackDirection::Send
            || receive_track.direction != CallTrackDirection::Receive
            || send_track.id == receive_track.id
            || send_track.id == 0
            || send_track.generation == 0
        {
            bail!("unsupported call audio tracks");
        }
        Ok(Self {
            socket,
            receive: MediaStream::new(receive_track.id, receive_track.generation)?,
            receive_track: receive_track.id,
            receive_generation: receive_track.generation,
            send_track,
            sequence: 0,
            timestamp: 0,
        })
    }
    pub(super) fn synchronize_tracks(
        &mut self,
        control: &Control,
        call_id: &str,
    ) -> Result<(bool, bool)> {
        let mut send_changed = false;
        let mut receive_changed = false;
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
                    self.sequence = 0;
                    self.timestamp = 0;
                    send_changed = true;
                }
                CallTrackDirection::Receive
                    if track.id == self.receive_track
                        && track.generation > self.receive_generation =>
                {
                    self.receive.advance_generation(track.generation)?;
                    self.receive_generation = track.generation;
                    receive_changed = true;
                }
                _ => {}
            }
        }
        Ok((send_changed, receive_changed))
    }
    pub(super) async fn send(&mut self, samples: &[i16], format: AudioFormat) -> Result<()> {
        let frame = encode_pcm(
            MediaHeader {
                track_id: self.send_track.id,
                generation: self.send_track.generation,
                sequence: self.sequence,
                timestamp_micros: self.timestamp,
            },
            format,
            samples,
        )?;
        timeout(
            WRITE_TIMEOUT,
            self.socket.send(Message::Binary(frame.into())),
        )
        .await
        .context("call media write timed out")??;
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("audio sequence exhausted")?;
        self.timestamp = self
            .timestamp
            .checked_add(u64::from(format.frame_ms()) * 1000)
            .context("audio timestamp exhausted")?;
        Ok(())
    }
}

async fn next_media(media: &mut Option<Media>) -> Result<Message> {
    match media {
        Some(media) => receive(&mut media.socket).await,
        None => std::future::pending().await,
    }
}

async fn owned_call(
    remote: &Remote,
    control: &mut Control,
    call_id: &str,
    owner_token: &str,
    audio: &mut LocalAudio,
    input: &mut Input,
    cancellation: &Cancellation,
) -> Result<bool> {
    let format = AudioFormat::new(
        control.format.sample_rate,
        control.format.channels,
        control.format.frame_ms,
    )?;
    let mut media: Option<Media> = None;
    let mut muted = false;
    let mut tick = tokio::time::interval(Duration::from_millis(u64::from(control.format.frame_ms)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    eprintln!("Call {call_id}. [m] Mute/unmute  [h] Hang up  Ctrl-C ends this call.");
    loop {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        if let Some(media) = &mut media {
            let (send_changed, receive_changed) = media.synchronize_tracks(control, call_id)?;
            if send_changed {
                audio.discard_capture();
            }
            if receive_changed {
                audio.playback_token = audio.playback.clear()?;
            }
        }
        if let Some(call) = control.call.as_ref().filter(|call| call.call_id == call_id) {
            match call.state {
                CallState::Ended => return Ok(true),
                CallState::Uncertain => {
                    bail!("call outcome unknown; check calls status before retrying")
                }
                _ => {}
            }
        }
        if control
            .call
            .as_ref()
            .is_some_and(|call| call.call_id == call_id && call.state == CallState::Connected)
            && media.is_none()
        {
            let attached = tokio::select! {
                _=cancellation.wait()=>return Ok(false),
                attached=Media::attach(remote,control,call_id,owner_token)=>attached?,
            };
            audio.discard_capture();
            media = Some(attached);
            eprintln!("Connected. Local microphone and speakers are active.");
        }
        tokio::select! {
            _=cancellation.wait()=>return Ok(false),
            line=input.next_line()=> {
                let Some(line)=line? else {return Ok(false);};
                match line.trim().to_ascii_lowercase().as_str() {
                    "h"|"hangup"|"q"=>return Ok(false),
                    "m"|"mute"=> {
                        muted = !muted;
                        control.command(CallCommand::Mute {request_id:request_id(),call_id:call_id.into(),muted}).await?;
                        eprintln!("Microphone {}.",if muted {"muted"} else {"unmuted"});
                    }
                    _=>eprintln!("Choose m to mute/unmute or h to hang up."),
                }
            }
            event=receive_event(&mut control.socket)=> {
                let event=event?;
                control.update(&event);
                match event {
                    CallEvent::State {call} if call.call_id==call_id=> match call.state {
                        CallState::Ended=>{eprintln!("Call ended.");return Ok(true);},
                        CallState::Uncertain=>bail!("call outcome unknown; check calls status before retrying"),
                        _=>{},
                    },
                    CallEvent::Availability {availability} if !availability.busy=>return Ok(true),
                    CallEvent::Error {code}=>bail!("call error: {}",serde_json::to_value(code)?),
                    _=>{},
                }
            }
            message=next_media(&mut media)=> {
                let Message::Binary(bytes)=message? else {bail!("unexpected call media message");};
                let frame=MediaFrame::decode(&bytes,format)?;
                media.as_mut().expect("media event has an attachment").receive.admit(&frame)?;
                let samples:Vec<f32>=frame.samples().map(|sample|f32::from(sample)/32768.0).collect();
                match audio.playback.enqueue(audio.playback_token,&samples,format.sample_rate()) {
                    Ok(())=>{},
                    Err(AudioError::QueueFull)=>{audio.playback_token=audio.playback.clear()?;},
                    Err(error)=>return Err(error.into()),
                }
            }
            _=tick.tick()=> {
                let samples=audio.frame(format.samples_per_frame(),muted)?;
                if let Some(media)=&mut media {media.send(&samples,format).await?;}
            }
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
            let event = receive_event(&mut control.socket).await?;
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
            let event = receive_event(&mut control.socket).await?;
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
    use super::*;
    #[test]
    fn late_answer_requires_the_original_offer() {
        assert!(exact_offer(None, None).is_err());
        let offer = CallOffer {
            call_id: "offer-a".into(),
            revision: 7,
            kind: wechat_core::call_types::CallKind::Direct,
            caller_id: "alice".into(),
            offered_media: CallMedia::default(),
        };
        assert!(exact_offer(Some(&offer), Some("offer-b")).is_err());
        assert_eq!(
            exact_offer(Some(&offer), Some("offer-a")).unwrap().revision,
            7
        );
    }
}
