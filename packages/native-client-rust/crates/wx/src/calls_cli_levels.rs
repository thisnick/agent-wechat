//! Signal-only terminal meters and unified voice/video alternate-screen layout.
//! No audio, device volume or call state is retained.
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use wechat_call_client::audio::SignalLevel;
#[cfg(test)]
use wechat_call_client::audio::LEVEL_FLOOR_DB;
use wechat_core::call_types::{
    CallMembership, CallParticipantStatus, CallParticipantStatusKind, CallView,
};

#[cfg(test)]
const FLOOR_DB: f64 = LEVEL_FLOOR_DB;
const MAX_NOTICES: usize = 4;

fn level_text(level: &SignalLevel) -> String {
    let count = (f64::from(level.normalized()) * 10.0)
        .round()
        .clamp(0.0, 10.0) as usize;
    format!(
        "[{}{}] {:>3.0}dB{}",
        "#".repeat(count),
        ".".repeat(10 - count),
        level.db(),
        if level.is_clipping() { "!" } else { " " }
    )
}

struct ParticipantLevel {
    level: SignalLevel,
    label: String,
    membership: CallMembership,
    status: Option<CallParticipantStatus>,
    microphone: Option<bool>,
    camera: Option<bool>,
}

pub(super) fn clean_label(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for c in text.chars().filter(|c| !c.is_control()) {
        let n = c.width().unwrap_or(0);
        if used + n > width {
            break;
        }
        used += n;
        result.push(c);
    }
    result
}

