//! Web pages, as Garry's Mod shows them: its HTML panels (`DHTML`), the main menu, MOTDs.
//!
//! Chromium (CEF) draws each [`Page`] off screen: the program asks for its newest picture
//! ([`Page::take_picture`]), RGBA, and draws it where the page's panel is; it hands the page
//! the mouse and the keys. Pages tell what happens - loading, the title, console messages -
//! as [`PageEvent`]s.
//!
//! Lua and pages talk as GMod has them. JavaScript runs in a page ([`Page::run_js`]). Lua
//! gives a page objects with functions ([`Page::add_function`]: `lua.Run`, `lua.PlaySound`
//! of the main menu): a call of one in the page comes back as [`PageEvent::Callback`] with its
//! arguments as JSON. Such a function goes through the page's console, as a message that only
//! the bridge writes; the bridge is in the page before its own scripts when the page comes
//! from `asset://` or [`Page::set_html`], and as it finishes loading otherwise.
//!
//! `asset://<host>/<path>` serves the game's files, as the program finds them
//! ([`Files`]): `asset://garrysmod/html/menu.html` is the main menu.
//!
//! CEF runs pages in processes of their own, started from the program itself: [`subprocess`]
//! must be the first thing the program does. In the program, Chromium has a thread of its own
//! for its work (its "UI" thread): the program's window and its messages are left alone, which
//! on Windows a Chromium pumped from inside the window's own messages does not do. What
//! Chromium tells comes on that thread and waits behind a lock until the program takes it.

// The wrappers CEF's crate makes for handlers are unsafe inside; this crate's own unsafe is in
// two places, each with why it is sound: reading a picture and writing a file's bytes.
#![deny(unsafe_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cef::rc::Rc as _;
use cef::{
    App, Browser, BrowserSettings, CefString, Client, CommandLine, DisplayHandler, ImplApp, ImplBrowser, ImplBrowserHost, ImplClient,
    ImplCommandLine, ImplDisplayHandler, ImplFrame, ImplLifeSpanHandler, ImplLoadHandler, ImplRenderHandler, ImplRequest,
    ImplResourceHandler, ImplResponse, ImplSchemeHandlerFactory, ImplSchemeRegistrar, ImplTask, KeyEvent, KeyEventType, LifeSpanHandler,
    LoadHandler, LogSeverity, MouseButtonType, MouseEvent, PaintElementType, Rect, RenderHandler, Request, ResourceHandler,
    ResourceReadCallback, Response, SchemeHandlerFactory, SchemeOptions, SchemeRegistrar, ScreenInfo, Settings, Task, ThreadId, WindowInfo,
    WrapApp, WrapClient, WrapDisplayHandler, WrapLifeSpanHandler, WrapLoadHandler, WrapRenderHandler, WrapResourceHandler,
    WrapSchemeHandlerFactory, WrapTask,
};

/// What `asset://` serves: the bytes of a file, by the URL's host and its path.
pub type Files = Arc<dyn Fn(&str, &str) -> Option<Vec<u8>> + Send + Sync>;

/// What a console message starts with when the bridge writes it: a call of a function Lua
/// gave the page.
const BRIDGE: &str = "\u{1}opengmod\u{1}";

/// A picture of a page: RGBA, `width` by `height`, its colours as they are to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What a page tells.
#[derive(Debug, Clone, PartialEq)]
pub enum PageEvent {
    /// It began loading a document.
    Loading {
        url: String,
    },
    /// It loaded one: its scripts ran, it can be talked to.
    Loaded {
        url: String,
    },
    Title(String),
    /// `console.log` and the like, and errors: `level` is `debug`, `info`, `warn` or `error`.
    Console {
        message: String,
        source: String,
        line: i32,
        level: &'static str,
    },
    /// A function Lua gave it was called: `object.function(...)`, the arguments as a JSON
    /// array. With a `callback`, the page waits for what Lua returns ([`Page::answer`]).
    Callback {
        object: String,
        function: String,
        callback: Option<u64>,
        args: String,
    },
}

/// What a page and the program share.
#[derive(Default)]
struct State {
    size: (u32, u32),
    picture: Option<Picture>,
    events: Vec<PageEvent>,
    loading: bool,
    /// A document was asked for and has not begun loading yet: the page counts as loading,
    /// so that what waits for it to load (`DHTML`'s queue of JavaScript) waits.
    requested: bool,
    /// The objects Lua gave the page, and their functions.
    objects: Vec<(String, Vec<String>)>,
}

