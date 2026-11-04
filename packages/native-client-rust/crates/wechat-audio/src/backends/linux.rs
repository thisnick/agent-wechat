// PulseAudio backend derived from sys-voice; see THIRD-PARTY.md.
// The nonblocking mainloop replaces pa_simple's uninterruptible reads.
//
// libpulse is loaded when local audio opens, not linked into the executable:
// wx then starts on machines without PulseAudio (servers, slim containers),
// and only commands that use this computer's microphone or speakers report
// that it is missing.
use super::{wait_ready, DeviceBackend, NATIVE_RATE};
use crate::{AecStatus, AudioConfig, AudioDevice, AudioError, PlaybackHandle};
use libloading::Library;
use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::{mpsc::SyncSender, OnceLock};

/// The part of the libpulse C API this backend uses (pulse/*.h). Opaque handles
/// stay opaque; the info structs declare only their leading fields, which are
/// read through pointers that libpulse owns.
#[allow(non_camel_case_types, dead_code)]
mod sys {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct pa_mainloop {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct pa_mainloop_api {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct pa_context {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct pa_stream {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct pa_operation {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct pa_sample_spec {
        pub format: c_int,
        pub rate: u32,
        pub channels: u8,
    }
    #[repr(C)]
    pub struct pa_buffer_attr {
        pub maxlength: u32,
        pub tlength: u32,
        pub prebuf: u32,
        pub minreq: u32,
        pub fragsize: u32,
    }
    /// The leading fields of `pa_server_info`.
    #[repr(C)]
    pub struct pa_server_info {
        pub user_name: *const c_char,
        pub host_name: *const c_char,
        pub server_version: *const c_char,
        pub server_name: *const c_char,
        pub sample_spec: pa_sample_spec,
        pub default_sink_name: *const c_char,
        pub default_source_name: *const c_char,
    }
    /// The leading fields shared by `pa_source_info` and `pa_sink_info`.
    #[repr(C)]
    pub struct pa_device_info {
        pub name: *const c_char,
        pub index: u32,
        pub description: *const c_char,
    }

    pub const PA_CONTEXT_NOAUTOSPAWN: c_int = 0x0001;
    pub const PA_CONTEXT_READY: c_int = 4;
    pub const PA_CONTEXT_FAILED: c_int = 5;
    pub const PA_CONTEXT_TERMINATED: c_int = 6;
    pub const PA_STREAM_READY: c_int = 2;
    pub const PA_STREAM_FAILED: c_int = 3;
    pub const PA_STREAM_TERMINATED: c_int = 4;
    pub const PA_STREAM_ADJUST_LATENCY: c_int = 0x2000;
    pub const PA_SAMPLE_FLOAT32LE: c_int = 5;
    pub const PA_SEEK_RELATIVE: c_int = 0;

