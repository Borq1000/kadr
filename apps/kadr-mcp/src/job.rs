//! A Windows Job Object that kills the Kadr we launched when `kadr-mcp`
//! goes away for any reason (including TerminateProcess, where no Drop
//! runs): the kernel closes our handle and the job's kill-on-close limit
//! takes the child (and its ffmpeg children) down. No-op elsewhere.

#[cfg(windows)]
pub struct Job(*mut std::ffi::c_void);

// SAFETY: a job handle is a plain kernel handle, usable from any thread.
#[cfg(windows)]
unsafe impl Send for Job {}

#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;
    pub type Handle = *mut c_void;

    /// JOBOBJECT_BASIC_LIMIT_INFORMATION
    #[repr(C)]
    #[derive(Default)]
    pub struct BasicLimit {
        pub per_process_user_time_limit: i64,
        pub per_job_user_time_limit: i64,
        pub limit_flags: u32,
        pub minimum_working_set_size: usize,
        pub maximum_working_set_size: usize,
        pub active_process_limit: u32,
        pub affinity: usize,
        pub priority_class: u32,
        pub scheduling_class: u32,
    }

    /// JOBOBJECT_EXTENDED_LIMIT_INFORMATION (IO_COUNTERS = six u64).
    #[repr(C)]
    #[derive(Default)]
    pub struct ExtendedLimit {
        pub basic: BasicLimit,
        pub io: [u64; 6],
        pub process_memory_limit: usize,
        pub job_memory_limit: usize,
        pub peak_process_memory_used: usize,
        pub peak_job_memory_used: usize,
    }

    pub const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> Handle;
        pub fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, len: u32) -> i32;
        pub fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        pub fn CloseHandle(h: Handle) -> i32;
    }
}

#[cfg(windows)]
impl Job {
    /// Puts `child` in a new kill-on-close job. `None` if any step fails
    /// (Kadr's own `--parent-pid` watch still covers that case).
    pub fn kill_on_close(child: &std::process::Child) -> Option<Job> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: plain Win32 calls; `info` outlives the call that reads it,
        // and the job handle is owned by the returned `Job` (closed on drop).
        unsafe {
            let h = ffi::CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
            if h.is_null() {
                return None;
            }
            let job = Job(h);
            let mut info = ffi::ExtendedLimit::default();
            info.basic.limit_flags = ffi::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = ffi::SetInformationJobObject(
                h,
                ffi::JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                &info as *const ffi::ExtendedLimit as *const std::ffi::c_void,
                std::mem::size_of::<ffi::ExtendedLimit>() as u32,
            ) != 0
                && ffi::AssignProcessToJobObject(h, child.as_raw_handle() as ffi::Handle) != 0;
            ok.then_some(job)
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: we own this handle.
        unsafe {
            ffi::CloseHandle(self.0);
        }
    }
}

#[cfg(not(windows))]
pub struct Job;

#[cfg(not(windows))]
impl Job {
    pub fn kill_on_close(_child: &std::process::Child) -> Option<Job> {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn extended_limit_information_has_the_win32_size() {
        // sizeof(JOBOBJECT_EXTENDED_LIMIT_INFORMATION): 144 on x64, 112 on x86.
        let expected = if cfg!(target_pointer_width = "64") { 144 } else { 112 };
        assert_eq!(std::mem::size_of::<super::ffi::ExtendedLimit>(), expected);
    }

    #[test]
    fn a_child_in_the_job_dies_when_the_job_closes() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let job = super::Job::kill_on_close(&child).expect("job created and assigned");
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        let t = std::time::Instant::now();
        while child.try_wait().unwrap().is_none() {
            if t.elapsed() > std::time::Duration::from_secs(5) {
                let _ = child.kill();
                panic!("child survived closing the job");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
