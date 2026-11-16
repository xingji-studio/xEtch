//! Own helper processes and terminate them when their backend disappears.
use std::{
    io,
    ops::{Deref, DerefMut},
    process::{Child, Command},
};

pub struct ManagedChild {
    child: Child,
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
}

impl Deref for ManagedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl ManagedChild {
    pub fn output(&mut self) -> io::Result<std::process::Output> {
        use std::io::Read;
        let mut stdout = self
            .child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("stdout pipe required"))?;
        let mut stderr = self
            .child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("stderr pipe required"))?;
        std::thread::scope(|scope| {
            let error_reader = scope.spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes)?;
                Ok::<_, io::Error>(bytes)
            });
            let mut bytes = Vec::new();
            let read = stdout.read_to_end(&mut bytes);
            if read.is_err() {
                let _ = self.child.kill();
            }
            let status = self.child.wait();
            let errors = error_reader
                .join()
                .map_err(|_| io::Error::other("helper stderr reader panicked"))??;
            read?;
            Ok(std::process::Output {
                status: status?,
                stdout: bytes,
                stderr: errors,
            })
        })
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn spawn(command: &mut Command) -> io::Result<ManagedChild> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: The fork hook only invokes async-signal-safe system calls.
        // Check the parent again to cover death between fork and prctl.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    let job = {
        use std::{mem, os::windows::io::FromRawHandle, ptr};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        // SAFETY: Anonymous non-inheritable job, owned until backend exit.
        unsafe {
            let raw = CreateJobObjectW(ptr::null(), ptr::null());
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = std::os::windows::io::OwnedHandle::from_raw_handle(raw);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            job
        }
    };
    #[allow(unused_mut)]
    let mut child = command.spawn()?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: Both handles are live and belong to this invocation.
        if unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                job.as_raw_handle(),
                child.as_raw_handle(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }
    Ok(ManagedChild {
        child,
        #[cfg(windows)]
        _job: job,
    })
}