type Shared = Arc<Mutex<State>>;

/// The pages, by the number of their browser: for `asset://` to know whose bridge to put in.
fn pages() -> &'static Mutex<HashMap<i32, Shared>> {
    static PAGES: OnceLock<Mutex<HashMap<i32, Shared>>> = OnceLock::new();
    PAGES.get_or_init(Default::default)
}

/// The documents of [`Page::set_html`], by the path `asset://html/` serves them at.
fn documents() -> &'static Mutex<HashMap<String, String>> {
    static DOCUMENTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    DOCUMENTS.get_or_init(Default::default)
}

/// Browsers made and not closed yet: Chromium may only be shut down without any.
static OPEN: AtomicUsize = AtomicUsize::new(0);

fn files() -> &'static OnceLock<Files> {
    static FILES: OnceLock<Files> = OnceLock::new();
    &FILES
}

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, State> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// A JavaScript string of `s`.
fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The bridge, and the objects Lua gave: the page's console as it was, kept for the bridge
/// alone - Lua may give the page a `console.log` of its own, as `DHTML` does.
fn bridge(objects: &[(String, Vec<String>)]) -> String {
    // a function last among the arguments is called back with what Lua returned
    let mut js = format!(
        "(function(){{if(!window.__opengmod){{var log=console.log.bind(console),waiting={{}},next=0;\
         window.__opengmod=function(o,f,a){{var n=a.length,id=0,args=[];\
         if(n>0&&typeof a[n-1]==='function'){{id=++next;waiting[id]=a[n-1];n--;}}\
         for(var i=0;i<n;i++){{var v=a[i];args.push(typeof v==='function'||v===undefined?null:v);}}\
         log({}+JSON.stringify([o,f,id,args]));}};\
         window.__opengmod_answer=function(id,values){{var f=waiting[id];delete waiting[id];if(f)f.apply(null,values);}};}}",
        js_string(BRIDGE)
    );
    for (object, functions) in objects {
        let o = js_string(object);
        js.push_str(&format!("window[{o}]=window[{o}]||{{}};"));
        for function in functions {
            let f = js_string(function);
            js.push_str(&format!("window[{o}][{f}]=function(){{window.__opengmod({o},{f},arguments);}};"));
        }
    }
    js.push_str("})();");
    js
}

/// An HTML document with the bridge first in it: after `<head>`, else at the start (after
/// the doctype, so as not to change how it is laid out).
fn with_bridge(html: &str, objects: &[(String, Vec<String>)]) -> String {
    let script = format!("<script>{}</script>", bridge(objects));
    let lower = html.to_ascii_lowercase();
    let at = match lower.find("<head") {
        Some(head) => lower[head..].find('>').map_or(0, |end| head + end + 1),
        None if lower.trim_start().starts_with("<!doctype") => lower.find('>').map_or(0, |end| end + 1),
        None => 0,
    };
    format!("{}{script}{}", &html[..at], &html[at..])
}

fn mime(path: &str) -> &'static str {
    let extension = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" => "text/html",
        "js" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ttf" => "font/ttf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

// --- the process ------------------------------------------------------------------------

cef::wrap_app! {
    struct WebApp {}

    impl App {
        fn on_before_command_line_processing(&self, _process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            let Some(command_line) = command_line else { return };
            // pages are pictures: drawn by the processor, as dependable as it gets
            for switch in ["disable-gpu", "disable-gpu-compositing", "no-sandbox", "noerrdialogs", "hide-crash-restore-bubble"] {
                command_line.append_switch(Some(&switch.into()));
            }
            command_line.append_switch_with_value(Some(&"autoplay-policy".into()), Some(&"no-user-gesture-required".into()));
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            let Some(registrar) = registrar else { return };
            let options = SchemeOptions::STANDARD.get_raw()
                | SchemeOptions::SECURE.get_raw()
                | SchemeOptions::CORS_ENABLED.get_raw()
                | SchemeOptions::FETCH_ENABLED.get_raw();
            registrar.add_custom_scheme(Some(&"asset".into()), options as _);
        }
    }
}

