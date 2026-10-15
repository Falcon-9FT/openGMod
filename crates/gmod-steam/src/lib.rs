//! Steam, as Garry's Mod talks to it, through the `steam_api64.dll` of the player's own
//! installation: openGMOD signs in as Garry's Mod (app 4000), as the game does.
//!
//! * The player: their SteamID and name, and whether their account has Garry's Mod
//!   ([`Steam::start`] refuses an account without it).
//! * Other players' names, by SteamID ([`Steam::player_name`]): known at once, or asked of
//!   Steam and told by an [`Event::Name`].
//! * The workshop: details of items ([`Steam::request_details`] → [`Event::Details`]),
//!   subscribing and unsubscribing, the items subscribed to.
//!
//! Steam answers later: what it says comes from [`Steam::run_frame`], called once a frame
//! (the manual dispatch of the API - no callbacks into Rust from Steam's threads).
//!
//! This crate is the only one of openGMOD with `unsafe` code: calling the library is.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::fmt;
use std::path::Path;

mod details;
mod sys;

pub use details::Details;

/// Garry's Mod's app on Steam.
pub const APP_ID: u32 = 4000;

/// Why Steam could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// The installation has no Steam library openGMOD can use.
    Library(String),
    /// Steam is not running.
    NotRunning(String),
    /// Steam said no (an outdated Steam, or another reason it gave).
    Failed(String),
    /// Steam runs, but no one is signed in to it.
    NotLoggedOn,
    /// The account signed in has no Garry's Mod.
    NotOwned,
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Library(e) => write!(f, "{e}"),
            StartError::NotRunning(e) => write!(f, "Steam is not running ({e}): start Steam and sign in"),
            StartError::Failed(e) => write!(f, "Steam refused: {e}"),
            StartError::NotLoggedOn => write!(f, "Steam is not signed in to an account"),
            StartError::NotOwned => write!(f, "the Steam account signed in does not own Garry's Mod"),
        }
    }
}

impl std::error::Error for StartError {}

/// What Steam said since the last frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The name of a player asked for ([`Steam::player_name`]) is known now.
    Name(u64),
    /// The details of workshop items asked for ([`Steam::request_details`]): those Steam
    /// knows, or the `EResult` it failed with.
    Details {
        request: u64,
        items: Result<Vec<Details>, i32>,
    },
    /// Subscribing to an item ended with this `EResult` (1: done).
    Subscribed {
        id: u64,
        result: i32,
    },
    Unsubscribed {
        id: u64,
        result: i32,
    },
    /// An item of Garry's Mod was downloaded and is there to mount.
    Installed {
        id: u64,
    },
}

// the callbacks and call results used, by their numbers in the API
const PERSONA_STATE_CHANGE: i32 = 304;
const API_CALL_COMPLETED: i32 = 703;
const SUBSCRIBE_RESULT: i32 = 1313;
const UNSUBSCRIBE_RESULT: i32 = 1315;
const UGC_QUERY_COMPLETED: i32 = 3401;
const ITEM_INSTALLED: i32 = 3405;

/// `k_EResultOK`.
const RESULT_OK: i32 = 1;

/// A call Steam is still to answer, by its handle.
enum Pending {
    Details { ids: Vec<u64> },
    Subscribe(u64),
    Unsubscribe(u64),
}

/// Steam, signed in as Garry's Mod. Steam is left when this goes.
pub struct Steam {
    api: sys::Api,
    pipe: i32,
    user: sys::Interface,
    apps: sys::Interface,
    friends: sys::Interface,
    ugc: sys::Interface,
    pending: RefCell<HashMap<u64, Pending>>,
    /// Players whose names were asked of Steam and are not told yet.
    asked: RefCell<HashSet<u64>>,
}

impl Steam {
    /// Signs in to the Steam running on this computer as Garry's Mod, with the library of the
    /// installation at `install_root`; refused unless someone is signed in whose account owns
    /// the game.
    ///
    /// It sets `SteamAppId` and `SteamGameId` for the API to read: call it before the program
    /// starts threads of its own.
    pub fn start(install_root: &Path) -> Result<Steam, StartError> {
        let path = sys::library_path(install_root);
        if !path.is_file() {
            return Err(StartError::Library(format!("{} is missing: Steam needs it", path.display())));
        }
        let api = sys::load(&path).map_err(StartError::Library)?;
        // SAFETY: as the documentation asks, before the program's threads (which read the
        // environment) are started
        unsafe {
            std::env::set_var("SteamAppId", APP_ID.to_string());
            std::env::set_var("SteamGameId", APP_ID.to_string());
        }
        let mut message: sys::ErrMsg = [0; 1024];
        // SAFETY: a buffer of the size `SteamErrMsg` is
        let result = unsafe { (api.init)(&mut message) };
        // SAFETY: the API leaves a string ending in a 0 there
        let said = unsafe { sys::text(message.as_ptr()) };
        match result {
            0 => {}
            2 => return Err(StartError::NotRunning(said)),
            _ => return Err(StartError::Failed(if said.is_empty() { format!("error {result}") } else { said })),
        }
        // SAFETY: the API is started; these take nothing and give its interfaces
        let steam = unsafe {
            (api.dispatch_init)();
            Steam {
                pipe: (api.pipe)(),
                user: (api.user)(),
                apps: (api.apps)(),
                friends: (api.friends)(),
                ugc: (api.ugc)(),
                api,
                pending: RefCell::new(HashMap::new()),
                asked: RefCell::new(HashSet::new()),
            }
        };
        if [steam.user, steam.apps, steam.friends, steam.ugc].iter().any(|i| i.is_null()) {
            return Err(StartError::Failed("it gave no interface for the user, the apps, friends or the workshop".into()));
        }
        // SAFETY: a valid interface
        if !unsafe { (steam.api.user_logged_on)(steam.user) } {
            return Err(StartError::NotLoggedOn);
        }
        if !steam.owns(APP_ID) {
            return Err(StartError::NotOwned);
        }
        Ok(steam)
    }

