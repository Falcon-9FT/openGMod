//! The main menu, as Garry's Mod has it: the Lua of the menu (`lua/menu/menu.lua`) with its
//! HTML page (`asset://garrysmod/html/menu.html`), drawn by Chromium ([`gmod_web`]). It starts
//! a game (`map`), leaves one (`disconnect`), switches addons on and off, quits; what it asks
//! of the engine comes to the program as console lines ([`Menu::take_commands`]).
//!
//! Chromium draws the HTML panels of the game's client too (the spawn menu's dupes and saves,
//! an addon's `DHTML`): [`web_pages`] makes pages for any realm.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gmod_fs::Vfs;
use gmod_fs::install::Install;
use gmod_fs::workshop::{self, AddonFile};
use gmod_lua::{Addon, MountableGame, Output, Realm, Runtime, WebButton, WebEvent, WebPage, WebPages};
use gmod_material::png::Rgba;
use gmod_web::{Button, Modifiers, PageEvent, Web};

/// Starts Chromium with `asset://` serving the game's files from `vfs`. None, with why, when it
/// cannot: the program runs on without HTML.
pub fn start_web(vfs: Arc<Vfs>) -> Result<Web, String> {
    let root = gmod_fs::UserDir::default_root().ok_or("no home folder for Chromium's cache")?;
    let log = root.join("logs").join("chromium.log");
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    println!("Chromium's log: {}", log.display());
    let files: gmod_web::Files = Arc::new(move |host, path| asset(&vfs, host, path));
    Web::start(&root.join("cache").join("chromium"), &log, files)
}

/// What `asset://` serves: `asset://garrysmod/<path>` the game's files, and the previews of
/// workshop items the menu downloaded ([`gmod_lua::menu::PREVIEWS`], in openGMOD's folder);
/// `asset://mapimage/<map>` a map's picture (`maps/thumb/<map>.png`, else the game's "no
/// picture").
fn asset(vfs: &Vfs, host: &str, path: &str) -> Option<Vec<u8>> {
    if host.eq_ignore_ascii_case("garrysmod") {
        if path.strip_prefix(gmod_lua::menu::PREVIEWS).is_some_and(|rest| rest.starts_with('/')) {
            let root = gmod_fs::UserDir::default_root()?;
            return gmod_fs::UserDir::open(root, None).ok()?.read(path).ok();
        }
        return vfs.read(path, "GAME").ok();
    }
    if host.eq_ignore_ascii_case("mapimage") {
        let map = path.trim_matches('/');
        return [format!("maps/thumb/{map}.png"), format!("maps/{map}.png"), "materials/gui/noicon.png".to_string()]
            .iter()
            .find_map(|file| vfs.read(file, "GAME").ok());
    }
    None
}

/// A page of Chromium, as the game's Lua has pages.
struct CefPage(gmod_web::Page);

fn modifiers(bits: u32) -> Modifiers {
    Modifiers(bits)
}

impl WebPage for CefPage {
    fn open_url(&self, url: &str) {
        self.0.open_url(url);
    }
    fn set_html(&self, html: &str) {
        self.0.set_html(html);
    }
    fn run_js(&self, js: &str) {
        self.0.run_js(js);
    }
    fn add_function(&self, object: &str, function: &str) {
        self.0.add_function(object, function);
    }
    fn answer(&self, callback: u64, values: &str) {
        self.0.answer(callback, values);
    }
    fn is_loading(&self) -> bool {
        self.0.is_loading()
    }
    fn stop(&self) {
        self.0.stop();
    }
    fn reload(&self) {
        self.0.reload();
    }
    fn go_back(&self) {
        self.0.go_back();
    }
    fn go_forward(&self) {
        self.0.go_forward();
    }
    fn resize(&self, width: u32, height: u32) {
        self.0.resize(width, height);
    }
    fn take_picture(&self) -> Option<Rgba> {
        self.0.take_picture().map(|p| Rgba { width: p.width, height: p.height, pixels: p.rgba })
    }
    fn take_events(&self) -> Vec<WebEvent> {
        self.0
            .take_events()
            .into_iter()
            .map(|event| match event {
                PageEvent::Loading { url } => WebEvent::Loading { url },
                PageEvent::Loaded { url } => WebEvent::Loaded { url },
                PageEvent::Title(title) => WebEvent::Title(title),
                PageEvent::Console { message, source, line, level } => WebEvent::Console { message, source, line, level },
                PageEvent::Callback { object, function, callback, args } => WebEvent::Callback { object, function, callback, args },
            })
            .collect()
    }
    fn focus(&self, on: bool) {
        self.0.focus(on);
    }
    fn mouse_move(&self, x: i32, y: i32, held: u32) {
        self.0.mouse_move(x, y, modifiers(held));
    }
    fn mouse_leave(&self) {
        self.0.mouse_leave();
    }
    fn mouse_button(&self, x: i32, y: i32, button: WebButton, down: bool, held: u32) {
        let button = match button {
            WebButton::Left => Button::Left,
            WebButton::Middle => Button::Middle,
            WebButton::Right => Button::Right,
        };
        self.0.mouse_button(x, y, button, down, modifiers(held));
    }
    fn mouse_wheel(&self, x: i32, y: i32, notches: i32, held: u32) {
        self.0.mouse_wheel(x, y, notches, modifiers(held));
    }
    fn key(&self, virtual_key: i32, down: bool, held: u32) {
        self.0.key(virtual_key, down, modifiers(held));
    }
    fn type_text(&self, text: &str, held: u32) {
        self.0.type_text(text, modifiers(held));
    }
}