    pub type ServerInfoCallback =
        unsafe extern "C" fn(*mut pa_context, *const pa_server_info, *mut c_void);
    pub type DeviceInfoCallback =
        unsafe extern "C" fn(*mut pa_context, *const pa_device_info, c_int, *mut c_void);
}

/// libpulse's entry points, resolved once and kept for the process lifetime.
struct Pulse {
    _library: Library,
    mainloop_new: unsafe extern "C" fn() -> *mut sys::pa_mainloop,
    mainloop_get_api: unsafe extern "C" fn(*mut sys::pa_mainloop) -> *mut sys::pa_mainloop_api,
    mainloop_iterate: unsafe extern "C" fn(*mut sys::pa_mainloop, c_int, *mut c_int) -> c_int,
    mainloop_free: unsafe extern "C" fn(*mut sys::pa_mainloop),
    context_new:
        unsafe extern "C" fn(*mut sys::pa_mainloop_api, *const c_char) -> *mut sys::pa_context,
    context_connect:
        unsafe extern "C" fn(*mut sys::pa_context, *const c_char, c_int, *const c_void) -> c_int,
    context_get_state: unsafe extern "C" fn(*const sys::pa_context) -> c_int,
    context_errno: unsafe extern "C" fn(*const sys::pa_context) -> c_int,
    context_disconnect: unsafe extern "C" fn(*mut sys::pa_context),
    context_unref: unsafe extern "C" fn(*mut sys::pa_context),
    context_get_server_info: unsafe extern "C" fn(
        *mut sys::pa_context,
        sys::ServerInfoCallback,
        *mut c_void,
    ) -> *mut sys::pa_operation,
    context_get_source_info_list: unsafe extern "C" fn(
        *mut sys::pa_context,
        sys::DeviceInfoCallback,
        *mut c_void,
    ) -> *mut sys::pa_operation,
    context_get_sink_info_list: unsafe extern "C" fn(
        *mut sys::pa_context,
        sys::DeviceInfoCallback,
        *mut c_void,
    ) -> *mut sys::pa_operation,
    operation_unref: unsafe extern "C" fn(*mut sys::pa_operation),
    stream_new: unsafe extern "C" fn(
        *mut sys::pa_context,
        *const c_char,
        *const sys::pa_sample_spec,
        *const c_void,
    ) -> *mut sys::pa_stream,
    stream_connect_record: unsafe extern "C" fn(
        *mut sys::pa_stream,
        *const c_char,
        *const sys::pa_buffer_attr,
        c_int,
    ) -> c_int,
    stream_connect_playback: unsafe extern "C" fn(
        *mut sys::pa_stream,
        *const c_char,
        *const sys::pa_buffer_attr,
        c_int,
        *const c_void,
        *mut sys::pa_stream,
    ) -> c_int,
    stream_get_state: unsafe extern "C" fn(*const sys::pa_stream) -> c_int,
    stream_peek: unsafe extern "C" fn(*mut sys::pa_stream, *mut *const c_void, *mut usize) -> c_int,
    stream_drop: unsafe extern "C" fn(*mut sys::pa_stream) -> c_int,
    stream_writable_size: unsafe extern "C" fn(*const sys::pa_stream) -> usize,
    stream_write: unsafe extern "C" fn(
        *mut sys::pa_stream,
        *const c_void,
        usize,
        Option<unsafe extern "C" fn(*mut c_void)>,
        i64,
        c_int,
    ) -> c_int,
    stream_flush: unsafe extern "C" fn(
        *mut sys::pa_stream,
        Option<unsafe extern "C" fn(*mut sys::pa_stream, c_int, *mut c_void)>,
        *mut c_void,
    ) -> *mut sys::pa_operation,
    stream_disconnect: unsafe extern "C" fn(*mut sys::pa_stream) -> c_int,
    stream_unref: unsafe extern "C" fn(*mut sys::pa_stream),
    strerror: unsafe extern "C" fn(c_int) -> *const c_char,
}

const MISSING_LIBRARY: &str = "PulseAudio is not available (libpulse.so.0 could not be loaded); \
install libpulse0, or PipeWire's PulseAudio support, to use this computer's microphone and speakers";

fn pulse() -> Result<&'static Pulse, AudioError> {
    static PULSE: OnceLock<Result<Pulse, String>> = OnceLock::new();
    PULSE
        .get_or_init(Pulse::load)
        .as_ref()
        .map_err(|message| AudioError::Backend(message.clone()))
}

fn symbol<T: Copy>(library: &Library, name: &str) -> Result<T, String> {
    let mut nul_terminated = name.as_bytes().to_vec();
    nul_terminated.push(0);
    // SAFETY: each field's type above matches the C prototype of `name`.
    unsafe { library.get::<T>(&nul_terminated) }
        .map(|symbol| *symbol)
        .map_err(|_| format!("libpulse.so.0 does not provide {name}"))
}

impl Pulse {
    fn load() -> Result<Self, String> {
        // SAFETY: loading libpulse runs only its own library constructors.
        let library =
            unsafe { Library::new("libpulse.so.0") }.map_err(|_| MISSING_LIBRARY.to_string())?;
        Ok(Self {
            mainloop_new: symbol(&library, "pa_mainloop_new")?,
            mainloop_get_api: symbol(&library, "pa_mainloop_get_api")?,
            mainloop_iterate: symbol(&library, "pa_mainloop_iterate")?,
            mainloop_free: symbol(&library, "pa_mainloop_free")?,
            context_new: symbol(&library, "pa_context_new")?,
            context_connect: symbol(&library, "pa_context_connect")?,
            context_get_state: symbol(&library, "pa_context_get_state")?,
            context_errno: symbol(&library, "pa_context_errno")?,
            context_disconnect: symbol(&library, "pa_context_disconnect")?,
            context_unref: symbol(&library, "pa_context_unref")?,
            context_get_server_info: symbol(&library, "pa_context_get_server_info")?,
            context_get_source_info_list: symbol(&library, "pa_context_get_source_info_list")?,
            context_get_sink_info_list: symbol(&library, "pa_context_get_sink_info_list")?,
            operation_unref: symbol(&library, "pa_operation_unref")?,
            stream_new: symbol(&library, "pa_stream_new")?,
            stream_connect_record: symbol(&library, "pa_stream_connect_record")?,
            stream_connect_playback: symbol(&library, "pa_stream_connect_playback")?,
            stream_get_state: symbol(&library, "pa_stream_get_state")?,
            stream_peek: symbol(&library, "pa_stream_peek")?,
            stream_drop: symbol(&library, "pa_stream_drop")?,
            stream_writable_size: symbol(&library, "pa_stream_writable_size")?,
            stream_write: symbol(&library, "pa_stream_write")?,
            stream_flush: symbol(&library, "pa_stream_flush")?,
            stream_disconnect: symbol(&library, "pa_stream_disconnect")?,
            stream_unref: symbol(&library, "pa_stream_unref")?,
            strerror: symbol(&library, "pa_strerror")?,
            _library: library,
        })
    }