    /// The player's SteamID (64-bit).
    pub fn steam_id(&self) -> u64 {
        // SAFETY: a valid interface
        unsafe { (self.api.user_steam_id)(self.user) }
    }

    /// The player's name on Steam.
    pub fn name(&self) -> String {
        // SAFETY: a valid interface; the name stays until the next call
        unsafe { sys::text((self.api.friends_persona_name)(self.friends)) }
    }

    /// Whether the player's account has an app (a game, or its content).
    pub fn owns(&self, app: u32) -> bool {
        // SAFETY: a valid interface
        unsafe { (self.api.apps_subscribed_app)(self.apps, app) }
    }

    /// The name of a player: known now, or asked of Steam - an [`Event::Name`] tells when it
    /// is known.
    pub fn player_name(&self, steam_id: u64) -> Option<String> {
        // SAFETY: a valid interface; true means Steam asks its servers first
        let asking = unsafe { (self.api.friends_request_user_information)(self.friends, steam_id, true) };
        if asking {
            self.asked.borrow_mut().insert(steam_id);
            return None;
        }
        Some(self.known_name(steam_id))
    }

    /// The name Steam has for a player now ("[unknown]" when it has none).
    pub fn known_name(&self, steam_id: u64) -> String {
        // SAFETY: a valid interface; the name stays until the next call
        unsafe { sys::text((self.api.friends_friend_persona_name)(self.friends, steam_id)) }
    }

    /// Asks for the details of workshop items: an [`Event::Details`] with this request's
    /// number brings them. None when Steam would not take the request.
    pub fn request_details(&self, ids: &[u64]) -> Option<u64> {
        if ids.is_empty() {
            return None;
        }
        let count = u32::try_from(ids.len()).ok()?;
        // SAFETY: a valid interface, `count` IDs at `ids`; the query is released when its
        // answer comes, or now if it could not be sent
        unsafe {
            let query = (self.api.ugc_create_details_request)(self.ugc, ids.as_ptr(), count);
            if query == u64::MAX {
                return None;
            }
            (self.api.ugc_set_return_long_description)(self.ugc, query, true);
            let call = (self.api.ugc_send_query)(self.ugc, query);
            if call == 0 {
                (self.api.ugc_release_query)(self.ugc, query);
                return None;
            }
            self.pending.borrow_mut().insert(call, Pending::Details { ids: ids.to_vec() });
            Some(call)
        }
    }

    /// Subscribes to a workshop item: Steam downloads it ([`Event::Subscribed`], then
    /// [`Event::Installed`]).
    pub fn subscribe(&self, id: u64) {
        // SAFETY: a valid interface
        let call = unsafe { (self.api.ugc_subscribe)(self.ugc, id) };
        if call != 0 {
            self.pending.borrow_mut().insert(call, Pending::Subscribe(id));
        }
    }

    /// Unsubscribes from a workshop item ([`Event::Unsubscribed`]).
    pub fn unsubscribe(&self, id: u64) {
        // SAFETY: a valid interface
        let call = unsafe { (self.api.ugc_unsubscribe)(self.ugc, id) };
        if call != 0 {
            self.pending.borrow_mut().insert(call, Pending::Unsubscribe(id));
        }
    }

    /// The workshop items the player is subscribed to; with those switched off in Steam's
    /// library too, or not.
    pub fn subscribed(&self, switched_off_too: bool) -> Vec<u64> {
        // SAFETY: a valid interface, and room for as many IDs as it said there are
        unsafe {
            let count = (self.api.ugc_num_subscribed)(self.ugc, switched_off_too);
            let mut ids = vec![0u64; count as usize];
            let got = (self.api.ugc_subscribed)(self.ugc, ids.as_mut_ptr(), count, switched_off_too);
            ids.truncate(got.min(count) as usize);
            ids
        }
    }

