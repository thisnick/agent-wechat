//! OS boundaries shared by the native CLI, daemon and profile storage.
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs::{File, Metadata},
    io,
    path::Path,
    process::Command,
};
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub const NOFOLLOW: i32 = libc::O_NOFOLLOW;
#[cfg(windows)]
pub const NOFOLLOW: i32 = 0x00200000; // FILE_FLAG_OPEN_REPARSE_POINT

/// Files never follow a final symlink/reparse point. Private creation is atomic.
pub struct OpenOptions {
    inner: std::fs::OpenOptions,
    private: bool,
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    create_new: bool,
    truncate: bool,
}
impl Default for OpenOptions {
    fn default() -> Self {
        Self::new()
    }
}
impl OpenOptions {
    pub fn new() -> Self {
        Self {
            inner: std::fs::OpenOptions::new(),
            private: false,
            read: false,
            write: false,
            append: false,
            create: false,
            create_new: false,
            truncate: false,
        }
    }
    pub fn read(&mut self, value: bool) -> &mut Self {
        self.read = value;
        self.inner.read(value);
        self
    }
    pub fn write(&mut self, value: bool) -> &mut Self {
        self.write = value;
        self.inner.write(value);
        self
    }
    pub fn append(&mut self, value: bool) -> &mut Self {
        self.append = value;
        self.inner.append(value);
        self
    }
    pub fn create(&mut self, value: bool) -> &mut Self {
        self.create = value;
        self.inner.create(value);
        self
    }
    pub fn create_new(&mut self, value: bool) -> &mut Self {
        self.create_new = value;
        self.inner.create_new(value);
        self
    }
    pub fn truncate(&mut self, value: bool) -> &mut Self {
        self.truncate = value;
        self.inner.truncate(value);
        self
    }
    pub fn mode(&mut self, mode: u32) -> &mut Self {
        self.private = true;
        #[cfg(unix)]
        self.inner.mode(mode);
        #[cfg(windows)]
        let _ = mode;
        self
    }
    pub fn custom_flags(&mut self, flags: i32) -> &mut Self {
        #[cfg(unix)]
        self.inner.custom_flags(flags | NOFOLLOW);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            self.inner.custom_flags(flags as u32 | NOFOLLOW as u32);
        }
        self
    }
    pub fn open(&mut self, path: impl AsRef<Path>) -> io::Result<File> {
        self.custom_flags(NOFOLLOW);
        #[cfg(unix)]
        {
            self.inner.open(path)
        }
        #[cfg(windows)]
        {
            windows::open(self, path.as_ref())
        }
    }
}

pub fn private_file(file: &File) -> io::Result<bool> {
    let m = file.metadata()?;
    #[cfg(unix)]
    {
        Ok(m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        windows::private_file(file, &m)
    }
}
pub fn private_path(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        let m = std::fs::symlink_metadata(path)?;
        Ok(!m.is_symlink() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        windows::private_path(path)
    }
}
pub fn owned_path(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        let m = std::fs::symlink_metadata(path)?;
        Ok(!m.is_symlink() && m.uid() == unsafe { libc::geteuid() })
    }
    #[cfg(windows)]
    {
        windows::owned_path(path)
    }
}
pub fn protect(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        windows::protect(file)
    }
}
pub fn protect_path(path: &Path, directory: bool) -> io::Result<()> {
    if !owned_path(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path must be owned by this user",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )
    }
    #[cfg(windows)]
    {
        let _ = directory;
        windows::protect_path(path)
    }
}
pub fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    #[cfg(windows)]
    windows::create_dir(path)?;
    if !std::fs::symlink_metadata(path)?.is_dir() || !private_path(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory must be private, owned and not a link",
        ));
    }
    Ok(())
}

