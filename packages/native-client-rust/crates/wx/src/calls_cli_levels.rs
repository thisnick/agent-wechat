//! Signal-only terminal meters. No audio, device volume or call state is retained.
use std::io::{self, IsTerminal, Write};

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

pub(super) struct CallLevels {
    mic: Level,
    speaker: Level,
    muted: bool,
    enabled: bool,
    drawn: bool,
    drawn_len: usize,
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
            drawn_len: 0,
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
            "Mic{} {} Speaker {}",
            if self.muted { "(muted)" } else { "" },
            self.mic.text(),
            self.speaker.text()
        )
    }
    pub(super) fn refresh(&mut self) {
        self.mic.advance();
        self.speaker.advance();
        if !self.enabled {
            return;
        }
        let line = self.line();
        let columns = terminal_columns();
        if !fits_width(&line, columns) {
            self.clear();
            return;
        }
        if self.drawn && columns.is_some_and(|width| self.drawn_len >= width) {
            self.clear();
        }
        let mut out = io::stderr().lock();
        let result = if self.drawn {
            // Keep the cursor and any typed command on its separate input line.
            write!(out, "\x1b7\x1b[1A\r\x1b[2K{line}\x1b8")
        } else {
            self.drawn = true;
            write!(out, "{line}\n> ")
        };
        self.drawn_len = line.len();
        if result.and_then(|_| out.flush()).is_err() {
            self.enabled = false;
        }
    }
    /// The terminal echoed Enter, moving below the command prompt.
    pub(super) fn input_received(&mut self) {
        if self.enabled && self.drawn {
            let rows = self.rows() + 1;
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
            let rows = self.rows();
            let _ = write!(io::stderr(), "\r\x1b[2K\x1b[{rows}A\r\x1b[J");
            self.drawn = false;
        }
    }
    fn rows(&self) -> usize {
        self.drawn_len
            .div_ceil(terminal_columns().unwrap_or(self.drawn_len + 1).max(1))
            .max(1)
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

fn fits_width(line: &str, columns: Option<usize>) -> bool {
    columns.is_some_and(|width| line.len() < width)
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