/// What the program must do first: when it was started as one of CEF's processes (a page's,
/// the GPU's ...), that process runs here and its exit code comes back, to exit with.
/// `None`: this is the program itself.
pub fn subprocess() -> Option<i32> {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let browser = args.as_cmd_line().is_none_or(|cmd| cmd.has_switch(Some(&"type".into())) != 1);
    let mut app = WebApp::new();
    let code = cef::execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if browser { None } else { Some(code) }
}

/// How the browser names itself: Chrome's, and what GMod's pages look for to know they are
/// in the game (`Valve Source Client`).
fn user_agent_product() -> String {
    use cef::sys::{CHROME_VERSION_BUILD, CHROME_VERSION_MAJOR, CHROME_VERSION_MINOR, CHROME_VERSION_PATCH};
    format!(
        "Chrome/{CHROME_VERSION_MAJOR}.{CHROME_VERSION_MINOR}.{CHROME_VERSION_BUILD}.{CHROME_VERSION_PATCH} Safari/537.36 GMod/openGMOD Valve Source Client"
    )
}

/// Chromium, started.
pub struct Web {
    /// A cache of this process's own, removed when Chromium is done.
    own_cache: Option<std::path::PathBuf>,
}

impl Web {
    /// Starts Chromium: its cache in `cache` (which must not be in the game's folder),
    /// `asset://` from `files`. Once a program.
    /// Chromium's own log goes to `log`.
    pub fn start(cache: &Path, log: &Path, files_from: Files) -> Result<Web, String> {
        let _ = files().set(files_from);
        // a cache another Chromium holds (another openGMOD, or what is left of one that hung)
        // cannot be shared: this one gets a folder of its own beside it
        let own_cache = cache_in_use(cache).then(|| cache.join(format!("process-{}", std::process::id())));
        if let Some(own) = &own_cache {
            println!("Chromium's cache is in use by another program: {} is used", own.display());
        }
        let cache = own_cache.as_deref().unwrap_or(cache);
        let args = cef::args::Args::new();
        let settings = Settings {
            windowless_rendering_enabled: 1,
            multi_threaded_message_loop: 1,
            no_sandbox: 1,
            root_cache_path: CefString::from(cache.to_string_lossy().as_ref()),
            cache_path: CefString::from(cache.join("default").to_string_lossy().as_ref()),
            log_severity: LogSeverity::WARNING,
            log_file: CefString::from(log.to_string_lossy().as_ref()),
            background_color: 0,
            // what GMod's pages look for to know they are in the game
            user_agent_product: CefString::from(user_agent_product().as_str()),
            ..Default::default()
        };
        let mut app = WebApp::new();
        if cef::initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
            return Err("Chromium could not be started".to_string());
        }
        let mut factory = AssetFactory::new();
        cef::register_scheme_handler_factory(Some(&"asset".into()), None, Some(&mut factory));
        Ok(Web { own_cache })
    }

    /// A new page, `width` by `height` pixels, empty. Made on Chromium's thread, as it must
    /// be; none when it is not made within a few seconds.
    pub fn page(&self, width: u32, height: u32) -> Option<Page> {
        let shared: Shared = Arc::new(Mutex::new(State { size: (width.max(1), height.max(1)), ..State::default() }));
        let (send, receive) = sync_channel(1);
        let mut task = MakePage::new(Arc::new(Mutex::new(Some((shared.clone(), send)))));
        if cef::post_task(ThreadId::UI, Some(&mut task)) != 1 {
            return None;
        }
        let browser = receive.recv_timeout(Duration::from_secs(10)).ok()??;
        Some(Page { browser, shared })
    }
}