/// A randomly named scratch directory, owned by this user from creation.
/// Windows' default temporary-directory owner can be Administrators when elevated.
pub struct PrivateTempDir {
    path: std::path::PathBuf,
}
impl PrivateTempDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn keep(mut self) -> std::path::PathBuf {
        std::mem::take(&mut self.path)
    }
}
impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
pub fn private_tempdir(prefix: &str) -> io::Result<PrivateTempDir> {
    private_tempdir_in(&std::env::temp_dir(), prefix)
}
pub fn private_tempdir_in(parent: &Path, prefix: &str) -> io::Result<PrivateTempDir> {
    let temporary = tempfile::Builder::new()
        .prefix(prefix)
        .rand_bytes(16)
        .make_in(parent, |path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(path)?;
            }
            #[cfg(windows)]
            windows::create_dir_new(path)?;
            let directory = PrivateTempDir { path: path.into() };
            if !private_path(path)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "scratch directory must be private and owned",
                ));
            }
            Ok(directory)
        })?;
    let (directory, mut path) = temporary.into_parts();
    // PrivateTempDir owns recursive cleanup; TempPath only removes files.
    path.disable_cleanup(true);
    Ok(directory)
}
pub fn private_tempfile_in(parent: &Path, prefix: &str) -> io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(prefix)
        .rand_bytes(16)
        .make_in(parent, |path| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        })
}
/// Windows has no directory fsync equivalent; files are flushed before publication.
pub fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
}
pub fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
}
pub fn user_key() -> String {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() }.to_string()
    }
    #[cfg(windows)]
    {
        windows::user_sid_string().expect("current Windows user SID")
    }
}
#[cfg(unix)]
pub fn same_file(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        a.dev() == b.dev() && a.ino() == b.ino()
    }
}
pub fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000200 | 0x00000008);
    }
}
/// PID reuse is excluded by comparing creation time and the canonical executable.
pub fn process_identity(pid: u32) -> Option<(String, std::path::PathBuf)> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let tail: Vec<_> = text.rsplit_once(')')?.1.split_whitespace().collect();
        if tail.first() == Some(&"Z") {
            return None;
        }
        Some((
            tail.get(19)?.to_string(),
            std::fs::read_link(format!("/proc/{pid}/exe")).ok()?,
        ))
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        if unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut _,
                size,
            )
        } != size
            || info.pbi_status == 5
        {
            return None;
        }
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let count =
            unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
        if count <= 0 {
            return None;
        }
        let end = path.iter().position(|b| *b == 0)?;
        use std::os::unix::ffi::OsStrExt;
        Some((
            format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
            Path::new(std::ffi::OsStr::from_bytes(&path[..end])).to_owned(),
        ))
    }
    #[cfg(windows)]
    {
        windows::process_identity(pid)
    }
}

/// Atomically publish a new directory, refusing any existing destination.
pub fn publish_directory(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let a = std::ffi::CString::new(path_bytes(source))?;
        let b = std::ffi::CString::new(path_bytes(destination))?;
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_EXCL) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(windows)]
    windows::publish_directory(source, destination)?;
    sync_dir(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("destination needs parent"))?,
    )
}
/// Includes Windows junctions and other reparse points, not only symlinks.
pub fn is_link(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        metadata.is_symlink()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
}
pub fn same_path_file(path: &Path, file: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        Ok(same_file(
            &std::fs::symlink_metadata(path)?,
            &file.metadata()?,
        ))
    }
    #[cfg(windows)]
    {
        windows::same_path_file(path, file)
    }
}

pub fn sync_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        OpenOptions::new().write(true).open(path)?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn scratch_ownership_cleanup_and_no_overwrite_persistence() {
        let parent = tempfile::tempdir().unwrap();
        let directory_path;
        {
            let directory = private_tempdir_in(parent.path(), "private scratch ").unwrap();
            directory_path = directory.path().to_owned();
            assert!(private_path(directory.path()).unwrap());
            let mut file = private_tempfile_in(directory.path(), ".private-").unwrap();
            assert!(private_path(file.path()).unwrap());
            file.write_all(b"synthetic data").unwrap();
            let destination = directory.path().join("saved");
            file.persist_noclobber(&destination).unwrap();
            assert!(private_path(&destination).unwrap());
            let another = private_tempfile_in(directory.path(), ".private-").unwrap();
            assert!(another.persist_noclobber(&destination).is_err());
            assert_eq!(std::fs::read(destination).unwrap(), b"synthetic data");
        }
        assert!(!directory_path.exists());
        let kept = private_tempdir_in(parent.path(), "kept-").unwrap().keep();
        assert!(private_path(&kept).unwrap());
        std::fs::remove_dir(kept).unwrap();
    }
    #[test]
    fn private_creation_and_no_overwrite_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("private root");
        private_dir(&root).unwrap();
        assert!(private_path(&root).unwrap());
        let key = root.join("key");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key)
            .unwrap();
        file.write_all(b"synthetic key").unwrap();
        assert!(private_file(&file).unwrap());
        assert!(same_path_file(&key, &file).unwrap());
        let stage = root.join("stage");
        private_dir(&stage).unwrap();
        let destination = root.join("destination");
        publish_directory(&stage, &destination).unwrap();
        private_dir(&stage).unwrap();
        assert!(publish_directory(&stage, &destination).is_err());
        assert!(stage.is_dir() && destination.is_dir());
    }
    #[test]
    fn process_identity_includes_creation_time_and_executable() {
        let (created, executable) = process_identity(std::process::id()).unwrap();
        assert!(!created.is_empty());
        assert_eq!(
            executable,
            std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        );
        assert!(process_identity(u32::MAX).is_none());
    }
    #[test]
    #[cfg(unix)]
    fn public_permissions_and_final_symlinks_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("key");
        std::fs::write(&path, b"synthetic").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!private_path(&path).unwrap());
        symlink(&path, temp.path().join("link")).unwrap();
        assert!(OpenOptions::new()
            .read(true)
            .open(temp.path().join("link"))
            .is_err());
    }
}
