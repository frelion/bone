//! Windows shell ownership: suspend before attaching a process to a kill-on-close job.
use std::{
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation, OpenJobObjectW,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

fn owned(handle: RawHandle) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        // Win32 returned a new owning handle; RAII closes it on every path.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

/// Workspace/subscription ownership must use the actual OS user's home even
/// when callers supply different HOME, USERPROFILE or data-directory values.
pub(crate) fn user_home() -> io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        Security::TOKEN_QUERY,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    #[link(name = "userenv")]
    unsafe extern "system" {
        fn GetUserProfileDirectoryW(token: RawHandle, path: *mut u16, length: *mut u32) -> i32;
    }
    let mut raw = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(raw)?;
    let mut path = vec![0_u16; 32_768];
    let mut length = path.len() as u32;
    if unsafe { GetUserProfileDirectoryW(token.as_raw_handle(), path.as_mut_ptr(), &mut length) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    if length == 0 || length as usize > path.len() {
        return Err(io::Error::other("OS user profile directory is invalid"));
    }
    Ok(std::ffi::OsString::from_wide(&path[..length as usize - 1]).into())
}

fn job_name(lease: &File) -> io::Result<Vec<u16>> {
    let mut path = vec![0_u16; 32_768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            lease.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            0,
        )
    } as usize;
    if length == 0 || length >= path.len() {
        return Err(io::Error::last_os_error());
    }
    let bytes: Vec<u8> = path[..length]
        .iter()
        .flat_map(|c| c.to_le_bytes())
        .collect();
    Ok(
        format!("Global\\BONE-writer-{}", crate::tools::sha256(&bytes))
            .encode_utf16()
            .chain(Some(0))
            .collect(),
    )
}

fn active_processes(handle: RawHandle) -> io::Result<u32> {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    if unsafe {
        QueryInformationJobObject(
            handle,
            JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(info.ActiveProcesses)
}

/// Locks may disappear before Windows finishes terminating a crashed owner's
/// descendants. Check the named kernel job before allowing another writer.
pub(crate) fn ensure_writer_stopped(lease: &File) -> io::Result<()> {
    let name = job_name(lease)?;
    let raw = unsafe {
        OpenJobObjectW(0x0004 /* JOB_OBJECT_QUERY */, 0, name.as_ptr())
    };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let job = owned(raw)?;
    if active_processes(job.as_raw_handle())? == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "previous Windows workspace writer is still stopping",
        ))
    }
}

pub(crate) struct ProcessGroup(OwnedHandle);
impl ProcessGroup {
    pub(crate) fn terminate(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(crate) fn new(lease: Option<&File>) -> io::Result<Self> {
        let name = match lease {
            Some(file) => job_name(file)?,
            None => format!("Global\\BONE-shell-{}", uuid::Uuid::new_v4())
                .encode_utf16()
                .chain(Some(0))
                .collect(),
        };
        let job = owned(unsafe { CreateJobObjectW(ptr::null(), name.as_ptr()) })?;
        if active_processes(job.as_raw_handle())? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "previous Windows shell is still running",
            ));
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(job))
    }

    pub(crate) fn start(&self, child: &tokio::process::Child) -> io::Result<()> {
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("shell did not start"))?;
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // CREATE_SUSPENDED starts only the primary thread, so it cannot run code
        // or create descendants before kernel ownership has been established.
        let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
        let mut thread = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut thread) };
        while found != 0 {
            if Some(thread.th32OwnerProcessID) == child.id() {
                let handle =
                    owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread.th32ThreadID) })?;
                if unsafe { ResumeThread(handle.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut thread) };
        }
        Err(io::Error::other(
            "suspended shell primary thread was not found",
        ))
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.terminate();
        // Keep the workspace lease until ordinary termination completes. Any
        // slower cleanup remains protected by the same named kernel job check.
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(active_processes(self.0.as_raw_handle()), Ok(n) if n > 0)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

pub(crate) fn file_identity(file: &File) -> io::Result<(u32, u64)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}