impl Drop for Web {
    fn drop(&mut self) {
        // the pages dropped before close as Chromium gets to it: a few seconds at most
        let started = std::time::Instant::now();
        while OPEN.load(Ordering::SeqCst) > 0 && started.elapsed().as_secs() < 3 {
            std::thread::sleep(Duration::from_millis(5));
        }
        cef::shutdown();
        if let Some(dir) = &self.own_cache {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Whether a Chromium is using the cache in `dir`: it holds `lockfile` open on Windows, and has
/// `SingletonLock` point to its process elsewhere.
fn cache_in_use(dir: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let lock = dir.join("lockfile");
        lock.exists() && std::fs::OpenOptions::new().write(true).share_mode(0).open(&lock).is_err()
    }
    #[cfg(not(windows))]
    {
        // `<host>-<pid>`, the process that holds it
        let Ok(target) = std::fs::read_link(dir.join("SingletonLock")) else { return false };
        let pid = target.to_string_lossy().rsplit('-').next().and_then(|p| p.parse::<u32>().ok());
        pid.is_some_and(|pid| pid != std::process::id() && Path::new(&format!("/proc/{pid}")).exists())
    }
}

/// Makes a page on Chromium's thread and sends it back.
type PageOrder = Arc<Mutex<Option<(Shared, SyncSender<Option<Browser>>)>>>;

cef::wrap_task! {
    struct MakePage {
        order: PageOrder,
    }

    impl Task {
        fn execute(&self) {
            let Some((shared, send)) = self.order.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
            let window = WindowInfo { windowless_rendering_enabled: 1, ..Default::default() };
            let settings = BrowserSettings { windowless_frame_rate: 60, background_color: 0, ..Default::default() };
            let mut client = PageClient::new(
                PageRender::new(shared.clone()),
                PageDisplay::new(shared.clone()),
                PageLoad::new(shared.clone()),
                PageLife::new(),
            );
            let browser =
                cef::browser_host_create_browser_sync(Some(&window), Some(&mut client), Some(&"".into()), Some(&settings), None, None);
            if let Some(browser) = &browser {
                OPEN.fetch_add(1, Ordering::SeqCst);
                pages().lock().unwrap_or_else(|e| e.into_inner()).insert(browser.identifier(), shared);
            }
            let _ = send.send(browser);
        }
    }
}

// --- a page ----------------------------------------------------------------------------

/// A page: one browser, drawn off screen.
pub struct Page {
    browser: Browser,
    shared: Shared,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Page {
    fn frame(&self) -> Option<cef::Frame> {
        self.browser.main_frame()
    }

    pub fn open_url(&self, url: &str) {
        if let Some(frame) = self.frame() {
            lock(&self.shared).requested = true;
            frame.load_url(Some(&url.into()));
        }
    }

    /// Shows a document given whole, served from `asset://html/` so that it can use the
    /// game's files beside it.
    pub fn set_html(&self, html: &str) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let name = format!("page{}.html", NEXT.fetch_add(1, Ordering::Relaxed));
        documents().lock().unwrap_or_else(|e| e.into_inner()).insert(name.clone(), html.to_string());
        self.open_url(&format!("asset://html/{name}"));
    }

    pub fn run_js(&self, js: &str) {
        if let Some(frame) = self.frame() {
            frame.execute_java_script(Some(&js.into()), Some(&"".into()), 0);
        }
    }

    /// Gives the page `object.function`: calls of it come back as [`PageEvent::Callback`].
    pub fn add_function(&self, object: &str, function: &str) {
        let objects = {
            let mut state = lock(&self.shared);
            match state.objects.iter_mut().find(|(o, _)| o == object) {
                Some((_, functions)) if functions.iter().any(|f| f == function) => return,
                Some((_, functions)) => functions.push(function.to_string()),
                None => state.objects.push((object.to_string(), vec![function.to_string()])),
            }
            state.objects.clone()
        };
        // into the document already there, too
        self.run_js(&bridge(&objects));
    }

    pub fn is_loading(&self) -> bool {
        let state = lock(&self.shared);
        state.loading || state.requested
    }

    pub fn stop(&self) {
        self.browser.stop_load();
    }

    pub fn reload(&self) {
        self.browser.reload();
    }

    pub fn go_back(&self) {
        self.browser.go_back();
    }

    pub fn go_forward(&self) {
        self.browser.go_forward();
    }

    /// The page is this size now.
    pub fn resize(&self, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        let changed = {
            let mut state = lock(&self.shared);
            std::mem::replace(&mut state.size, size) != size
        };
        if changed && let Some(host) = self.browser.host() {
            host.was_resized();
        }
    }

    /// The page's newest picture, if it drew one since the last call.
    pub fn take_picture(&self) -> Option<Picture> {
        lock(&self.shared).picture.take()
    }

    /// What the page told since the last call, oldest first.
    pub fn take_events(&self) -> Vec<PageEvent> {
        std::mem::take(&mut lock(&self.shared).events)
    }

    pub fn focus(&self, on: bool) {
        if let Some(host) = self.browser.host() {
            host.set_focus(i32::from(on));
        }
    }

    /// The cursor is at `x`, `y` of the page.
    pub fn mouse_move(&self, x: i32, y: i32, modifiers: Modifiers) {
        if let Some(host) = self.browser.host() {
            host.send_mouse_move_event(Some(&MouseEvent { x, y, modifiers: modifiers.0 }), 0);
        }
    }

    /// The mouse left the page.
    pub fn mouse_leave(&self) {
        if let Some(host) = self.browser.host() {
            host.send_mouse_move_event(Some(&MouseEvent { x: -1, y: -1, modifiers: 0 }), 1);
        }
    }

    pub fn mouse_button(&self, x: i32, y: i32, button: Button, down: bool, modifiers: Modifiers) {
        let kind = match button {
            Button::Left => MouseButtonType::LEFT,
            Button::Middle => MouseButtonType::MIDDLE,
            Button::Right => MouseButtonType::RIGHT,
        };
        if let Some(host) = self.browser.host() {
            host.send_mouse_click_event(Some(&MouseEvent { x, y, modifiers: modifiers.0 }), kind, i32::from(!down), 1);
        }
    }

    /// The wheel turned by `dy` notches (up positive).
    pub fn mouse_wheel(&self, x: i32, y: i32, dy: i32, modifiers: Modifiers) {
        if let Some(host) = self.browser.host() {
            host.send_mouse_wheel_event(Some(&MouseEvent { x, y, modifiers: modifiers.0 }), 0, dy * 120);
        }
    }

    /// A key, by its Windows virtual-key code, went down or up.
    pub fn key(&self, virtual_key: i32, down: bool, modifiers: Modifiers) {
        if let Some(host) = self.browser.host() {
            let event = KeyEvent {
                size: std::mem::size_of::<cef::sys::cef_key_event_t>(),
                type_: if down { KeyEventType::RAWKEYDOWN } else { KeyEventType::KEYUP },
                modifiers: modifiers.0,
                windows_key_code: virtual_key,
                native_key_code: virtual_key,
                ..Default::default()
            };
            host.send_key_event(Some(&event));
        }
    }

    /// Text typed.
    pub fn type_text(&self, text: &str, modifiers: Modifiers) {
        let Some(host) = self.browser.host() else { return };
        for unit in text.encode_utf16() {
            let event = KeyEvent {
                size: std::mem::size_of::<cef::sys::cef_key_event_t>(),
                type_: KeyEventType::CHAR,
                modifiers: modifiers.0,
                windows_key_code: i32::from(unit),
                character: unit,
                unmodified_character: unit,
                ..Default::default()
            };
            host.send_key_event(Some(&event));
        }
    }

    /// What Lua returned for a [`PageEvent::Callback`] that waits: its values as a JSON array.
    pub fn answer(&self, callback: u64, values: &str) {
        self.run_js(&format!("window.__opengmod_answer&&window.__opengmod_answer({callback},{values});"));
    }
}

/// Keys and buttons held while the mouse or a key does something.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers(pub u32);

impl Modifiers {
    pub const SHIFT: Modifiers = Modifiers(1 << 1);
    pub const CONTROL: Modifiers = Modifiers(1 << 2);
    pub const ALT: Modifiers = Modifiers(1 << 3);
    pub const LEFT_BUTTON: Modifiers = Modifiers(1 << 4);
    pub const MIDDLE_BUTTON: Modifiers = Modifiers(1 << 5);
    pub const RIGHT_BUTTON: Modifiers = Modifiers(1 << 6);
}

impl std::ops::BitOr for Modifiers {
    type Output = Modifiers;
    fn bitor(self, other: Modifiers) -> Modifiers {
        Modifiers(self.0 | other.0)
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        pages().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.browser.identifier());
        if let Some(host) = self.browser.host() {
            host.close_browser(1);
        }
    }
}

// --- what CEF calls --------------------------------------------------------------------

cef::wrap_client! {
    struct PageClient {
        render: RenderHandler,
        display: DisplayHandler,
        load: LoadHandler,
        life: LifeSpanHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life.clone())
        }
    }
}

