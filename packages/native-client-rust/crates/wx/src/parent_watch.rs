//! Notices when the process that started `wx` has exited, for
//! `wx serve --exit-with-parent`.

/// Reports when the process that started this one has exited.
///
/// Create it at startup, while that process is still running. On Unix it
/// notices that this process now has a different parent, which happens when
/// the original parent exits. On Windows it holds a handle to the original
/// parent, opened while that parent was alive, so a reused process ID cannot
/// be mistaken for it.
pub struct ParentWatch(imp::Watch);
impl ParentWatch {
    /// Starts watching the current parent process.
    pub fn new() -> std::io::Result<Self> {
        imp::Watch::new().map(Self)
    }
    /// Whether the original parent has exited. This never blocks.
    pub fn exited(&self) -> bool {
        self.0.exited()
    }
}
// The daemon keeps the watch in its shutdown future, which moves between
// threads, so it must stay `Send` and `Sync` on every platform.
const _: fn() = || {
    fn shareable<T: Send + Sync>() {}
    shareable::<ParentWatch>();
};

#[cfg(unix)]
mod imp {
    pub struct Watch(libc::pid_t);
    impl Watch {
        pub fn new() -> std::io::Result<Self> {
            Ok(Self(unsafe { libc::getppid() }))
        }
        pub fn exited(&self) -> bool {
            unsafe { libc::getppid() != self.0 }
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess,
                WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        },
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }
    /// A handle to the parent process, or `None` when it had already exited.
    pub struct Watch(Option<Handle>);
    // SAFETY: a process handle is a kernel object reference that any thread may
    // wait on or close; the watch only waits with a zero timeout and closes it once.
    unsafe impl Send for Watch {}
    unsafe impl Sync for Watch {}
    impl Watch {
        pub fn new() -> io::Result<Self> {
            let parent = parent_process_id()?;
            let handle = Handle(unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    parent,
                )
            });
            if handle.0.is_null() {
                return Ok(Self(None));
            }
            // A process created after this one only reuses the exited parent's ID.
            if creation_time(handle.0)? > creation_time(unsafe { GetCurrentProcess() })? {
                return Ok(Self(None));
            }
            Ok(Self(Some(handle)))
        }
        pub fn exited(&self) -> bool {
            self.0
                .as_ref()
                .is_none_or(|handle| unsafe { WaitForSingleObject(handle.0, 0) } == WAIT_OBJECT_0)
        }
    }
    fn parent_process_id() -> io::Result<u32> {
        let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) });
        if snapshot.0 == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let current = unsafe { GetCurrentProcessId() };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        while found {
            if entry.th32ProcessID == current {
                return Ok(entry.th32ParentProcessID);
            }
            found = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "this process is missing from the process list",
        ))
    }
    fn creation_time(process: HANDLE) -> io::Result<u64> {
        let mut created: FILETIME = unsafe { std::mem::zeroed() };
        let mut exited = created;
        let mut kernel = created;
        let mut user = created;
        if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}
