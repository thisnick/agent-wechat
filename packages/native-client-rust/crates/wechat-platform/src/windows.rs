use super::*;
use std::{
    ffi::c_void,
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::Threading::*,
};
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let path = std::path::absolute(path)?;
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a NUL",
        ));
    }
    let mut result: Vec<u16> = if units.starts_with(&[92, 92, 63, 92]) {
        units
    } else if units.starts_with(&[92, 92]) {
        "\\\\?\\UNC\\"
            .encode_utf16()
            .chain(units[2..].iter().copied())
            .collect()
    } else {
        "\\\\?\\".encode_utf16().chain(units).collect()
    };
    result.push(0);
    Ok(result)
}
struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                CloseHandle(self.0);
            }
        }
    }
}
fn error() -> io::Error {
    io::Error::last_os_error()
}
fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(error());
    }
    let _memory = Local(text.cast());
    let mut n = 0;
    unsafe {
        while *text.add(n) != 0 {
            n += 1;
        }
    }
    Ok(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(text, n)
    }))
}
pub fn user_sid_string() -> io::Result<String> {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(error());
    }
    let token = Handle(token);
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut bytes);
    }
    let mut storage = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(error());
    }
    sid_string(unsafe { (*(storage.as_ptr() as *const TOKEN_USER)).User.Sid })
}
fn descriptor() -> io::Result<Local> {
    let sid = user_sid_string()?;
    let text: Vec<u16> = format!("O:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(error());
    }
    Ok(Local(descriptor))
}
fn attributes(descriptor: &Local) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    }
}
fn no_reparse(file: &File) -> io::Result<()> {
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reparse points are unsupported",
        ));
    }
    Ok(())
}
pub fn open(options: &super::OpenOptions, path: &Path) -> io::Result<File> {
    let descriptor = descriptor()?;
    let attributes = attributes(&descriptor);
    if !(options.read || options.write || options.append) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "read or write access is required",
        ));
    }
    let access = READ_CONTROL
        | if options.read { GENERIC_READ } else { 0 }
        | if options.write {
            GENERIC_WRITE
        } else if options.append {
            FILE_APPEND_DATA
        } else {
            0
        };
    let disposition = if options.create_new {
        CREATE_NEW
    } else if options.create && options.truncate {
        CREATE_ALWAYS
    } else if options.create {
        OPEN_ALWAYS
    } else if options.truncate {
        TRUNCATE_EXISTING
    } else {
        OPEN_EXISTING
    };
    let handle = unsafe {
        CreateFileW(
            wide(path)?.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    no_reparse(&file)?;
    if options.private && !private_file(&file, &file.metadata()?)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file must have an owner-only ACL",
        ));
    }
    Ok(file)
}
fn path_handle(path: &Path, write_acl: bool) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options
        .access_mode(READ_CONTROL | if write_acl { WRITE_DAC } else { 0 })
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    no_reparse(&file)?;
    Ok(file)
}
fn security(file: &File, check_acl: bool) -> io::Result<bool> {
    no_reparse(file)?;
    let mut owner = std::ptr::null_mut();
    let mut acl = std::ptr::null_mut();
    let mut sd = std::ptr::null_mut();
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let _memory = Local(sd);
    let current = user_sid_string()?;
    if owner.is_null() || sid_string(owner)? != current {
        return Ok(false);
    }
    if !check_acl {
        return Ok(true);
    }
    if acl.is_null() {
        return Ok(false);
    } // a null DACL grants everyone full access
    for i in 0..unsafe { (*acl).AceCount } {
        let mut ace = std::ptr::null_mut();
        if unsafe { GetAce(acl, i as u32, &mut ace) } == 0 {
            return Err(error());
        }
        let header = unsafe { &*(ace as *const ACE_HEADER) };
        if header.AceType == 1 {
            continue;
        }
        if header.AceType != 0 {
            return Ok(false);
        }
        let allowed = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
        let sid = sid_string(&allowed.SidStart as *const _ as PSID)?;
        if allowed.Mask != 0 && sid != current && sid != "S-1-5-18" {
            return Ok(false);
        }
    }
    Ok(true)
}
pub fn private_file(file: &File, _metadata: &Metadata) -> io::Result<bool> {
    security(file, true)
}
pub fn private_path(path: &Path) -> io::Result<bool> {
    security(&path_handle(path, false)?, true)
}
pub fn owned_path(path: &Path) -> io::Result<bool> {
    security(&path_handle(path, false)?, false)
}
fn protect_with_handle(file: &File, writable: HANDLE) -> io::Result<()> {
    if !security(file, false)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file is not owned by this user",
        ));
    }
    let descriptor = descriptor()?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = std::ptr::null_mut();
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted) }
        == 0
        || present == 0
    {
        return Err(error());
    }
    let result = unsafe {
        SetSecurityInfo(
            writable,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    Ok(())
}
pub fn protect(file: &File) -> io::Result<()> {
    let writable = Handle(unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    });
    if writable.0 == INVALID_HANDLE_VALUE {
        return Err(error());
    }
    protect_with_handle(file, writable.0)
}
pub fn protect_path(path: &Path) -> io::Result<()> {
    // This handle already grants WRITE_DAC. Reopening a directory is unnecessary
    // and can fail even when the owner can update its security descriptor.
    let file = path_handle(path, true)?;
    protect_with_handle(&file, file.as_raw_handle())
}
pub fn create_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        no_reparse(&path_handle(path, false)?)?;
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_dir(parent)?;
    }
    match create_dir_new(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}
