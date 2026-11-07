//! Signal-only terminal meters. No audio, device volume or call state is retained.
use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use wechat_core::call_types::{
    CallMembership, CallParticipantStatus, CallParticipantStatusKind, CallView,
};

const FLOOR_DB: f64 = -60.0;

struct Level {
    squares: f64,
    count: usize,
    peak: f64,
    db: f64,
    clip_ticks: u8,
}
impl Default for Level {
    fn default() -> Self {
        Self {
            squares: 0.0,
            count: 0,
            peak: 0.0,
            db: FLOOR_DB,
            clip_ticks: 0,
        }
    }
}
impl Level {
    fn observe(&mut self, samples: impl Iterator<Item = f64>) {
        for sample in samples {
            let value = sample.clamp(-1.0, 1.0);
            self.squares += value * value;
            self.peak = self.peak.max(value.abs());
            self.count += 1;
        }
    }
    fn advance(&mut self) {
        let rms = if self.count == 0 {
            0.0
        } else {
            (self.squares / self.count as f64).sqrt()
        };
        let db = if rms > 0.0 {
            (20.0 * rms.log10()).max(FLOOR_DB)
        } else {
            FLOOR_DB
        };
        // Immediate attack; a quiet signal falls 8 dB per 100 ms refresh.
        self.db = db.max(self.db - 8.0);
        self.clip_ticks = if self.peak >= 0.999 {
            10
        } else {
            self.clip_ticks.saturating_sub(1)
        };
        self.squares = 0.0;
        self.count = 0;
        self.peak = 0.0;
    }
    fn text(&self) -> String {
        let count = (((self.db - FLOOR_DB) / -FLOOR_DB) * 10.0)
            .round()
            .clamp(0.0, 10.0) as usize;
        format!(
            "[{}{}] {:>3.0}dB{}",
            "#".repeat(count),
            ".".repeat(10 - count),
            self.db,
            if self.clip_ticks > 0 { "!" } else { " " }
        )
    }
}