/// Pages of Chromium for the HTML panels of a realm.
pub fn web_pages(web: Rc<Web>) -> WebPages {
    Rc::new(move |width, height| web.page(width, height).map(|page| Box::new(CefPage(page)) as Box<dyn WebPage>))
}

/// The menu's Lua.
pub struct Menu {
    runtime: Runtime,
    /// The game's files the menu reads.
    vfs: Arc<Vfs>,
    /// The server browser's network side.
    browser: std::cell::RefCell<crate::browser::Browser>,
}

impl Menu {
    /// Starts the menu's Lua on the game's files, showing `gamemode`; its HTML panels are
    /// `pages` when there is Chromium.
    pub fn start(
        vfs: Arc<Vfs>,
        pages: Option<WebPages>,
        gamemode: &str,
        addons: Vec<Addon>,
        games: Vec<MountableGame>,
        no_addons: bool,
    ) -> Result<Menu, String> {
        let runtime = Runtime::new(Realm::Menu, vfs.clone()).map_err(|e| format!("LuaJIT could not be started: {e}"))?;
        runtime.set_output(|output| match output {
            Output::Text(text) => {
                use std::io::Write;
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            Output::Error(error) => eprintln!("{error}"),
        });
        if let Some(dir) = gmod_fs::UserDir::default_root().and_then(|root| gmod_fs::UserDir::open(root, None).ok()) {
            runtime.set_data_dir(dir);
        }
        if let Some(pages) = pages {
            runtime.set_web_pages(pages);
        }
        runtime.set_addons(addons);
        runtime.set_games(games);
        runtime.set_no_addons(no_addons);
        runtime.start(gamemode);
        let browser = crate::browser::Browser::new(gmod_fs::UserDir::default_root().map(|root| root.join("servers")));
        runtime.set_favorite_servers(browser.favorites());
        Ok(Menu { runtime, vfs, browser: std::cell::RefCell::new(browser) })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The game's files the menu reads.
    pub fn vfs(&self) -> Arc<Vfs> {
        self.vfs.clone()
    }

    /// A frame of the menu: its timers and hooks, then its panels painted on a screen of
    /// `width` by `height`.
    pub fn frame(&self, width: u32, height: u32, seconds: f64) {
        self.runtime.set_screen_size(width, height);
        self.runtime.think(seconds);
        self.runtime.paint(seconds);
    }

    /// The server browser asks the network what the menu asked, and hands back what came.
    pub fn browse(&self) {
        self.browser.borrow_mut().poll(&self.runtime);
    }

    /// The game is on a server (its address), or on none any more: the server heads the
    /// history, and the menu may make it a favourite.
    pub fn on_server(&self, address: Option<&str>) {
        if let Some(address) = address {
            self.browser.borrow().played_on(address);
        }
        self.runtime.set_current_server(address.map(str::to_string));
    }

    /// The engine's commands the menu ran: `map`, `gamemode`, `disconnect`, `quit` ...
    pub fn take_commands(&self) -> Vec<String> {
        self.runtime.take_engine_commands()
    }

    /// A console line typed into the terminal while the menu shows; `lua_run_menu` runs Lua in
    /// the menu.
    pub fn console(&self, line: &str) {
        if let Some(code) = line.trim_start().strip_prefix("lua_run_menu ") {
            let _ = self.runtime.run_string(code, "lua_run_menu");
            return;
        }
        if let Err(e) = self.runtime.run_string(&format!("RunConsoleCommand( unpack( string.Explode( ' ', {line:?} ) ) )"), "console") {
            eprintln!("{e}");
        }
    }
}

/// The addons of the installation for the menu, those switched off (`disabled`) not mounted;
/// of the workshop, those Steam says the player subscribed to (`subscribed`).
pub fn addons(install: &Install, disabled: &HashSet<String>, subscribed: Option<&HashSet<u64>>) -> Vec<Addon> {
    workshop::addon_files(install, subscribed)
        .into_iter()
        .filter(|file| file.origin != workshop::Origin::ServerDownload)
        .map(|file| addon(install, &file, disabled))
        .collect()
}

fn addon(install: &Install, file: &AddonFile, disabled: &HashSet<String>) -> Addon {
    // an old compressed download is read where it was unpacked to when it was mounted
    let unpacked = || {
        let root = gmod_fs::UserDir::default_root()?;
        Some(root.join("cache").join("workshop").join(format!("{}.gma", file.workshop_id?)))
    };
    let readable = if file.packed { unpacked() } else { Some(file.path.clone()) };
    let gma = readable.and_then(|path| gmod_fs::gma::GmaArchive::open(&path).ok());
    // one never unpacked has no title here: the menu shows the workshop's
    let stem =
        if file.packed { String::new() } else { file.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() };
    let title = gma.as_ref().map(|g| g.info().title.clone()).filter(|t| !t.is_empty()).unwrap_or(stem);
    let models = gma.as_ref().map_or(0, |g| g.entries().iter().filter(|e| e.path.ends_with(".mdl")).count() as u32);
    let metadata = std::fs::metadata(&file.path).ok();
    let modified = metadata
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    let name = match file.path.strip_prefix(install.game_dir()) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    Addon {
        title,
        file: name,
        workshop_id: if file.origin == workshop::Origin::AddonsFolder { 0 } else { file.workshop_id.unwrap_or(0) },
        mounted: !disabled.contains(&workshop::addon_key(file)),
        size: metadata.map_or(0, |m| m.len()),
        tags: String::new(),
        updated: modified,
        added: modified,
        models,
    }
}

/// What an addon of the menu's list is switched off by ([`workshop::addon_key`]).
pub fn addon_key(addon: &Addon) -> String {
    if addon.workshop_id != 0 { addon.workshop_id.to_string() } else { addon.file.rsplit('/').next().unwrap_or_default().to_lowercase() }
}

/// Where the addons switched off are written down.
pub fn disabled_file() -> Option<PathBuf> {
    gmod_fs::UserDir::default_root().map(|root| root.join("settings").join("addons_disabled.txt"))
}

/// The games GMod can mount, as the menu lists them: those of `cfg/mount.cfg` mounted.
pub fn games(install: &Install) -> Vec<MountableGame> {
    const GAMES: [(u32, &str, &str); 10] = [
        (220, "Half-Life 2", "hl2"),
        (240, "Counter-Strike: Source", "cstrike"),
        (300, "Day of Defeat: Source", "dod"),
        (440, "Team Fortress 2", "tf"),
        (420, "Half-Life 2: Episode Two", "ep2"),
        (380, "Half-Life 2: Episode One", "episodic"),
        (320, "Half-Life 2: Deathmatch", "hl2mp"),
        (340, "Half-Life 2: Lost Coast", "lostcoast"),
        (280, "Half-Life: Source", "hl1"),
        (400, "Portal", "portal"),
    ];
    let mounted: HashSet<String> = std::fs::read(install.game_dir().join("cfg/mount.cfg"))
        .ok()
        .map(|bytes| {
            let (doc, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&bytes), gmod_kv::Options::default());
            gmod_fs::gameinfo::mount_cfg(&doc).into_iter().filter(|(_, dir)| dir.is_dir()).map(|(name, _)| name.to_lowercase()).collect()
        })
        .unwrap_or_default();
    GAMES
        .iter()
        .map(|&(depot, title, folder)| {
            let on = mounted.contains(folder);
            MountableGame { depot, title: title.to_string(), folder: folder.to_string(), owned: on, installed: on, mounted: on }
        })
        .collect()
}