pub(super) struct CallLevels {
    mic: SignalLevel,
    speaker: SignalLevel,
    muted: bool,
    enabled: bool,
    alt_screen: bool,
    prompt_drawn: bool,
    last_size: Option<(usize, usize)>,
    participants: BTreeMap<String, ParticipantLevel>,
    speaker_enabled: bool,
    header_title: String,
    local_camera: Option<bool>,
    camera_source: Option<String>,
    is_conference: bool,
    notices: VecDeque<String>,
    last_draw: Option<Instant>,
    video_dirty: bool,
    #[cfg(windows)]
    console_mode: Option<u32>,
}
impl CallLevels {
    pub(super) fn new() -> Self {
        let meter = Self::with_terminal(terminal_enabled(
            io::stdin().is_terminal(),
            io::stderr().is_terminal(),
            std::env::var("TERM").ok().as_deref(),
        ));
        #[cfg(windows)]
        {
            let mut meter = meter;
            if meter.enabled {
                let (available, restore) = windows_console::enable();
                meter.enabled = available;
                meter.console_mode = restore;
            }
            meter
        }
        #[cfg(not(windows))]
        {
            meter
        }
    }
    fn with_terminal(enabled: bool) -> Self {
        Self {
            mic: SignalLevel::default(),
            speaker: SignalLevel::default(),
            muted: false,
            enabled,
            alt_screen: false,
            prompt_drawn: false,
            last_size: None,
            participants: BTreeMap::new(),
            speaker_enabled: true,
            header_title: String::from("Call"),
            local_camera: None,
            camera_source: None,
            is_conference: false,
            notices: VecDeque::new(),
            last_draw: None,
            video_dirty: false,
            #[cfg(windows)]
            console_mode: None,
        }
    }
    pub(super) fn configure_header(
        &mut self,
        title: String,
        local_camera: Option<bool>,
        camera_source: Option<String>,
        is_conference: bool,
    ) {
        self.header_title = clean_label(&title, 96);
        self.local_camera = local_camera;
        self.camera_source = camera_source.map(|s| clean_label(&s, 32));
        self.is_conference = is_conference;
    }
    pub(super) fn set_camera(&mut self, enabled: Option<bool>) {
        self.local_camera = enabled;
    }
    pub(super) fn is_interactive(&self) -> bool {
        self.enabled
    }
    pub(super) fn microphone(&mut self, samples: &[i16], muted: bool) {
        self.muted = muted;
        if muted {
            self.mic.reset();
        } else {
            self.mic.observe_i16(samples);
        }
    }
    pub(super) fn speaker(&mut self, samples: &[f32]) {
        self.speaker.observe_f32(samples);
    }
    pub(super) fn clear_microphone(&mut self) {
        self.mic.reset();
    }
    pub(super) fn clear_speaker(&mut self) {
        self.speaker.reset();
    }
    fn line(&self) -> String {
        format!(
            "Mic{} {} Speaker{} {}",
            if self.muted { "(muted)" } else { "" },
            level_text(&self.mic),
            if self.speaker_enabled { "" } else { "(off)" },
            level_text(&self.speaker)
        )
    }
    pub(super) fn roster(&mut self, call: &CallView) {
        self.is_conference = call.kind == wechat_core::call_types::CallKind::Conference;
        for participant in &call.participants {
            let row = self
                .participants
                .entry(participant.participant_id.clone())
                .or_insert_with(|| ParticipantLevel {
                    level: SignalLevel::default(),
                    label: String::new(),
                    membership: CallMembership::Unknown,
                    status: None,
                    microphone: None,
                    camera: None,
                });
            row.label = clean_label(&participant.contact_id, 64);
            row.membership = participant.membership;
            row.status = participant.status;
            row.microphone = participant.microphone_enabled;
            row.camera = participant.camera_enabled;
            if participant.membership == CallMembership::Left
                || participant.microphone_enabled == Some(false)
            {
                row.level.reset();
            }
        }
    }
    pub(super) fn participant_audio(
        &mut self,
        track: u32,
        participant: Option<&str>,
        samples: &[i16],
    ) {
        if !self.is_conference && participant.is_none() {
            return;
        }
        let id = participant
            .map(str::to_owned)
            .unwrap_or_else(|| format!("track-{track}"));
        if self.participants.len() >= 128 && !self.participants.contains_key(&id) {
            return;
        }
        let row = self
            .participants
            .entry(id.clone())
            .or_insert_with(|| ParticipantLevel {
                level: SignalLevel::default(),
                label: if participant.is_some() {
                    clean_label(&id, 64)
                } else {
                    format!("Unattributed track {track}")
                },
                membership: CallMembership::Unknown,
                status: None,
                microphone: None,
                camera: None,
            });
        if row.microphone != Some(false) && row.membership != CallMembership::Left {
            row.level.observe_i16(samples);
        }
    }
    pub(super) fn playback_enabled(&mut self, enabled: bool) {
        self.speaker_enabled = enabled;
        if !enabled {
            self.clear_speaker();
        }
    }
    #[cfg(test)]
    pub(super) fn for_test() -> Self {
        Self::with_terminal(false)
    }
    #[cfg(test)]
    pub(super) fn participant_level(&self, id: &str) -> f64 {
        self.participants.get(id).unwrap().level.db()
    }
    fn lines(&self, width: usize, rows: usize) -> Vec<String> {
        let mut lines = vec![clean_label(&self.line(), width.saturating_sub(1))];
        let limit = rows.saturating_sub(5).max(1);
        for row in self.participants.values().take(limit) {
            let membership = match row.membership {
                CallMembership::Joined => "joined",
                CallMembership::Invited => "invited",
                CallMembership::Left => "left",
                CallMembership::Unknown => "unknown",
            };
            let status = match row.status {
                Some(status) => match status.kind {
                    CallParticipantStatusKind::Undefined => "undefined".to_owned(),
                    CallParticipantStatusKind::Joined => "joined".to_owned(),
                    CallParticipantStatusKind::Invited => "invited".to_owned(),
                    CallParticipantStatusKind::Acknowledged => "acknowledged".to_owned(),
                    CallParticipantStatusKind::Left => "left".to_owned(),
                    CallParticipantStatusKind::Cancelled => "cancelled".to_owned(),
                    CallParticipantStatusKind::Busy => "busy".to_owned(),
                    CallParticipantStatusKind::Rejected => "rejected".to_owned(),
                    CallParticipantStatusKind::NoResponse => "no response".to_owned(),
                    CallParticipantStatusKind::Unknown => format!("unknown ({})", status.code),
                },
                None => membership.to_owned(),
            };
            let mic = match row.microphone {
                Some(true) => "mic on",
                Some(false) => "mic off",
                None => "mic unknown",
            };
            let cam = match row.camera {
                Some(true) => " cam on",
                Some(false) => " cam off",
                None => "",
            };
            let tail = format!(" {} {status} {mic}{cam}", level_text(&row.level));
            let label = clean_label(&row.label, width.saturating_sub(tail.width() + 2));
            lines.push(clean_label(
                &format!("{label}{tail}"),
                width.saturating_sub(1),
            ));
        }
        if self.participants.len() > limit {
            lines.push(format!(
                "+{} participants; p shows the full roster",
                self.participants.len() - limit
            ));
        }
        lines
    }
    fn header_bar(&self, width: usize) -> String {
        let mic = if self.muted { "Mic: MUTED" } else { "Mic: ON" };
        let spk = if self.speaker_enabled {
            "Spk: ON"
        } else {
            "Spk: OFF"
        };
        let cam = match (self.local_camera, self.camera_source.as_deref()) {
            (Some(true), Some(src)) => format!("Cam: ON ({src})"),
            (Some(true), None) => "Cam: ON".into(),
            (Some(false), _) => "Cam: OFF".into(),
            (None, _) => "Cam: N/A".into(),
        };
        let raw = format!(" {}  |  {mic}  |  {cam}  |  {spk} ", self.header_title);
        let clipped = clean_label(&raw, width);
        let fill = width.saturating_sub(clipped.width());
        format!(
            "\x1b[1;38;2;245;248;252;48;2;28;56;88m{clipped}{}\x1b[0m",
            " ".repeat(fill)
        )
    }
    fn controls_bar(&self, width: usize) -> String {
        let raw = match (self.local_camera.is_some(), self.is_conference) {
            (true, true) => {
                " [m] Mic  [v] Cam  [s] Spk  [i] Invite  [p] Roster  [d] Devices  [l/h] Leave  [?] Help "
            }
            (true, false) => {
                " [m] Mic  [v] Cam  [s] Spk  [d] Devices  [h] Hangup  [?] Help "
            }
            (false, true) => {
                " [m] Mic  [s] Spk  [i] Invite  [p] Roster  [d] Devices  [l/h] Leave  [?] Help "
            }
            (false, false) => " [m] Mic  [s] Spk  [d] Devices  [h] Hangup  [?] Help ",
        };
        let clipped = clean_label(raw, width);
        let fill = width.saturating_sub(clipped.width());
        format!(
            "\x1b[38;2;200;210;224;48;2;32;36;46m{clipped}{}\x1b[0m",
            " ".repeat(fill)
        )
    }
    fn screen_rows(
        &self,
        width: usize,
        rows: usize,
        video: Option<&super::video::ReceiveVideo>,
    ) -> Vec<String> {
        let body_budget = rows.saturating_sub(1); // Reserve last row for "> " prompt.
        let meter_and_roster = self.lines(width, body_budget.min(10));
        let notice_count = self.notices.len().min(3);
        let fixed_rows = 1 /* header */ + meter_and_roster.len() + notice_count + 1 /* controls */;
        let video_budget = body_budget.saturating_sub(fixed_rows + 1);

        let mut frame = Vec::with_capacity(body_budget);
        frame.push(self.header_bar(width));

        if let Some(video) = video.filter(|v| v.has_visible_video()) {
            let vrows = super::video::render_rows(video, width, video_budget);
            if !vrows.is_empty() {
                frame.extend(vrows);
            }
        }

        // Subtle divider before audio meters when video is shown.
        if frame.len() > 1 && frame.len() + meter_and_roster.len() + notice_count + 1 < body_budget
        {
            frame.push(format!(
                "\x1b[38;2;74;84;100m{}\x1b[0m",
                "─".repeat(width.saturating_sub(1))
            ));
        }

        for line in meter_and_roster {
            if frame.len() + 1 < body_budget {
                frame.push(line);
            }
        }

        let start_notice = self.notices.len().saturating_sub(notice_count);
        for notice in self.notices.iter().skip(start_notice) {
            if frame.len() + 1 < body_budget {
                let clipped = clean_label(notice, width.saturating_sub(2));
                frame.push(format!("\x1b[38;2;152;195;235m• {clipped}\x1b[0m"));
            }
        }

        // Pad empty rows so the controls bar sits neatly right above the prompt.
        while frame.len() + 1 < body_budget {
            frame.push(String::new());
        }
        if frame.len() < body_budget {
            frame.push(self.controls_bar(width));
        }
        frame
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn refresh(&mut self) {
        self.refresh_with_video(None);
    }
    pub(super) fn refresh_with_video(&mut self, video: Option<&super::video::ReceiveVideo>) {
        self.mic.advance();
        self.speaker.advance();
        for row in self.participants.values_mut() {
            row.level.advance();
        }
        self.draw(video);
    }
    /// Record that a freshly decoded video frame is ready to be painted on the next due slot.
    pub(super) fn mark_video_dirty(&mut self) {
        self.video_dirty = true;
    }
    /// Repaint the screen when a fresh video frame is pending and the minimum
    /// inter-frame interval (~33 ms / 30 fps cap) has elapsed, without stepping
    /// audio-meter decay faster than the meter tick.
    pub(super) fn draw_video_if_due(&mut self, video: &super::video::ReceiveVideo) {
        if !self.enabled || !self.video_dirty {
            return;
        }
        if self
            .last_draw
            .is_some_and(|last| last.elapsed() < Duration::from_millis(33))
        {
            return;
        }
        self.draw(Some(video));
    }
    fn draw(&mut self, video: Option<&super::video::ReceiveVideo>) {
        if !self.enabled {
            return;
        }
        let Some(columns) = terminal_columns().filter(|width| *width >= 32) else {
            self.leave_alt_screen();
            return;
        };
        let rows = terminal_rows().max(10);
        let resized = self.last_size != Some((columns, rows));
        self.last_size = Some((columns, rows));
        let frame = self.screen_rows(columns, rows, video);
        let mut out = io::stderr().lock();
        let mut buf = String::with_capacity(columns * rows * 4);
        buf.push_str("\x1b[?2026h");
        if !self.alt_screen {
            buf.push_str("\x1b[?1049h\x1b[2J");
            self.alt_screen = true;
            self.prompt_drawn = false;
        }
        if self.prompt_drawn && !resized {
            buf.push_str("\x1b7");
            for (idx, line) in frame.iter().enumerate() {
                let row = idx + 1;
                buf.push_str(&format!("\x1b[{row};1H\x1b[2K"));
                buf.push_str(line);
                buf.push_str("\x1b[0m");
            }
            for row in (frame.len() + 1)..rows {
                buf.push_str(&format!("\x1b[{row};1H\x1b[2K"));
            }
            buf.push_str("\x1b8");
        } else {
            for (idx, line) in frame.iter().enumerate() {
                let row = idx + 1;
                buf.push_str(&format!("\x1b[{row};1H\x1b[2K"));
                buf.push_str(line);
                buf.push_str("\x1b[0m");
            }
            for row in (frame.len() + 1)..rows {
                buf.push_str(&format!("\x1b[{row};1H\x1b[2K"));
            }
            buf.push_str(&format!("\x1b[{rows};1H\x1b[2K> "));
            self.prompt_drawn = true;
        }
        buf.push_str("\x1b[?2026l");
        let result = write!(out, "{buf}").and_then(|_| out.flush());
        if result.is_err() {
            self.enabled = false;
        } else {
            self.last_draw = Some(Instant::now());
            self.video_dirty = false;
        }
    }
    /// The terminal echoed Enter on the bottom prompt line; repaint cleanly next tick.
    pub(super) fn input_received(&mut self) {
        if self.enabled && self.alt_screen {
            self.prompt_drawn = false;
        }
    }
    pub(super) fn notice(&mut self, message: &str) {
        if !self.enabled {
            eprintln!("{message}");
            return;
        }
        for line in message.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if self.notices.len() >= MAX_NOTICES {
                self.notices.pop_front();
            }
            self.notices.push_back(trimmed.to_owned());
        }
    }
    fn leave_alt_screen(&mut self) {
        if self.alt_screen {
            let mut out = io::stderr().lock();
            let _ = write!(out, "\x1b[0m\x1b[?1049l");
            let _ = out.flush();
            self.alt_screen = false;
            self.prompt_drawn = false;
        }
    }
}
impl Drop for CallLevels {
    fn drop(&mut self) {
        self.leave_alt_screen();
        #[cfg(windows)]
        if let Some(mode) = self.console_mode {
            windows_console::restore(mode);
        }
    }
}

