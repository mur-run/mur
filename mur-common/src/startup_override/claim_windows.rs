//! Windows claim. `std::fs::rename` is `MoveFileExW`, which opens the source
//! with sharing allowed and renames through that handle. Two racing claims can
//! both open the live file; the first rename moves it, the second handle
//! follows the file and renames it again, and both report success.
//!
//! Here the live file is opened with no sharing, so only one side holds a
//! handle at a time; the rename goes through that handle. The other side gets
//! `ERROR_SHARING_VIOLATION`, waits, and retries until the live name is gone
//! (`NotFound` → the caller maps it to `Absent`) or it gets the handle itself.

use std::fs::OpenOptions;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::Duration;

use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
};

/// How long the losing side waits between attempts while the winner holds
/// the live file open. The winner holds it only for one rename call.
const SHARING_RETRY_INTERVAL: Duration = Duration::from_millis(5);
/// Attempts before giving up with the sharing error (`Failed`, never
/// `Absent`): ~2 s at [`SHARING_RETRY_INTERVAL`].
const SHARING_RETRY_ATTEMPTS: u32 = 400;

pub(super) fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(0)
            .open(from)
        {
            Ok(file) => return rename_handle(&file, to),
            Err(e)
                if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
                    && attempt < SHARING_RETRY_ATTEMPTS =>
            {
                attempt += 1;
                std::thread::sleep(SHARING_RETRY_INTERVAL);
            }
            Err(e) => return Err(e),
        }
    }
}

/// `SetFileInformationByHandle(FileRenameInfo)` with replace-if-exists, the
/// same overwrite behaviour as `std::fs::rename`.
fn rename_handle(file: &std::fs::File, to: &Path) -> io::Result<()> {
    let to = std::path::absolute(to)?;
    let name: Vec<u16> = to.as_os_str().encode_wide().collect();
    let name_bytes = name.len() * size_of::<u16>();
    let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    // Header + name + NUL, in u64 words so the buffer meets the struct's
    // alignment (it holds a pointer-sized HANDLE).
    let total = header + name_bytes + size_of::<u16>();
    let mut buf = vec![0u64; total.div_ceil(size_of::<u64>())];
    let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `buf` is zeroed, aligned for FILE_RENAME_INFO, and large enough
    // for the header plus `name` and a NUL terminator.
    let ok = unsafe {
        (*info).Anonymous.ReplaceIfExists = 1;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = u32::try_from(name_bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path too long"))?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        SetFileInformationByHandle(
            file.as_raw_handle() as _,
            FileRenameInfo,
            info.cast(),
            u32::try_from(total)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path too long"))?,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
