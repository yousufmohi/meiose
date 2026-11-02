//! Make the MCP server's whole process tree die with it.
//!
//! Servers are often launched through wrappers (`npx` → `node`, `uvx` →
//! `python`, `cmd /c …`). Killing only the direct child would orphan the
//! real server. So:
//! - Windows: the child is put in a Job Object with KILL_ON_JOB_CLOSE. Every
//!   descendant inherits the job, and when meiose exits, even if it is
//!   killed outright, Windows closes the handle and kills the whole tree.
//! - Unix: the child leads its own process group; a forced kill signals the
//!   group.
//!
//! The graceful path (close the server's stdin, wait) is unchanged; this is
//! only the backstop.

use tokio::process::{Child, Command};

/// Prepare a command so its tree can be killed as a unit (call before spawn).
pub fn configure(cmd: &mut Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

pub struct Tree {
    #[cfg(windows)]
    job: Option<windows::Job>,
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl Tree {
    /// Attach to a freshly spawned child. Never fails: if the OS refuses,
    /// we log it and fall back to killing just the child.
    pub fn attach(child: &Child) -> Tree {
        #[cfg(windows)]
        {
            let job = child.raw_handle().and_then(|h| match windows::Job::assign(h) {
                Ok(j) => Some(j),
                Err(code) => {
                    tracing::warn!(code, "could not put the MCP server in a job object; a forced kill may leave its children running");
                    None
                }
            });
            Tree { job }
        }
        #[cfg(unix)]
        {
            Tree {
                pgid: child.id().map(|p| p as i32),
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = child;
            Tree {}
        }
    }

    /// Kill the child and everything it started.
    pub async fn kill(&self, child: &mut Child) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            // SAFETY: plain syscall; a negative pid addresses the group.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
        let _ = child.kill().await;
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::RawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // The handle is only used through thread-safe kernel calls.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        /// Create a kill-on-close job and put `process` in it.
        pub fn assign(process: RawHandle) -> Result<Job, u32> {
            // SAFETY: documented Win32 calls with valid arguments; every
            // failure path closes the job handle.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(GetLastError());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 || AssignProcessToJobObject(job, process as HANDLE) == 0 {
                    let err = GetLastError();
                    CloseHandle(job);
                    return Err(err);
                }
                Ok(Job(job))
            }
        }

        pub fn terminate(&self) {
            // SAFETY: self.0 is a live job handle owned by us.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // Closing the last handle kills every process still in the job.
            // SAFETY: we own the handle and close it exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