cef::wrap_life_span_handler! {
    struct PageLife {}

    impl LifeSpanHandler {
        // no windows of their own: a page that opens one opens nothing
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            _popup_id: ::std::os::raw::c_int,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: cef::WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&cef::PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<cef::DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            1
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            OPEN.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

cef::wrap_render_handler! {
    struct PageRender {
        shared: Shared,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let (w, h) = lock(&self.shared).size;
                rect.width = w as i32;
                rect.height = h as i32;
            }
        }

        fn screen_info(&self, _browser: Option<&mut Browser>, screen_info: Option<&mut ScreenInfo>) -> ::std::os::raw::c_int {
            match screen_info {
                Some(info) => {
                    info.device_scale_factor = 1.0;
                    1
                }
                None => 0,
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let length = width as usize * height as usize * 4;
            // SAFETY: CEF hands `on_paint` a buffer of `width * height` BGRA pixels, valid
            // until it returns; it is read here and nowhere after.
            #[allow(unsafe_code)]
            let bgra = unsafe { std::slice::from_raw_parts(buffer, length) };
            // BGRA with the colour already times its alpha: RGBA as it is drawn
            let mut rgba = Vec::with_capacity(length);
            for p in bgra.as_chunks::<4>().0 {
                let a = p[3];
                let unmultiply = |c: u8| if a == 0 { 0 } else { ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8 };
                rgba.extend([unmultiply(p[2]), unmultiply(p[1]), unmultiply(p[0]), a]);
            }
            lock(&self.shared).picture = Some(Picture { width: width as u32, height: height as u32, rgba });
        }
    }
}

cef::wrap_display_handler! {
    struct PageDisplay {
        shared: Shared,
    }

    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            let title = title.map(ToString::to_string).unwrap_or_default();
            lock(&self.shared).events.push(PageEvent::Title(title));
        }

        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            level: LogSeverity,
            message: Option<&CefString>,
            source: Option<&CefString>,
            line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let message = message.map(ToString::to_string).unwrap_or_default();
            let event = match message.strip_prefix(BRIDGE).and_then(parse_call) {
                Some((object, function, callback, args)) => PageEvent::Callback { object, function, callback, args },
                None => PageEvent::Console {
                    message,
                    source: source.map(ToString::to_string).unwrap_or_default(),
                    line,
                    level: match level {
                        l if l == LogSeverity::ERROR || l == LogSeverity::FATAL => "error",
                        l if l == LogSeverity::WARNING => "warn",
                        l if l == LogSeverity::VERBOSE => "debug",
                        _ => "info",
                    },
                },
            };
            lock(&self.shared).events.push(event);
            // told here: Chromium need not log it again
            1
        }
    }
}