struct ParticipantLevel {
    level: Level,
    label: String,
    membership: CallMembership,
    status: Option<CallParticipantStatus>,
    microphone: Option<bool>,
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
    mic: Level,
    speaker: Level,
    muted: bool,
    enabled: bool,
    drawn: bool,
    drawn_rows: usize,
    participants: BTreeMap<String, ParticipantLevel>,
    speaker_enabled: bool,
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
            mic: Level::default(),
            speaker: Level::default(),
            muted: false,
            enabled,
            drawn: false,
            drawn_rows: 0,
            participants: BTreeMap::new(),
            speaker_enabled: true,
            #[cfg(windows)]
            console_mode: None,
        }
    }
    pub(super) fn microphone(&mut self, samples: &[i16], muted: bool) {
        self.muted = muted;
        if muted {
            self.mic = Level::default();
        } else {
            self.mic
                .observe(samples.iter().map(|v| f64::from(*v) / 32768.0));
        }
    }
    pub(super) fn speaker(&mut self, samples: &[f32]) {
        self.speaker.observe(samples.iter().map(|v| f64::from(*v)));
    }
    pub(super) fn clear_microphone(&mut self) {
        self.mic = Level::default();
    }
    pub(super) fn clear_speaker(&mut self) {
        self.speaker = Level::default();
    }
    fn line(&self) -> String {
        format!(
            "Mic{} {} Speaker{} {}",
            if self.muted { "(muted)" } else { "" },
            self.mic.text(),
            if self.speaker_enabled { "" } else { "(off)" },
            self.speaker.text()
        )
    }
    pub(super) fn roster(&mut self, call: &CallView) {
        for participant in &call.participants {
            let row = self
                .participants
                .entry(participant.participant_id.clone())
                .or_insert_with(|| ParticipantLevel {
                    level: Level::default(),
                    label: String::new(),
                    membership: CallMembership::Unknown,
                    status: None,
                    microphone: None,
                });
            row.label = clean_label(&participant.contact_id, 64);
            row.membership = participant.membership;
            row.status = participant.status;
            row.microphone = participant.microphone_enabled;
            if participant.membership == CallMembership::Left
                || participant.microphone_enabled == Some(false)
            {
                row.level = Level::default();
            }
        }
    }
    pub(super) fn participant_audio(
        &mut self,
        track: u32,
        participant: Option<&str>,
        samples: &[i16],
    ) {
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
                level: Level::default(),
                label: if participant.is_some() {
                    clean_label(&id, 64)
                } else {
                    format!("Unattributed track {track}")
                },
                membership: CallMembership::Unknown,
                status: None,
                microphone: None,
            });
        if row.microphone != Some(false) && row.membership != CallMembership::Left {
            row.level
                .observe(samples.iter().map(|s| f64::from(*s) / 32768.0));
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
        self.participants.get(id).unwrap().level.db
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
            let tail = format!(" {} {status} {mic}", row.level.text());
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
    pub(super) fn refresh(&mut self) {
        self.mic.advance();
        self.speaker.advance();
        for row in self.participants.values_mut() {
            row.level.advance();
        }
        if !self.enabled {
            return;
        }
        let Some(columns) = terminal_columns().filter(|width| *width >= 32) else {
            self.clear();
            return;
        };
        let lines = self.lines(columns, terminal_rows());
        if self.drawn && self.drawn_rows != lines.len() {
            self.clear();
        }
        let mut out = io::stderr().lock();
        let result = if self.drawn {
            let mut redraw = format!("\x1b7\x1b[{}A\r", lines.len());
            for line in &lines {
                redraw.push_str("\x1b[2K");
                redraw.push_str(line);
                redraw.push('\n');
            }
            redraw.push_str("\x1b8");
            write!(out, "{redraw}")
        } else {
            self.drawn = true;
            write!(out, "{}\n> ", lines.join("\n"))
        };
        self.drawn_rows = lines.len();
        if result.and_then(|_| out.flush()).is_err() {
            self.enabled = false;
        }
    }
    /// The terminal echoed Enter, moving below the command prompt.
    pub(super) fn input_received(&mut self) {
        if self.enabled && self.drawn {
            let rows = self.drawn_rows + 1;
            let _ = write!(io::stderr(), "\x1b[{rows}A\r\x1b[J");
            self.drawn = false;
        }
    }
    pub(super) fn notice(&mut self, message: &str) {
        self.clear();
        eprintln!("{message}");
    }
    fn clear(&mut self) {
        if self.enabled && self.drawn {
            let rows = self.drawn_rows;
            let _ = write!(io::stderr(), "\r\x1b[2K\x1b[{rows}A\r\x1b[J");
            self.drawn = false;
        }
    }
}
impl Drop for CallLevels {
    fn drop(&mut self) {
        self.clear();
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
        assert!((meter.mic.db + 6.02).abs() < 0.1);
        assert!((meter.speaker.db + 12.04).abs() < 0.1);
        assert!(meter.line().contains("-6dB"));
        assert!(!meter.drawn);
        meter.speaker(&[1.0; 320]);
        meter.refresh();
        assert_eq!(meter.speaker.clip_ticks, 10);
        meter.microphone(&[32767; 320], true);
        meter.refresh();
        assert_eq!(meter.mic.db, FLOOR_DB);
        assert!(meter.line().contains("Mic(muted)"));
        for _ in 0..10 {
            meter.refresh();
        }
        assert_eq!(meter.speaker.db, FLOOR_DB);
        assert_eq!(meter.speaker.clip_ticks, 0);
        meter.microphone(&[-32768; 320], false);
        meter.refresh();
        assert_eq!(meter.mic.db, 0.0);
        meter.clear_microphone();
        meter.clear_speaker();
        assert_eq!(meter.mic.db, FLOOR_DB);
        assert_eq!(meter.speaker.db, FLOOR_DB);
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
                level: Level::default(),
                label: "Contact".into(),
                membership: CallMembership::Invited,
                status: Some(CallParticipantStatus {
                    code: 3,
                    kind: CallParticipantStatusKind::Acknowledged,
                }),
                microphone: None,
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
            }],
        };
        levels.roster(&call);
        levels.participant_audio(7, Some("person"), &[16000; 320]);
        levels.refresh();
        assert!(levels.participant_level("person") > -10.0);
        call.participants[0].microphone_enabled = Some(false);
        levels.roster(&call);
        levels.participant_audio(7, Some("person"), &[16000; 320]);
        levels.refresh();
        assert_eq!(levels.participant_level("person"), FLOOR_DB);
        assert!(levels.lines(80, 24)[1].contains("mic off"));
        assert!(!levels.lines(80, 24)[1].contains('\x1b'));
        assert_eq!(
            levels.participants["person"].membership,
            CallMembership::Joined
        );
        call.participants[0].microphone_enabled = None;
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
}