fn terminal_enabled(input: bool, output: bool, term: Option<&str>) -> bool {
    input && output && term != Some("dumb")
}

#[cfg(test)]
fn fits_width(line: &str, columns: Option<usize>) -> bool {
    columns.is_some_and(|width| line.len() < width)
}

#[cfg(unix)]
fn terminal_rows() -> usize {
    use std::os::fd::AsRawFd;
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    if unsafe {
        libc::ioctl(
            io::stderr().as_raw_fd(),
            libc::TIOCGWINSZ,
            size.as_mut_ptr(),
        )
    } == 0
    {
        let rows = unsafe { size.assume_init() }.ws_row;
        if rows > 0 {
            return usize::from(rows);
        }
    }
    24
}
#[cfg(not(unix))]
fn terminal_rows() -> usize {
    24
}

#[cfg(unix)]
fn terminal_columns() -> Option<usize> {
    use std::os::fd::AsRawFd;
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // ioctl writes only this initialized winsize; the stderr handle is borrowed.
    if unsafe {
        libc::ioctl(
            io::stderr().as_raw_fd(),
            libc::TIOCGWINSZ,
            size.as_mut_ptr(),
        )
    } != 0
    {
        return None;
    }
    let width = unsafe { size.assume_init() }.ws_col;
    (width > 0).then_some(usize::from(width))
}