/// A call as the bridge writes it: `["object","function",callback,[args...]]`; the arguments
/// stay JSON.
fn parse_call(json: &str) -> Option<(String, String, Option<u64>, String)> {
    let json = json.trim();
    let inner = json.strip_prefix('[')?.strip_suffix(']')?;
    let (object, rest) = json_string(inner)?;
    let rest = rest.trim_start().strip_prefix(',')?;
    let (function, rest) = json_string(rest)?;
    let rest = rest.trim_start().strip_prefix(',')?.trim_start();
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let callback: u64 = rest[..digits].parse().ok()?;
    let args = rest[digits..].trim_start().strip_prefix(',')?.trim();
    Some((object, function, (callback != 0).then_some(callback), args.to_string()))
}

/// A JSON string at the start of `s`, and what follows it.
fn json_string(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = (0..4).filter_map(|_| chars.next().map(|(_, c)| c)).collect();
                    out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'));
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

cef::wrap_load_handler! {
    struct PageLoad {
        shared: Shared,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            _can_go_back: ::std::os::raw::c_int,
            _can_go_forward: ::std::os::raw::c_int,
        ) {
            lock(&self.shared).loading = is_loading != 0;
        }

        fn on_load_start(&self, _browser: Option<&mut Browser>, frame: Option<&mut cef::Frame>, _transition_type: cef::TransitionType) {
            let Some(frame) = frame.filter(|f| f.is_main() == 1) else { return };
            let url = CefString::from(&frame.url()).to_string();
            let mut state = lock(&self.shared);
            state.requested = false;
            state.events.push(PageEvent::Loading { url });
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut cef::Frame>,
            error_code: cef::Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            // what was asked for will not load: the page tells it as an error of its console
            if frame.is_some_and(|f| f.is_main() == 1) {
                let url = failed_url.map(ToString::to_string).unwrap_or_default();
                let text = error_text.map(ToString::to_string).unwrap_or_default();
                let mut state = lock(&self.shared);
                state.requested = false;
                state.events.push(PageEvent::Console {
                    message: format!("could not load {url}: {text} ({})", *error_code.as_ref() as i32),
                    source: url,
                    line: 0,
                    level: "error",
                });
            }
        }

        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut cef::Frame>, _http_status_code: ::std::os::raw::c_int) {
            let Some(frame) = frame.filter(|f| f.is_main() == 1) else { return };
            let url = CefString::from(&frame.url()).to_string();
            // pages from elsewhere get the bridge now
            let objects = lock(&self.shared).objects.clone();
            if !url.starts_with("asset://") {
                frame.execute_java_script(Some(&bridge(&objects).as_str().into()), Some(&"".into()), 0);
            }
            lock(&self.shared).events.push(PageEvent::Loaded { url });
        }
    }
}

