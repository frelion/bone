//! Concrete file commits shared by the runtime and local UI.
use std::{fs::File, io, path::Path};

pub fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    return windows_move(source, destination, true);
    #[cfg(not(windows))]
    std::fs::rename(source, destination)
}

pub fn create(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    return windows_move(source, destination, false);
    #[cfg(not(windows))]
    {
        std::fs::hard_link(source, destination)?;
        std::fs::remove_file(source)
    }
}

pub fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    return File::open(directory)?.sync_all();
    #[cfg(windows)]
    {
        // Windows does not provide POSIX directory fsync. All installs above
        // request write-through after flushing file contents. Retained pending
        // markers conservatively protect operations whose outcome is unknown.
        let _ = directory;
        Ok(())
    }
}

pub fn private_file(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    windows_private_file(file, false)?;
    Ok(())
}

/// Authentication SDKs write their own cache; protect its directory first.
pub fn private_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, READ_CONTROL, WRITE_DAC,
        };
        let file = std::fs::OpenOptions::new()
            .access_mode(READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(directory)?;
        windows_private_file(&file, true)?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_move(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // Both paths name temporary and final files in one directory/volume.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn windows_private_file(file: &File, directory: bool) -> io::Result<()> {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT,
                SetSecurityInfo,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION,
        },
    };
    // OWNER RIGHTS follows the file's owner; SYSTEM retains normal OS access.
    // Protect the ACL before any secret is written, even in a shared data dir.
    let text: Vec<u16> = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    }
    .encode_utf16()
    .chain(Some(0))
    .collect();
    let mut descriptor = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = ptr::null_mut();
    let result = (|| {
        let writable = if directory {
            // The directory was opened explicitly with WRITE_DAC above.
            use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle};
            unsafe { OwnedHandle::from_raw_handle(file.try_clone()?.into_raw_handle()) }
        } else {
            security_handle(file)?
        };
        if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            == 0
        {
            Err(io::Error::last_os_error())
        } else {
            let status = unsafe {
                SetSecurityInfo(
                    writable.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    acl,
                    ptr::null(),
                )
            };
            if status == 0 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(status as i32))
            }
        }
    })();
    unsafe {
        LocalFree(descriptor);
    }
    result
}

#[cfg(windows)]
fn security_handle(file: &File) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, ReOpenFile,
            WRITE_DAC,
        },
    };
    let raw = unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            0,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

/// Preserve existing Windows access controls when replacing a workspace file.
#[cfg(windows)]
pub fn copy_access(source: &File, destination: &File) -> io::Result<()> {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo},
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
            PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
            UNPROTECTED_DACL_SECURITY_INFORMATION,
        },
    };
    let mut descriptor = ptr::null_mut();
    let mut acl = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            source.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let result = (|| {
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let access = DACL_SECURITY_INFORMATION
            | if control & SE_DACL_PROTECTED != 0 {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
        let writable = security_handle(destination)?;
        let status = unsafe {
            SetSecurityInfo(
                writable.as_raw_handle(),
                SE_FILE_OBJECT,
                access,
                ptr::null_mut(),
                ptr::null_mut(),
                acl,
                ptr::null(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    })();
    unsafe {
        LocalFree(descriptor);
    }
    result
}

/// BONE-created credential files must retain the protected owner/System ACL.
#[cfg(windows)]
pub fn check_private(file: &File) -> io::Result<()> {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            ACCESS_ALLOWED_ACE,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorControl, IsWellKnownSid,
            SE_DACL_PROTECTED, WinCreatorOwnerRightsSid, WinLocalSystemSid,
        },
    };
    let mut descriptor = ptr::null_mut();
    let mut acl = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let result = (|| {
        let reject = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "BONE credential file must have a private owner/System ACL; provide the credential again",
            )
        };
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if control & SE_DACL_PROTECTED == 0 || acl.is_null() || unsafe { (*acl).AceCount } != 2 {
            return Err(reject());
        }
        for index in 0..2 {
            let mut raw = ptr::null_mut();
            if unsafe { GetAce(acl, index, &mut raw) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
            if ace.Header.AceType != 0 {
                return Err(reject());
            }
            let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
            if unsafe { IsWellKnownSid(sid, WinCreatorOwnerRightsSid) } == 0
                && unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } == 0
            {
                return Err(reject());
            }
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor);
    }
    result
}