#[cfg(windows)]
fn terminal_columns() -> Option<usize> {
    windows_console::columns()
}

#[cfg(not(any(unix, windows)))]
fn terminal_columns() -> Option<usize> {
    None
}

#[cfg(windows)]
mod windows_console {
    use std::{io, os::windows::io::AsRawHandle};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetConsoleScreenBufferInfo, SetConsoleMode, CONSOLE_SCREEN_BUFFER_INFO,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };
    pub(super) fn enable() -> (bool, Option<u32>) {
        let handle = io::stderr().as_raw_handle();
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return (false, None);
        }
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            return (true, None);
        }
        (
            unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } != 0,
            Some(mode),
        )
    }
    pub(super) fn restore(mode: u32) {
        unsafe {
            SetConsoleMode(io::stderr().as_raw_handle(), mode);
        }
    }
    pub(super) fn columns() -> Option<usize> {
        let mut info = std::mem::MaybeUninit::<CONSOLE_SCREEN_BUFFER_INFO>::zeroed();
        if unsafe { GetConsoleScreenBufferInfo(io::stderr().as_raw_handle(), info.as_mut_ptr()) }
            == 0
        {
            return None;
        }
        let info = unsafe { info.assume_init() };
        let width = info.srWindow.Right - info.srWindow.Left + 1;
        (width > 0).then_some(width as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signal_levels_follow_audio_silence_mute_and_terminal_policy() {
        let mut meter = CallLevels::with_terminal(false);
        meter.microphone(&[16384; 320], false);
        meter.speaker(&[0.25; 320]);
        meter.refresh();
        assert!((meter.mic.db() + 6.02).abs() < 0.1);
        assert!((meter.speaker.db() + 12.04).abs() < 0.1);
        assert!(meter.line().contains("-6dB"));
        assert!(!meter.alt_screen);
        meter.speaker(&[1.0; 320]);
        meter.refresh();
        assert_eq!(meter.speaker.clip_ticks(), 10);
        meter.microphone(&[32767; 320], true);
        meter.refresh();
        assert_eq!(meter.mic.db(), FLOOR_DB);
        assert!(meter.line().contains("Mic(muted)"));
        for _ in 0..10 {
            meter.refresh();
        }
        assert_eq!(meter.speaker.db(), FLOOR_DB);
        assert_eq!(meter.speaker.clip_ticks(), 0);
        meter.microphone(&[-32768; 320], false);
        meter.refresh();
        assert_eq!(meter.mic.db(), 0.0);
        meter.clear_microphone();
        meter.clear_speaker();
        assert_eq!(meter.mic.db(), FLOOR_DB);
        assert_eq!(meter.speaker.db(), FLOOR_DB);
        assert!(terminal_enabled(true, true, Some("xterm-256color")));
        assert!(!terminal_enabled(true, true, Some("dumb")));
        assert!(!terminal_enabled(false, true, None));
        assert!(!terminal_enabled(true, false, None));
        assert!(fits_width(&meter.line(), Some(80)));
        assert!(!fits_width(&meter.line(), Some(32)));
        assert!(!fits_width(&meter.line(), Some(meter.line().len())));
        assert!(!fits_width(&meter.line(), None));
    }
}

#[cfg(test)]
mod participant_tests {
    use super::*;
    use wechat_core::call_types::{CallDirection, CallKind, CallParticipant, CallState, CallType};
    #[test]
    fn roster_displays_signaling_outcomes_without_inventing_membership() {
        let mut levels = CallLevels::for_test();
        levels.participants.insert(
            "pending".into(),
            ParticipantLevel {
                level: SignalLevel::default(),
                label: "Contact".into(),
                membership: CallMembership::Invited,
                status: Some(CallParticipantStatus {
                    code: 3,
                    kind: CallParticipantStatusKind::Acknowledged,
                }),
                microphone: None,
                camera: None,
            },
        );
        assert!(levels.lines(120, 24)[1].contains("acknowledged"));
        for (code, kind, label) in [
            (5, CallParticipantStatusKind::Cancelled, "cancelled"),
            (6, CallParticipantStatusKind::Busy, "busy"),
            (7, CallParticipantStatusKind::Rejected, "rejected"),
            (8, CallParticipantStatusKind::NoResponse, "no response"),
            (10, CallParticipantStatusKind::Unknown, "unknown (10)"),
        ] {
            levels.participants.get_mut("pending").unwrap().status =
                Some(CallParticipantStatus { code, kind });
            assert!(levels.lines(120, 24)[1].contains(label));
            assert_eq!(
                levels.participants["pending"].membership,
                CallMembership::Invited
            );
        }
    }

    #[test]
    fn participant_meters_keep_mute_membership_and_signal_separate() {
        let mut levels = CallLevels::for_test();
        let mut call = CallView {
            call_id: "test".into(),
            revision: 1,
            kind: CallKind::Conference,
            call_type: CallType::Voice,
            direction: CallDirection::Incoming,
            state: CallState::Connected,
            owned_by_this_client: true,
            group_id: Some("group".into()),
            controls: vec![],
            participants: vec![CallParticipant {
                participant_id: "person".into(),
                contact_id: "Alice\x1b[31m".into(),
                membership: CallMembership::Joined,
                status: None,
                microphone_enabled: Some(true),
                camera_enabled: Some(true),
            }],
        };
        levels.roster(&call);
        levels.participant_audio(7, Some("person"), &[16000; 320]);
        levels.refresh();
        assert!(levels.participant_level("person") > -10.0);
        assert!(levels.lines(96, 24)[1].contains("cam on"));
        call.participants[0].microphone_enabled = Some(false);
        call.participants[0].camera_enabled = Some(false);
        levels.roster(&call);
        levels.participant_audio(7, Some("person"), &[16000; 320]);
        levels.refresh();
        assert_eq!(levels.participant_level("person"), FLOOR_DB);
        assert!(levels.lines(96, 24)[1].contains("mic off"));
        assert!(levels.lines(96, 24)[1].contains("cam off"));
        assert!(!levels.lines(96, 24)[1].contains('\x1b'));
        assert_eq!(
            levels.participants["person"].membership,
            CallMembership::Joined
        );
        call.participants[0].microphone_enabled = None;
        call.participants[0].camera_enabled = None;
        levels.roster(&call);
        assert!(levels.lines(80, 24)[1].contains("mic unknown"));
        levels.playback_enabled(false);
        levels.participant_audio(7, Some("person"), &[16000; 320]);
        levels.refresh();
        assert!(levels.participant_level("person") > -10.0);
        assert!(levels.line().contains("Speaker(off)"));
        assert_eq!(clean_label("你好 Alice", 5), "你好 ");
        assert!(levels.lines(32, 10).iter().all(|line| line.width() < 32));
    }

    #[test]
    fn unified_screen_shows_header_controls_and_collapses_video_for_voice() {
        let mut levels = CallLevels::for_test();
        levels.configure_header(
            "Direct Video Call • alice".into(),
            Some(true),
            Some("test".into()),
            false,
        );
        let rows = levels.screen_rows(80, 24, None);
        assert_eq!(rows.len(), 23);
        assert!(rows[0].contains("Direct Video Call • alice"));
        assert!(rows[0].contains("Cam: ON (test)"));
        assert!(rows.last().unwrap().contains("[v] Cam"));
    }
}
