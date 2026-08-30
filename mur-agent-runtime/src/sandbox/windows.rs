use super::{SandboxPolicy, SandboxStatus};
use anyhow::bail;

pub fn apply_windows(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            bail!(
                "CreateJobObjectW failed: {}",
                std::io::Error::last_os_error()
            );
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        let ok = QueryInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        );
        if ok == 0 {
            windows_sys::Win32::Foundation::CloseHandle(job);
            bail!(
                "QueryInformationJobObject failed: {}",
                std::io::Error::last_os_error()
            );
        }

        // Disable breakaway: child processes cannot escape the job.
        info.BasicLimitInformation.LimitFlags &= !JOB_OBJECT_LIMIT_BREAKAWAY_OK;

        // Apply memory cap from policy (default 512 MB if not set).
        let memory_limit_bytes: usize = (policy.memory_limit_mb.unwrap_or(512) * 1024 * 1024)
            .try_into()
            .unwrap_or(512 * 1024 * 1024);
        info.ProcessMemoryLimit = memory_limit_bytes;
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;

        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            windows_sys::Win32::Foundation::CloseHandle(job);
            bail!(
                "SetInformationJobObject failed: {}",
                std::io::Error::last_os_error()
            );
        }

        let proc = GetCurrentProcess();
        let ok = AssignProcessToJobObject(job, proc);
        if ok == 0 {
            windows_sys::Win32::Foundation::CloseHandle(job);
            bail!(
                "AssignProcessToJobObject failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    Ok(SandboxStatus {
        platform: "windows-job-object".to_string(),
        effective_abi: None,
        enforcing: true,
        dropped: Vec::new(),
    })
}