    fn describe(&self, code: c_int) -> String {
        // SAFETY: pa_strerror returns a static string or null.
        unsafe { text((self.strerror)(code)) }.unwrap_or_else(|| format!("PulseAudio error {code}"))
    }
}

/// An owned copy of a C string from libpulse.
///
/// # Safety
/// `value` is null or points to a NUL-terminated string valid for this call.
unsafe fn text(value: *const c_char) -> Option<String> {
    (!value.is_null()).then(|| {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    })
}

fn error(e: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(e.to_string())
}

struct Mainloop {
    pa: &'static Pulse,
    raw: *mut sys::pa_mainloop,
}

impl Mainloop {
    fn new(pa: &'static Pulse) -> Result<Self, AudioError> {
        // SAFETY: no preconditions.
        let raw = unsafe { (pa.mainloop_new)() };
        if raw.is_null() {
            return Err(error("cannot create PulseAudio mainloop"));
        }
        Ok(Self { pa, raw })
    }

    fn iterate(&mut self) -> Result<(), AudioError> {
        // SAFETY: `raw` is a live mainloop; a null retval pointer is allowed.
        if unsafe { (self.pa.mainloop_iterate)(self.raw, 0, ptr::null_mut()) } < 0 {
            return Err(error("PulseAudio mainloop stopped"));
        }
        Ok(())
    }
}

impl Drop for Mainloop {
    fn drop(&mut self) {
        // SAFETY: every context and stream on this mainloop was dropped first.
        unsafe { (self.pa.mainloop_free)(self.raw) }
    }
}

struct Context {
    pa: &'static Pulse,
    raw: *mut sys::pa_context,
}

impl Context {
    fn connect(
        pa: &'static Pulse,
        mainloop: &mut Mainloop,
        name: &CStr,
        failure: &str,
    ) -> Result<Self, AudioError> {
        // SAFETY: `mainloop` is live and outlives the context (its owner drops it last).
        let raw = unsafe { (pa.context_new)((pa.mainloop_get_api)(mainloop.raw), name.as_ptr()) };
        if raw.is_null() {
            return Err(error("cannot create PulseAudio context"));
        }
        let context = Self { pa, raw };
        // SAFETY: a null server selects the default; null spawn API is allowed.
        if unsafe {
            (pa.context_connect)(raw, ptr::null(), sys::PA_CONTEXT_NOAUTOSPAWN, ptr::null())
        } < 0
        {
            return Err(context.error("cannot connect to PulseAudio"));
        }
        wait_ready(|| {
            mainloop.iterate()?;
            match context.state() {
                sys::PA_CONTEXT_READY => Ok(true),
                sys::PA_CONTEXT_FAILED | sys::PA_CONTEXT_TERMINATED => Err(error(failure)),
                _ => Ok(false),
            }
        })?;
        Ok(context)
    }

    fn state(&self) -> c_int {
        // SAFETY: `raw` is a live context.
        unsafe { (self.pa.context_get_state)(self.raw) }
    }