pub fn create_dir_new(path: &Path) -> io::Result<()> {
    let descriptor = descriptor()?;
    let attributes = attributes(&descriptor);
    if unsafe { CreateDirectoryW(wide(path)?.as_ptr(), &attributes) } == 0 {
        return Err(error());
    }
    Ok(())
}
pub fn process_identity(pid: u32) -> Option<(String, std::path::PathBuf)> {
    let handle = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    if handle.0.is_null() {
        return None;
    }
    let mut exit = 0;
    if unsafe { GetExitCodeProcess(handle.0, &mut exit) } == 0 || exit != 259 {
        return None;
    }
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return None;
    }
    let mut path = vec![0u16; 32768];
    let mut len = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, path.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    let executable = std::fs::canonicalize(String::from_utf16(&path[..len as usize]).ok()?).ok()?;
    Some((
        format!("{}:{}", created.dwHighDateTime, created.dwLowDateTime),
        executable,
    ))
}
pub fn publish_directory(source: &Path, destination: &Path) -> io::Result<()> {
    if unsafe {
        MoveFileExW(
            wide(source)?.as_ptr(),
            wide(destination)?.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(error());
    }
    Ok(())
}
pub fn same_path_file(path: &Path, file: &File) -> io::Result<bool> {
    fn identity(file: &File) -> io::Result<(u32, u32, u32)> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(error());
        }
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    }
    Ok(identity(&path_handle(path, false)?)? == identity(file)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_paths_are_lossless_extended_and_reject_embedded_nuls() {
        use std::os::windows::ffi::OsStringExt;
        let path =
            std::path::PathBuf::from(std::ffi::OsString::from_wide(&[67, 58, 92, 97, 0, 98]));
        assert_eq!(wide(&path).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            String::from_utf16_lossy(&wide(Path::new("C:\\a b")).unwrap()),
            "\\\\?\\C:\\a b\0"
        );
    }
    #[test]
    fn junctions_are_rejected_before_private_directory_admission() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        crate::private_dir(&target).unwrap();
        let link = root.path().join("junction");
        let result = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(result.status.success(), "junction fixture creation failed");
        assert!(crate::is_link(&std::fs::symlink_metadata(&link).unwrap()));
        assert!(crate::private_dir(&link).is_err());
        assert!(crate::OpenOptions::new().read(true).open(&link).is_err());
        std::fs::remove_dir(&link).unwrap();
    }
    #[test]
    fn an_everyone_acl_is_rejected_and_owner_can_restore_private_access() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private-key");
        let file = crate::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        assert!(crate::private_file(&file).unwrap());
        let text: Vec<u16> = "D:P(A;;FA;;;WD)".encode_utf16().chain(Some(0)).collect();
        let mut sd = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut sd,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let _memory = Local(sd);
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = std::ptr::null_mut();
        assert_ne!(
            unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted) },
            0
        );
        let writable = path_handle(&path, true).unwrap();
        assert_eq!(
            unsafe {
                SetSecurityInfo(
                    writable.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    acl,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        assert!(!crate::private_file(&file).unwrap());
        crate::protect_path(&path, false).unwrap();
        assert!(crate::private_file(&file).unwrap());
    }
    #[test]
    fn an_owned_directory_acl_can_be_protected_repeatedly() {
        let root = crate::private_tempdir("directory-acl-").unwrap();
        assert!(crate::owned_path(root.path()).unwrap());
        for _ in 0..2 {
            crate::protect_path(root.path(), true).unwrap();
            assert!(crate::private_path(root.path()).unwrap());
        }
    }
}