    /// What Steam said since the last frame.
    pub fn run_frame(&self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut message = sys::CallbackMsg { user: 0, callback: 0, param: std::ptr::null(), size: 0 };
        // SAFETY: the manual dispatch as the API documents it: each callback taken is read
        // while it is there, then freed
        unsafe {
            (self.api.run_frame)(self.pipe);
            while (self.api.next_callback)(self.pipe, &mut message) {
                let param = if message.param.is_null() || message.size <= 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(message.param, message.size as usize)
                };
                if message.callback == API_CALL_COMPLETED {
                    self.call_completed(param, &mut events);
                } else {
                    self.callback(message.callback, param, &mut events);
                }
                (self.api.free_last_callback)(self.pipe);
            }
        }
        events
    }

    /// A callback Steam sent of itself.
    fn callback(&self, number: i32, param: &[u8], events: &mut Vec<Event>) {
        match number {
            // PersonaStateChange_t: the SteamID, what changed
            PERSONA_STATE_CHANGE => {
                if let Some(id) = u64_at(param, 0)
                    && self.asked.borrow_mut().remove(&id)
                {
                    events.push(Event::Name(id));
                }
            }
            // ItemInstalled_t: the app, the item
            ITEM_INSTALLED => {
                if u32_at(param, 0) == Some(APP_ID)
                    && let Some(id) = u64_at(param, 8)
                {
                    events.push(Event::Installed { id });
                }
            }
            _ => {}
        }
    }

    /// A call answered: `SteamAPICallCompleted_t` - the call, the number of its result and
    /// its size.
    fn call_completed(&self, param: &[u8], events: &mut Vec<Event>) {
        let (Some(call), Some(expected), Some(size)) = (u64_at(param, 0), i32_at(param, 8), u32_at(param, 12)) else { return };
        let Some(pending) = self.pending.borrow_mut().remove(&call) else { return };
        let mut result = vec![0u8; size as usize];
        let mut failed = false;
        // SAFETY: a buffer of the size Steam gave for this result
        let got =
            unsafe { (self.api.call_result)(self.pipe, call, result.as_mut_ptr().cast::<c_void>(), size as i32, expected, &mut failed) };
        let answered = got && !failed;
        match pending {
            Pending::Details { ids } => {
                let items = if answered && expected == UGC_QUERY_COMPLETED { self.query_completed(&result, &ids) } else { Err(0) };
                events.push(Event::Details { request: call, items });
            }
            // RemoteStorage(Un)subscribePublishedFileResult_t: the result, the item
            Pending::Subscribe(id) => {
                let result = if answered && expected == SUBSCRIBE_RESULT { i32_at(&result, 0).unwrap_or(0) } else { 0 };
                events.push(Event::Subscribed { id, result });
            }
            Pending::Unsubscribe(id) => {
                let result = if answered && expected == UNSUBSCRIBE_RESULT { i32_at(&result, 0).unwrap_or(0) } else { 0 };
                events.push(Event::Unsubscribed { id, result });
            }
        }
    }

    /// `SteamUGCQueryCompleted_t`: the query, its result, how many items it returned. Each
    /// item is read, and the query released.
    fn query_completed(&self, param: &[u8], ids: &[u64]) -> Result<Vec<Details>, i32> {
        let (Some(query), Some(result), Some(count)) = (u64_at(param, 0), i32_at(param, 8), u32_at(param, 12)) else { return Err(0) };
        let mut items = Vec::new();
        if result == RESULT_OK {
            let mut raw = vec![0u8; details::BUFFER];
            let mut url = [0 as std::ffi::c_char; 1024];
            for index in 0..count {
                raw.fill(0);
                // SAFETY: a valid query and room for more than a `SteamUGCDetails_t`; the
                // address is a string ending in a 0 in a buffer of the size given
                let item = unsafe {
                    if !(self.api.ugc_query_result)(self.ugc, query, index, raw.as_mut_ptr()) {
                        continue;
                    }
                    url[0] = 0;
                    (self.api.ugc_query_preview_url)(self.ugc, query, index, url.as_mut_ptr(), url.len() as u32);
                    details::parse(&raw, sys::text(url.as_ptr()))
                };
                match item {
                    // the layout of the details is checked against what was asked for
                    Some(item) if ids.contains(&item.id) => items.push(item),
                    _ => {
                        // SAFETY: a valid query, released once
                        unsafe { (self.api.ugc_release_query)(self.ugc, query) };
                        return Err(0);
                    }
                }
            }
        }
        // SAFETY: a valid query, released once
        unsafe { (self.api.ugc_release_query)(self.ugc, query) };
        if result == RESULT_OK { Ok(items) } else { Err(result) }
    }
}

impl Drop for Steam {
    fn drop(&mut self) {
        // SAFETY: started in `start`, left once
        unsafe { (self.api.shutdown)() };
    }
}

pub(crate) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}
