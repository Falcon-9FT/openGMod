//! The C functions of the Steam API (`steam_api_flat.h`), taken from the `steam_api64.dll` of
//! the player's Garry's Mod at run time: openGMOD does not carry Valve's library, nor link to
//! one version of it. The interfaces are asked for by the newest version the library has
//! (`SteamAPI_SteamUGC_v021` ...): their functions used here have stayed the same.

use std::ffi::{CStr, c_char, c_void};
use std::path::Path;

/// An interface of the API (`ISteamUser*` ...).
pub(crate) type Interface = *mut c_void;

/// `CallbackMsg_t`: a callback the manual dispatch hands out.
#[repr(C)]
pub(crate) struct CallbackMsg {
    pub user: i32,
    pub callback: i32,
    pub param: *const u8,
    pub size: i32,
}

/// `SteamErrMsg`: what `SteamAPI_InitFlat` says went wrong.
pub(crate) type ErrMsg = [c_char; 1024];

/// The functions used, and the interfaces they act on.
pub(crate) struct Api {
    pub init: unsafe extern "C" fn(*mut ErrMsg) -> i32,
    pub shutdown: unsafe extern "C" fn(),
    pub pipe: unsafe extern "C" fn() -> i32,
    pub dispatch_init: unsafe extern "C" fn(),
    pub run_frame: unsafe extern "C" fn(i32),
    pub next_callback: unsafe extern "C" fn(i32, *mut CallbackMsg) -> bool,
    pub free_last_callback: unsafe extern "C" fn(i32),
    pub call_result: unsafe extern "C" fn(i32, u64, *mut c_void, i32, i32, *mut bool) -> bool,

    pub user: unsafe extern "C" fn() -> Interface,
    pub apps: unsafe extern "C" fn() -> Interface,
    pub friends: unsafe extern "C" fn() -> Interface,
    pub ugc: unsafe extern "C" fn() -> Interface,

    pub user_steam_id: unsafe extern "C" fn(Interface) -> u64,
    pub user_logged_on: unsafe extern "C" fn(Interface) -> bool,
    pub apps_subscribed_app: unsafe extern "C" fn(Interface, u32) -> bool,
    pub friends_persona_name: unsafe extern "C" fn(Interface) -> *const c_char,
    pub friends_request_user_information: unsafe extern "C" fn(Interface, u64, bool) -> bool,
    pub friends_friend_persona_name: unsafe extern "C" fn(Interface, u64) -> *const c_char,
    pub ugc_create_details_request: unsafe extern "C" fn(Interface, *const u64, u32) -> u64,
    pub ugc_set_return_long_description: unsafe extern "C" fn(Interface, u64, bool) -> bool,
    pub ugc_send_query: unsafe extern "C" fn(Interface, u64) -> u64,
    pub ugc_query_result: unsafe extern "C" fn(Interface, u64, u32, *mut u8) -> bool,
    pub ugc_query_preview_url: unsafe extern "C" fn(Interface, u64, u32, *mut c_char, u32) -> bool,
    pub ugc_release_query: unsafe extern "C" fn(Interface, u64) -> bool,
    pub ugc_subscribe: unsafe extern "C" fn(Interface, u64) -> u64,
    pub ugc_unsubscribe: unsafe extern "C" fn(Interface, u64) -> u64,
    // the newer libraries take whether items switched off are counted too; an older one
    // leaves the argument it does not take where it is
    pub ugc_num_subscribed: unsafe extern "C" fn(Interface, bool) -> u32,
    pub ugc_subscribed: unsafe extern "C" fn(Interface, *mut u64, u32, bool) -> u32,
}

/// Where Garry's Mod keeps the library, from its folder.
pub(crate) fn library_path(install_root: &Path) -> std::path::PathBuf {
    if cfg!(target_pointer_width = "64") {
        install_root.join("bin").join("win64").join("steam_api64.dll")
    } else {
        install_root.join("bin").join("steam_api.dll")
    }
}

#[cfg(windows)]
mod os {
    use std::ffi::{c_char, c_void};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        pub fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    /// The library's own folder is where what it loads is looked for first.
    pub const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 8;
}

/// The address of a function as the type of function it is.
///
/// # Safety
/// `F` is a function pointer type, of the function at `address`.
#[cfg(windows)]
unsafe fn function_pointer<F: Copy>(address: *mut c_void) -> F {
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*mut c_void>(), "a function pointer");
    // SAFETY: as the caller promises; of the same size, checked
    unsafe { std::mem::transmute_copy::<*mut c_void, F>(&address) }
}

