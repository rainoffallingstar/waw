//! Windows platform bindings: shell execution, job objects, and thread
//! resumption for suspended child processes.
//!
//! Everything here is `cfg(windows)`; the crate builds without it on other
//! hosts, where the elevation paths return an error instead.

#[cfg(windows)]
pub type Handle = *mut std::ffi::c_void;
#[cfg(windows)]
pub type Hwnd = Handle;
#[cfg(windows)]
pub type Hinstance = Handle;
#[cfg(windows)]
pub type Hkey = Handle;
#[cfg(windows)]
pub type Hicon = Handle;
#[cfg(windows)]
pub type Hmonitor = Handle;

#[cfg(windows)]
pub const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
#[cfg(windows)]
pub const WAIT_OBJECT_0: u32 = 0x0000_0000;
#[cfg(windows)]
pub const WAIT_TIMEOUT: u32 = 0x0000_0102;
#[cfg(windows)]
pub const WAIT_FAILED: u32 = 0xFFFF_FFFF;
#[cfg(windows)]
pub const ERROR_CANCELLED: i32 = 1223;

#[cfg(windows)]
#[allow(non_snake_case)]
#[repr(C)]
pub union ShellExecuteInfoUnion {
    pub hIcon: Hicon,
    pub hMonitor: Hmonitor,
}

#[cfg(windows)]
#[allow(non_snake_case)]
#[repr(C)]
pub struct ShellExecuteInfoW {
    pub cbSize: u32,
    pub fMask: u32,
    pub hwnd: Hwnd,
    pub lpVerb: *const u16,
    pub lpFile: *const u16,
    pub lpParameters: *const u16,
    pub lpDirectory: *const u16,
    pub nShow: i32,
    pub hInstApp: Hinstance,
    pub lpIDList: *mut std::ffi::c_void,
    pub lpClass: *const u16,
    pub hkeyClass: Hkey,
    pub dwHotKey: u32,
    pub Anonymous: ShellExecuteInfoUnion,
    pub hProcess: Handle,
}

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    pub fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    pub fn GetExitCodeProcess(handle: Handle, exit_code: *mut u32) -> i32;
    pub fn CloseHandle(handle: Handle) -> i32;
}

#[cfg(windows)]
pub const CREATE_SUSPENDED: u32 = 0x0000_0004;
#[cfg(windows)]
pub const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;
#[cfg(windows)]
pub const THREAD_SUSPEND_RESUME: u32 = 0x0002;
#[cfg(windows)]
pub const INVALID_HANDLE_VALUE: isize = -1;
#[cfg(windows)]
pub const PROCESS_TERMINATE: u32 = 0x0001;
#[cfg(windows)]
pub const PROCESS_SET_QUOTA: u32 = 0x0100;

#[cfg(windows)]
#[repr(C)]
pub struct ThreadEntry32 {
    pub dw_size: u32,
    pub cnt_usage: u32,
    pub th32_thread_id: u32,
    pub th32_owner_process_id: u32,
    pub tp_base_pri: i32,
    pub tp_priority_delta: i32,
    pub dw_flags: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn CreateJobObjectW(attributes: *mut std::ffi::c_void, name: *const u16) -> Handle;
    pub fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    pub fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
    pub fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
    pub fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry32) -> i32;
    pub fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry32) -> i32;
    pub fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> Handle;
    pub fn ResumeThread(thread: Handle) -> u32;
    pub fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> Handle;
    pub fn TerminateProcess(handle: Handle, exit_code: u32) -> i32;
}

/// Owns a Windows job object so that a timed-out backend command can be
/// terminated together with every process it spawned.
///
/// The process is started suspended and attached to the job before any of its
/// threads run, which closes the race where a wrapper such as `scoop.cmd`
/// could spawn a grandchild before the assignment happened.
#[cfg(windows)]
pub struct ProcessJob {
    handle: Handle,
}

#[cfg(windows)]
impl ProcessJob {
    pub fn create() -> Option<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    pub fn assign(&self, process: Handle) -> bool {
        unsafe { AssignProcessToJobObject(self.handle, process) != 0 }
    }

    pub fn terminate(&self, exit_code: u32) {
        if !self.handle.is_null() {
            unsafe {
                TerminateJobObject(self.handle, exit_code);
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
pub fn resume_process_threads(process_id: u32) -> usize {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot as isize == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return 0;
    }

    let mut entry: ThreadEntry32 = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ThreadEntry32>() as u32;

    let mut resumed = 0usize;
    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while has_entry {
        if entry.th32_owner_process_id == process_id {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32_thread_id) };
            if !thread.is_null() {
                unsafe {
                    ResumeThread(thread);
                    CloseHandle(thread);
                }
                resumed += 1;
            }
        }
        has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }

    unsafe {
        CloseHandle(snapshot);
    }
    resumed
}
