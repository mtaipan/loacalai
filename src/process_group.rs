//! Ownership of an entire command group, including children surviving their parent.
use anyhow::{Context, Result};

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        ffi::c_void,
        mem::{size_of, zeroed},
    };
    type Handle = *mut c_void;
    #[repr(C)]
    struct Basic {
        process_time: i64,
        job_time: i64,
        flags: u32,
        min_working: usize,
        max_working: usize,
        active: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }
    #[repr(C)]
    struct Extended {
        basic: Basic,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process: usize,
        peak_job: usize,
    }
    #[repr(C)]
    struct Thread {
        size: u32,
        usage: u32,
        id: u32,
        owner: u32,
        priority: i32,
        delta: i32,
        flags: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: Handle, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, size: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Thread32First(snapshot: Handle, entry: *mut Thread) -> i32;
        fn Thread32Next(snapshot: Handle, entry: *mut Thread) -> i32;
        fn OpenThread(access: u32, inherit: i32, id: u32) -> Handle;
        fn ResumeThread(thread: Handle) -> u32;
    }
    pub struct Group(isize);
    impl Group {
        pub fn prepare(command: &mut tokio::process::Command) -> Result<Self> {
            // Child cannot run or spawn descendants until assignment succeeds.
            command.creation_flags(0x08000000 | 0x00000004);
            let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
            anyhow::ensure!(
                !handle.is_null(),
                "CreateJobObject: {}",
                std::io::Error::last_os_error()
            );
            let group = Self(handle as isize);
            let mut info: Extended = unsafe { zeroed() };
            info.basic.flags = 0x00002000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; no breakaway.
            anyhow::ensure!(
                unsafe {
                    SetInformationJobObject(
                        handle,
                        9,
                        &info as *const _ as _,
                        size_of::<Extended>() as u32,
                    )
                } != 0,
                "SetInformationJobObject: {}",
                std::io::Error::last_os_error()
            );
            Ok(group)
        }
        pub fn attach(&mut self, child: &tokio::process::Child) -> Result<()> {
            let process = child
                .raw_handle()
                .context("Нет handle дочернего процесса")?;
            anyhow::ensure!(
                unsafe { AssignProcessToJobObject(self.0 as Handle, process) } != 0,
                "AssignProcessToJobObject: {}",
                std::io::Error::last_os_error()
            );
            let pid = child.id().context("Нет PID дочернего процесса")?;
            // Tokio retains the process handle, but not the initial thread handle.
            // A suspended new process has a single initial thread; resume only its PID.
            let snapshot = unsafe { CreateToolhelp32Snapshot(4, 0) };
            anyhow::ensure!(
                snapshot as isize != -1,
                "Thread snapshot: {}",
                std::io::Error::last_os_error()
            );
            let result = (|| -> Result<()> {
                let mut entry: Thread = unsafe { zeroed() };
                entry.size = size_of::<Thread>() as u32;
                let mut found = unsafe { Thread32First(snapshot, &mut entry) } != 0;
                while found {
                    if entry.owner == pid {
                        let thread = unsafe { OpenThread(2, 0, entry.id) };
                        anyhow::ensure!(
                            !thread.is_null(),
                            "OpenThread: {}",
                            std::io::Error::last_os_error()
                        );
                        let resumed = unsafe { ResumeThread(thread) };
                        unsafe { CloseHandle(thread) };
                        anyhow::ensure!(
                            resumed != u32::MAX,
                            "ResumeThread: {}",
                            std::io::Error::last_os_error()
                        );
                        return Ok(());
                    }
                    found = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
                }
                anyhow::bail!("Не найден начальный поток приостановленного процесса")
            })();
            unsafe { CloseHandle(snapshot) };
            result
        }
        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0 as Handle, 1);
            }
        }
    }
    impl Drop for Group {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0 as Handle);
            }
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    pub struct Group(Option<i32>);
    impl Group {
        pub fn prepare(command: &mut tokio::process::Command) -> Result<Self> {
            command.process_group(0);
            Ok(Self(None))
        }
        pub fn attach(&mut self, child: &tokio::process::Child) -> Result<()> {
            self.0 = Some(child.id().context("Нет PID процесса")? as i32);
            Ok(())
        }
        pub fn terminate(&self) {
            if let Some(pid) = self.0 {
                extern "C" {
                    fn kill(pid: i32, signal: i32) -> i32;
                }
                unsafe {
                    kill(-pid, 9);
                }
            }
        }
    }
    impl Drop for Group {
        fn drop(&mut self) {
            self.terminate();
        }
    }
}
pub use platform::Group;
