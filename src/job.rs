//! A Windows job object that kills the JVM when the host exits.
//!
//! Rule: the game dying must not leave a zombie JVM behind.  The host owns the
//! job.  When the host's handle closes, cleanly or by being killed, Windows
//! terminates everything assigned to it.

#[cfg(windows)]
mod imp {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct Job(HANDLE);

    // The handle is only ever used to assign children and is closed by the OS
    // when the process exits, and moving it between threads is safe.
    unsafe impl Send for Job {}

    impl Job {
        pub fn create() -> Option<Job> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
                    std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags =
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_mut(&mut info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    return None;
                }
                Some(Job(handle))
            }
        }

        pub fn adopt(&self, child: &Child) -> bool {
            unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) != 0 }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::process::Child;

    pub struct Job;

    impl Job {
        pub fn create() -> Option<Job> {
            None
        }
        pub fn adopt(&self, _child: &Child) -> bool {
            false
        }
    }
}

pub use imp::Job;