    fn error(&self, action: &str) -> AudioError {
        // SAFETY: `raw` is a live context.
        let code = unsafe { (self.pa.context_errno)(self.raw) };
        error(format!("{action}: {}", self.pa.describe(code)))
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: disconnecting is valid in any state; this drops our reference.
        unsafe {
            (self.pa.context_disconnect)(self.raw);
            (self.pa.context_unref)(self.raw);
        }
    }
}

struct Operation {
    pa: &'static Pulse,
    raw: *mut sys::pa_operation,
}

impl Operation {
    fn new(
        pa: &'static Pulse,
        raw: *mut sys::pa_operation,
        context: &Context,
        action: &str,
    ) -> Result<Self, AudioError> {
        if raw.is_null() {
            return Err(context.error(action));
        }
        Ok(Self { pa, raw })
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        // SAFETY: this drops the reference the request returned.
        unsafe { (self.pa.operation_unref)(self.raw) }
    }
}

enum Peek<'a> {
    Empty,
    Hole,
    Data(&'a [u8]),
}

struct Stream {
    pa: &'static Pulse,
    raw: *mut sys::pa_stream,
}

impl Stream {
    fn new(pa: &'static Pulse, context: &Context, name: &CStr) -> Result<Self, AudioError> {
        let spec = sys::pa_sample_spec {
            format: sys::PA_SAMPLE_FLOAT32LE,
            rate: NATIVE_RATE,
            channels: 1,
        };
        // SAFETY: the context is live; a null channel map selects the default.
        let raw = unsafe { (pa.stream_new)(context.raw, name.as_ptr(), &spec, ptr::null()) };
        if raw.is_null() {
            return Err(context.error("cannot create PulseAudio stream"));
        }
        Ok(Self { pa, raw })
    }

    fn state(&self) -> c_int {
        // SAFETY: `raw` is a live stream.
        unsafe { (self.pa.stream_get_state)(self.raw) }
    }

    fn peek(&mut self) -> Result<Peek<'_>, AudioError> {
        let mut data = ptr::null();
        let mut length = 0usize;
        // SAFETY: `raw` is a live record stream; both out-pointers are valid.
        let code = unsafe { (self.pa.stream_peek)(self.raw, &mut data, &mut length) };
        if code < 0 {
            return Err(error(format!(
                "cannot read PulseAudio capture: {}",
                self.pa.describe(-code)
            )));
        }
        Ok(match (data.is_null(), length) {
            (true, 0) => Peek::Empty,
            (true, _) => Peek::Hole,
            // SAFETY: libpulse keeps the fragment valid until `discard`, which
            // needs `&mut self` and so ends this borrow first.
            (false, _) => {
                Peek::Data(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) })
            }
        })
    }

    fn discard(&mut self) -> Result<(), AudioError> {
        // SAFETY: called only after a peek returned data or a hole.
        let code = unsafe { (self.pa.stream_drop)(self.raw) };
        if code < 0 {
            return Err(error(format!(
                "cannot release PulseAudio capture: {}",
                self.pa.describe(-code)
            )));
        }
        Ok(())
    }