// --- asset:// ---------------------------------------------------------------------------

cef::wrap_scheme_handler_factory! {
    struct AssetFactory {}

    impl SchemeHandlerFactory {
        fn create(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let url = CefString::from(&request?.url()).to_string();
            let rest = url.strip_prefix("asset://")?;
            let rest = rest.split(['?', '#']).next().unwrap_or_default();
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let path = percent_decode(path);
            let objects = browser
                .and_then(|b| pages().lock().unwrap_or_else(|e| e.into_inner()).get(&b.identifier()).cloned())
                .map(|shared| lock(&shared).objects.clone())
                .unwrap_or_default();
            let body = if host.eq_ignore_ascii_case("html") {
                documents().lock().unwrap_or_else(|e| e.into_inner()).get(&path).map(|html| html.clone().into_bytes())
            } else {
                files().get().and_then(|files| files(host, &path))
            };
            let mime = if host.eq_ignore_ascii_case("html") { "text/html" } else { mime(&path) };
            let body = match body {
                Some(bytes) if mime == "text/html" => Some(with_bridge(&String::from_utf8_lossy(&bytes), &objects).into_bytes()),
                other => other,
            };
            Some(AssetHandler::new(Arc::new(body), mime, Arc::new(Mutex::new(0))))
        }
    }
}

/// `%20` and the like.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

cef::wrap_resource_handler! {
    struct AssetHandler {
        body: Arc<Option<Vec<u8>>>,
        mime: &'static str,
        offset: Arc<Mutex<usize>>,
    }

    impl ResourceHandler {
        fn open(&self, _request: Option<&mut Request>, handle_request: Option<&mut ::std::os::raw::c_int>, _callback: Option<&mut cef::Callback>) -> ::std::os::raw::c_int {
            if let Some(handle) = handle_request {
                *handle = 1;
            }
            1
        }

        fn response_headers(&self, response: Option<&mut Response>, response_length: Option<&mut i64>, _redirect_url: Option<&mut CefString>) {
            let Some(response) = response else { return };
            match self.body.as_ref() {
                Some(body) => {
                    response.set_status(200);
                    response.set_mime_type(Some(&self.mime.into()));
                    if let Some(length) = response_length {
                        *length = body.len() as i64;
                    }
                }
                None => {
                    response.set_status(404);
                    if let Some(length) = response_length {
                        *length = 0;
                    }
                }
            }
            response.set_header_by_name(Some(&"Access-Control-Allow-Origin".into()), Some(&"*".into()), 1);
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(body) = self.body.as_ref() else { return 0 };
            let mut offset = self.offset.lock().unwrap_or_else(|e| e.into_inner());
            let n = body.len().saturating_sub(*offset).min(bytes_to_read.max(0) as usize);
            if n == 0 || data_out.is_null() {
                return 0;
            }
            // SAFETY: CEF gives `read` a buffer of at least `bytes_to_read` bytes to fill; `n`
            // is no more than that.
            #[allow(unsafe_code)]
            let out = unsafe { std::slice::from_raw_parts_mut(data_out, n) };
            out.copy_from_slice(&body[*offset..*offset + n]);
            *offset += n;
            if let Some(read) = bytes_read {
                *read = n as i32;
            }
            1
        }
    }
}

#[cfg(test)]
mod tests;