/// Loads the library and takes its functions. It stays loaded as long as the program runs.
#[cfg(windows)]
pub(crate) fn load(path: &Path) -> Result<Api, String> {
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: a path ending in a 0, no file handle, a documented flag
    let module = unsafe { os::LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), os::LOAD_WITH_ALTERED_SEARCH_PATH) };
    if module.is_null() {
        return Err(format!("{} could not be loaded: {}", path.display(), std::io::Error::last_os_error()));
    }
    let find = |name: &CStr| -> Option<*mut c_void> {
        // SAFETY: a loaded module and a name ending in a 0
        let f = unsafe { os::GetProcAddress(module, name.as_ptr()) };
        (!f.is_null()).then_some(f)
    };
    let missing = |name: &str| format!("{} has no {name}: an older Steam API than openGMOD knows", path.display());
    macro_rules! function {
        ($name:literal) => {{
            let name = CStr::from_bytes_with_nul(concat!($name, "\0").as_bytes()).expect("a name ending in a 0");
            let f = find(name).ok_or_else(|| missing($name))?;
            // SAFETY: the function of that name in steam_api_flat.h has the type of the field
            // it goes into
            unsafe { function_pointer(f) }
        }};
    }
    // the newest version of an interface the library has
    let interface = |name: &str, oldest: u32| -> Result<unsafe extern "C" fn() -> Interface, String> {
        (oldest..=oldest + 30)
            .rev()
            .find_map(|version| {
                let symbol = std::ffi::CString::new(format!("SteamAPI_{name}_v{version:03}")).ok()?;
                find(&symbol)
            })
            // SAFETY: every `SteamAPI_<interface>_v<n>` takes nothing and gives the interface
            .map(|f| unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> Interface>(f) })
            .ok_or_else(|| missing(&format!("SteamAPI_{name}_v{oldest:03}")))
    };
    Ok(Api {
        init: function!("SteamAPI_InitFlat"),
        shutdown: function!("SteamAPI_Shutdown"),
        pipe: function!("SteamAPI_GetHSteamPipe"),
        dispatch_init: function!("SteamAPI_ManualDispatch_Init"),
        run_frame: function!("SteamAPI_ManualDispatch_RunFrame"),
        next_callback: function!("SteamAPI_ManualDispatch_GetNextCallback"),
        free_last_callback: function!("SteamAPI_ManualDispatch_FreeLastCallback"),
        call_result: function!("SteamAPI_ManualDispatch_GetAPICallResult"),
        user: interface("SteamUser", 20)?,
        apps: interface("SteamApps", 8)?,
        friends: interface("SteamFriends", 17)?,
        ugc: interface("SteamUGC", 14)?,
        user_steam_id: function!("SteamAPI_ISteamUser_GetSteamID"),
        user_logged_on: function!("SteamAPI_ISteamUser_BLoggedOn"),
        apps_subscribed_app: function!("SteamAPI_ISteamApps_BIsSubscribedApp"),
        friends_persona_name: function!("SteamAPI_ISteamFriends_GetPersonaName"),
        friends_request_user_information: function!("SteamAPI_ISteamFriends_RequestUserInformation"),
        friends_friend_persona_name: function!("SteamAPI_ISteamFriends_GetFriendPersonaName"),
        ugc_create_details_request: function!("SteamAPI_ISteamUGC_CreateQueryUGCDetailsRequest"),
        ugc_set_return_long_description: function!("SteamAPI_ISteamUGC_SetReturnLongDescription"),
        ugc_send_query: function!("SteamAPI_ISteamUGC_SendQueryUGCRequest"),
        ugc_query_result: function!("SteamAPI_ISteamUGC_GetQueryUGCResult"),
        ugc_query_preview_url: function!("SteamAPI_ISteamUGC_GetQueryUGCPreviewURL"),
        ugc_release_query: function!("SteamAPI_ISteamUGC_ReleaseQueryUGCRequest"),
        ugc_subscribe: function!("SteamAPI_ISteamUGC_SubscribeItem"),
        ugc_unsubscribe: function!("SteamAPI_ISteamUGC_UnsubscribeItem"),
        ugc_num_subscribed: function!("SteamAPI_ISteamUGC_GetNumSubscribedItems"),
        ugc_subscribed: function!("SteamAPI_ISteamUGC_GetSubscribedItems"),
    })
}

/// Steam is taken from Garry's Mod's own library, which openGMOD loads on Windows only yet.
#[cfg(not(windows))]
pub(crate) fn load(path: &Path) -> Result<Api, String> {
    Err(format!("{}: Steam is only on Windows in openGMOD yet", path.display()))
}

/// A string the API gives, which it keeps: copied while it is there.
///
/// # Safety
/// `text` is null or a string ending in a 0 that lives until this returns.
pub(crate) unsafe fn text(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: as the caller promises
    unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
}
