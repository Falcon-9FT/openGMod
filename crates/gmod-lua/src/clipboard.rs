//! The system's clipboard, which `SetClipboardText`, text entries and the console copy to and
//! paste from. Tests keep one of their own, leaving the player's alone; elsewhere than Windows
//! it is the program's own for now.

#[cfg(any(test, not(windows)))]
mod own {
    use std::cell::RefCell;

    thread_local! {
        static CLIPBOARD: RefCell<String> = const { RefCell::new(String::new()) };
    }

    pub fn set(text: &str) {
        CLIPBOARD.with(|c| *c.borrow_mut() = text.to_string());
    }

    pub fn get() -> String {
        CLIPBOARD.with(|c| c.borrow().clone())
    }
}

#[cfg(any(test, not(windows)))]
pub(crate) use own::{get, set};

// Windows' own functions for it: the only way there is to it
#[cfg(all(windows, not(test)))]
#[allow(unsafe_code)]
mod os {
    use std::ffi::c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(format: u32, memory: *mut c_void) -> *mut c_void;
        fn GetClipboardData(format: u32) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalFree(memory: *mut c_void) -> *mut c_void;
        fn GlobalLock(memory: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(memory: *mut c_void) -> i32;
        fn GlobalSize(memory: *mut c_void) -> usize;
    }

    /// Text in UTF-16, ending in a 0.
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 2;

    /// The clipboard, open until dropped; another program may have it for a moment.
    struct Open;

    impl Open {
        fn new() -> Option<Open> {
            for _ in 0..10 {
                // SAFETY: no window owns it
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    return Some(Open);
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            None
        }
    }

    impl Drop for Open {
        fn drop(&mut self) {
            // SAFETY: opened by this thread
            unsafe { CloseClipboard() };
        }
    }

    pub fn set(text: &str) {
        let wide: Vec<u16> = text.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16().chain(std::iter::once(0)).collect();
        let Some(_open) = Open::new() else { return };
        // SAFETY: the clipboard is open; the memory is as large as what is copied into it, and
        // the clipboard owns it once given (freed here only if it was not taken)
        unsafe {
            EmptyClipboard();
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            if memory.is_null() {
                return;
            }
            let at = GlobalLock(memory).cast::<u16>();
            if at.is_null() {
                GlobalFree(memory);
                return;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), at, wide.len());
            GlobalUnlock(memory);
            if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
                GlobalFree(memory);
            }
        }
    }

    pub fn get() -> String {
        let Some(_open) = Open::new() else { return String::new() };
        // SAFETY: the clipboard is open; its text is read no further than its memory and its 0
        unsafe {
            let memory = GetClipboardData(CF_UNICODETEXT);
            if memory.is_null() {
                return String::new();
            }
            let at = GlobalLock(memory).cast::<u16>();
            if at.is_null() {
                return String::new();
            }
            let most = GlobalSize(memory) / 2;
            let units = std::slice::from_raw_parts(at, most);
            let end = units.iter().position(|&u| u == 0).unwrap_or(most);
            let text = String::from_utf16_lossy(&units[..end]);
            GlobalUnlock(memory);
            text.replace("\r\n", "\n")
        }
    }
}

#[cfg(all(windows, not(test)))]
pub(crate) use os::{get, set};