    fn writable_size(&self) -> usize {
        // SAFETY: `raw` is a live playback stream; (size_t)-1 means an error.
        match unsafe { (self.pa.stream_writable_size)(self.raw) } {
            usize::MAX => 0,
            size => size,
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), AudioError> {
        // SAFETY: without a free callback libpulse copies `bytes` before returning.
        let code = unsafe {
            (self.pa.stream_write)(
                self.raw,
                bytes.as_ptr().cast(),
                bytes.len(),
                None,
                0,
                sys::PA_SEEK_RELATIVE,
            )
        };
        if code < 0 {
            return Err(error(format!(
                "cannot write PulseAudio playback: {}",
                self.pa.describe(-code)
            )));
        }
        Ok(())
    }

    fn flush(&mut self) {
        // SAFETY: `raw` is a live playback stream; the operation needs no callback.
        let operation = unsafe { (self.pa.stream_flush)(self.raw, None, ptr::null_mut()) };
        if !operation.is_null() {
            // SAFETY: this drops the reference the request returned.
            unsafe { (self.pa.operation_unref)(operation) }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: disconnecting an unconnected stream only returns an error; this
        // drops our reference.
        unsafe {
            (self.pa.stream_disconnect)(self.raw);
            (self.pa.stream_unref)(self.raw);
        }
    }
}

#[derive(Default)]
struct ServerDefaults {
    done: bool,
    input: Option<String>,
    output: Option<String>,
}

struct DeviceList {
    input: bool,
    default: Option<String>,
    limit: usize,
    devices: Vec<AudioDevice>,
    done: bool,
    failed: bool,
}

unsafe extern "C" fn on_server_info(
    _: *mut sys::pa_context,
    info: *const sys::pa_server_info,
    userdata: *mut c_void,
) {
    // SAFETY: `userdata` is the RefCell passed with the request, which outlives
    // the context; libpulse calls back only while the mainloop iterates.
    let Ok(mut defaults) =
        (unsafe { &*userdata.cast::<RefCell<ServerDefaults>>() }).try_borrow_mut()
    else {
        return;
    };
    // SAFETY: a non-null `info` is valid for this call.
    if let Some(info) = unsafe { info.as_ref() } {
        defaults.input = unsafe { text(info.default_source_name) };
        defaults.output = unsafe { text(info.default_sink_name) };
    }
    defaults.done = true;
}

unsafe extern "C" fn on_device(
    _: *mut sys::pa_context,
    info: *const sys::pa_device_info,
    end: c_int,
    userdata: *mut c_void,
) {
    // SAFETY: as in `on_server_info`.
    let Ok(mut list) = (unsafe { &*userdata.cast::<RefCell<DeviceList>>() }).try_borrow_mut()
    else {
        return;
    };
    if end != 0 {
        list.failed |= end < 0;
        list.done = true;
        return;
    }
    // SAFETY: a non-null `info` is valid for this call.
    let Some(info) = (unsafe { info.as_ref() }) else {
        return;
    };
    if list.devices.len() >= list.limit {
        return;
    }
    if let Some(id) = unsafe { text(info.name) } {
        let device = AudioDevice {
            input: list.input,
            name: unsafe { text(info.description) }.unwrap_or_else(|| id.clone()),
            is_default: list.default.as_deref() == Some(id.as_str()),
            id: Some(id),
        };
        list.devices.push(device);
    }
}

pub(super) fn devices() -> Result<Vec<AudioDevice>, AudioError> {
    let pa = pulse()?;
    // Callback state is declared first so it outlives the context and mainloop.
    let defaults = RefCell::new(ServerDefaults::default());
    let list = RefCell::new(DeviceList {
        input: true,
        default: None,
        limit: 1024,
        devices: Vec::new(),
        done: false,
        failed: false,
    });
    let mut mainloop = Mainloop::new(pa)?;
    let context = Context::connect(
        pa,
        &mut mainloop,
        c"wx audio devices",
        "PulseAudio connection failed; start PulseAudio or PipeWire's PulseAudio service",
    )?;
    // SAFETY: the callback matches pa_server_info_cb_t; `defaults` outlives the context.
    let request = unsafe {
        (pa.context_get_server_info)(
            context.raw,
            on_server_info,
            ptr::from_ref(&defaults).cast_mut().cast(),
        )
    };
    let operation = Operation::new(pa, request, &context, "cannot read PulseAudio defaults")?;
    wait_ready(|| {
        mainloop.iterate()?;
        Ok(defaults.borrow().done)
    })?;
    drop(operation);
    {
        let mut defaults = defaults.borrow_mut();
        let mut list = list.borrow_mut();
        list.default = defaults.input.take();
    }
    // SAFETY: the callback matches pa_source_info_cb_t; `list` outlives the context.
    let request = unsafe {
        (pa.context_get_source_info_list)(
            context.raw,
            on_device,
            ptr::from_ref(&list).cast_mut().cast(),
        )
    };
    let operation = Operation::new(pa, request, &context, "cannot list PulseAudio microphones")?;
    wait_ready(|| {
        mainloop.iterate()?;
        Ok(list.borrow().done)
    })?;
    drop(operation);
    if list.borrow().failed {
        return Err(error("cannot list PulseAudio microphones"));
    }
    {
        let mut list = list.borrow_mut();
        list.input = false;
        list.default = defaults.borrow_mut().output.take();
        list.limit = 2048;
        list.done = false;
    }
    // SAFETY: the callback matches pa_sink_info_cb_t; `list` outlives the context.
    let request = unsafe {
        (pa.context_get_sink_info_list)(
            context.raw,
            on_device,
            ptr::from_ref(&list).cast_mut().cast(),
        )
    };
    let operation = Operation::new(pa, request, &context, "cannot list PulseAudio speakers")?;
    wait_ready(|| {
        mainloop.iterate()?;
        Ok(list.borrow().done)
    })?;
    drop(operation);
    drop(context);
    if list.borrow().failed {
        return Err(error("cannot list PulseAudio speakers"));
    }
    // The context is gone, so no callback can reach the list any more.
    Ok(list.into_inner().devices)
}

/// Field order is drop order: streams, then their context, then the mainloop.
pub(super) struct Backend {
    record: Stream,
    output: Stream,
    /// Kept alive for the streams; nothing reads it.
    _context: Context,
    mainloop: Mainloop,
    playback: PlaybackHandle,
    capture: SyncSender<Vec<f32>>,
    generation: u64,
}

impl Backend {
    pub(super) fn new(
        config: &AudioConfig,
        playback: PlaybackHandle,
        capture: SyncSender<Vec<f32>>,
    ) -> Result<Self, AudioError> {
        let pa = pulse()?;
        let mut mainloop = Mainloop::new(pa)?;
        let context = Context::connect(
            pa,
            &mut mainloop,
            c"wechat-audio",
            "PulseAudio connection failed",
        )?;
        let record = Stream::new(pa, &context, c"WeChat microphone")?;
        let output = Stream::new(pa, &context, c"WeChat speaker")?;
        let attr = sys::pa_buffer_attr {
            maxlength: 48_000 * 4 / 10,
            tlength: 48_000 * 4 / 25,
            prebuf: 0,
            minreq: 48_000 * 4 / 100,
            fragsize: 48_000 * 4 / 100,
        };
        let input = device_name(config.input_device.as_deref())?;
        let output_device = device_name(config.output_device.as_deref())?;
        // SAFETY: the streams are live and unconnected; names and attributes
        // are copied by libpulse.
        if unsafe {
            (pa.stream_connect_record)(
                record.raw,
                input.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
                &attr,
                sys::PA_STREAM_ADJUST_LATENCY,
            )
        } < 0
        {
            return Err(context.error("cannot open the microphone"));
        }
        // SAFETY: as above; null volume and sync stream select the defaults.
        if unsafe {
            (pa.stream_connect_playback)(
                output.raw,
                output_device
                    .as_ref()
                    .map_or(ptr::null(), |name| name.as_ptr()),
                &attr,
                sys::PA_STREAM_ADJUST_LATENCY,
                ptr::null(),
                ptr::null_mut(),
            )
        } < 0
        {
            return Err(context.error("cannot open the speaker"));
        }
        wait_ready(|| {
            mainloop.iterate()?;
            for stream in [&record, &output] {
                if matches!(
                    stream.state(),
                    sys::PA_STREAM_FAILED | sys::PA_STREAM_TERMINATED
                ) {
                    return Err(error("PulseAudio stream failed"));
                }
            }
            Ok(record.state() == sys::PA_STREAM_READY && output.state() == sys::PA_STREAM_READY)
        })?;
        let generation = playback.generation()?;
        Ok(Self {
            record,
            output,
            _context: context,
            mainloop,
            playback,
            capture,
            generation,
        })
    }
}

fn device_name(name: Option<&str>) -> Result<Option<CString>, AudioError> {
    name.map(CString::new)
        .transpose()
        .map_err(|_| error("audio device names cannot contain NUL"))
}

impl DeviceBackend for Backend {
    fn aec_status(&self) -> AecStatus {
        AecStatus::Unknown
    }
    fn poll(&mut self) -> Result<(), AudioError> {
        self.mainloop.iterate()?;
        if self.record.state() != sys::PA_STREAM_READY
            || self.output.state() != sys::PA_STREAM_READY
        {
            return Err(error("PulseAudio device disconnected"));
        }
        // Clear is checked before reading any queued output. No old producer can
        // enqueue after the generation change, and the server buffer is flushed.
        let generation = self.playback.generation()?;
        if generation != self.generation {
            self.output.flush();
            self.generation = generation;
        }
        for _ in 0..8 {
            let samples = match self.record.peek()? {
                Peek::Empty => break,
                Peek::Hole => None,
                Peek::Data(bytes) => {
                    if bytes.len() > 48_000 * 4 || bytes.len() % 4 != 0 {
                        return Err(error("invalid PulseAudio capture extent"));
                    }
                    Some(
                        bytes
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect::<Vec<f32>>(),
                    )
                }
            };
            self.record.discard()?;
            if let Some(samples) = samples {
                let _ = self.capture.try_send(samples);
            }
        }
        // Fill the whole server request. Writing only one 10ms chunk per poll
        // adds the polling delay to every request and eventually underruns even
        // when the producer and device have the same nominal sample rate.
        let writable_frames = self.output.writable_size() / 4;
        let frames = writable_frames.min(4_800);
        if frames > 0 {
            let mut samples = [0.0f32; 4_800];
            self.playback.drain(&mut samples[..frames])?;
            let mut bytes = [0u8; 4_800 * 4];
            for (sample, out) in samples[..frames]
                .iter()
                .zip(bytes[..frames * 4].chunks_exact_mut(4))
            {
                out.copy_from_slice(&sample.to_le_bytes());
            }
            self.output.write(&bytes[..frames * 4])?;
        }
        Ok(())
    }
}
