//! The terminal. openGMOD is a program of windows, as the game is: playing, it opens no
//! terminal of its own, and what it says goes to the console in the game (`). A dedicated
//! server, a master server and `--help` write to the terminal they were started from, or one
//! of their own; what went wrong before there is a window shows in a message box.

/// Whether what is said already goes somewhere: a terminal, or a pipe of the program that
/// started this one.
pub fn has_output() -> bool {
    os::has_output()
}

/// Says what is said in the terminal this program was started from, or a new one: for a
/// program that has no window. Whether there is one now.
pub fn open() -> bool {
    has_output() || os::attach()
}

/// Shows what went wrong where there is no terminal to say it in.
pub fn message(title: &str, text: &str) {
    os::message(title, text);
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod os {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process: u32) -> i32;
        fn AllocConsole() -> i32;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(window: *mut c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ_WRITE: u32 = 3;
    const OPEN_EXISTING: u32 = 3;
    const MB_ICONERROR: u32 = 0x10;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn valid(handle: *mut c_void) -> bool {
        !handle.is_null() && handle as isize != -1
    }

    pub fn has_output() -> bool {
        // SAFETY: a documented constant; the handle is only looked at
        valid(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) })
    }

    pub fn attach() -> bool {
        // SAFETY: documented constants; no pointers
        let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 || AllocConsole() != 0 };
        if !attached {
            return false;
        }
        // what writes and reads goes to the terminal, where nothing was given in its place
        for (which, name, access) in [
            (STD_INPUT_HANDLE, "CONIN$", GENERIC_READ | GENERIC_WRITE),
            (STD_OUTPUT_HANDLE, "CONOUT$", GENERIC_READ | GENERIC_WRITE),
            (STD_ERROR_HANDLE, "CONOUT$", GENERIC_READ | GENERIC_WRITE),
        ] {
            // SAFETY: a name ending in a 0, documented flags, no security or template; a
            // handle opened is the process's for as long as it runs
            unsafe {
                if valid(GetStdHandle(which)) {
                    continue;
                }
                let name = wide(name);
                let handle =
                    CreateFileW(name.as_ptr(), access, FILE_SHARE_READ_WRITE, std::ptr::null_mut(), OPEN_EXISTING, 0, std::ptr::null_mut());
                if valid(handle) {
                    SetStdHandle(which, handle);
                }
            }
        }
        true
    }

    pub fn message(title: &str, text: &str) {
        let (title, text) = (wide(title), wide(text));
        // SAFETY: texts ending in a 0, no owner window
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_ICONERROR) };
    }
}

#[cfg(not(windows))]
mod os {
    pub fn has_output() -> bool {
        true
    }

    pub fn attach() -> bool {
        true
    }

    pub fn message(_: &str, _: &str) {}
}
