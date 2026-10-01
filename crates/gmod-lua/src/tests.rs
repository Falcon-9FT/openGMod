//! The runtime on small Lua files of its own; the game's Lua is not in the repository (see
//! `gmod-check lua` for that).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gmod_fs::{Backing, DirMount, Mount, Vfs};
use mlua::Value;

use super::*;

/// A folder that is removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("gmod-lua-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Game {
    runtime: Runtime,
    printed: Rc<RefCell<String>>,
    _dir: TempDir,
}

impl Game {
    fn printed(&self) -> String {
        self.printed.borrow().clone()
    }

    fn eval<T: mlua::FromLuaMulti>(&self, code: &str) -> T {
        self.runtime.lua().load(code).eval().unwrap_or_else(|e| panic!("{code}: {e}"))
    }

    fn messages(&self) -> Vec<String> {
        self.runtime.errors().into_iter().map(|e| e.message).collect()
    }
}

/// A runtime of `realm` on a game folder with these files.
fn game(realm: Realm, files: &[(&str, &str)]) -> Game {
    let dir = TempDir::new();
    for (path, text) in files {
        let path = dir.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut vfs = Vfs::new();
    vfs.push(Mount::new(&["GAME"], Backing::Dir(DirMount::new(&dir.0))));
    let runtime = Runtime::new(realm, Arc::new(vfs)).unwrap();
    let printed = Rc::new(RefCell::new(String::new()));
    let sink = printed.clone();
    runtime.set_output(move |output| match output {
        Output::Text(text) => sink.borrow_mut().push_str(text),
        Output::Error(error) => sink.borrow_mut().push_str(&format!("{error}\n")),
    });
    Game { runtime, printed, _dir: dir }
}

#[test]
fn include_finds_files_next_to_the_caller_first() {
    let g = game(
        Realm::Server,
        &[
            ("lua/includes/init.lua", "include('util.lua') include('shared.lua') print(UTIL, SHARED)"),
            ("lua/includes/util.lua", "UTIL = 'includes/util.lua'"),
            ("lua/util.lua", "UTIL = 'util.lua'"),
            ("lua/shared.lua", "SHARED = 'from the root'"),
        ],
    );
    assert!(g.runtime.init());
    assert_eq!(g.printed(), "includes/util.lua\tfrom the root\n");
    assert_eq!(g.runtime.files_loaded(), 3);
}

#[test]
fn gamemodes_are_in_the_search_path() {
    let g = game(
        Realm::Server,
        &[
            ("gamemodes/sandbox/gamemode/init.lua", "AddCSLuaFile('shared.lua') AddCSLuaFile() include('shared.lua')"),
            ("gamemodes/sandbox/gamemode/shared.lua", "print(debug.getinfo(1, 'S').source)"),
        ],
    );
    assert!(g.runtime.include("sandbox/gamemode/init.lua"));
    assert_eq!(g.printed(), "@gamemodes/sandbox/gamemode/shared.lua\n");
    assert_eq!(g.runtime.sent_files(), ["gamemodes/sandbox/gamemode/shared.lua", "gamemodes/sandbox/gamemode/init.lua"]);
}

#[test]
fn a_failing_file_does_not_stop_the_one_that_included_it() {
    let g = game(
        Realm::Server,
        &[
            ("lua/a.lua", "include('b.lua') include('missing.lua') include('broken.lua') print('a goes on')"),
            ("lua/b.lua", "local function inner() Undefined() end\ninner()"),
            ("lua/broken.lua", "this is not lua"),
        ],
    );
    assert!(g.runtime.include("a.lua"));
    assert!(g.printed().contains("a goes on\n"));
    let errors = g.runtime.errors();
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert_eq!(errors[0].message, "lua/b.lua:1: attempt to call global 'Undefined' (a nil value)");
    assert_eq!(errors[0].stack[0], "inner - lua/b.lua:1");
    assert!(errors[0].stack.iter().any(|f| f == "include - [C]"), "{:?}", errors[0].stack);
    assert!(errors[0].stack.iter().any(|f| f == "unknown - lua/a.lua:1"), "{:?}", errors[0].stack);
    assert_eq!(errors[1].message, "Couldn't include file 'missing.lua' - File not found (lua/a.lua:1)");
    assert!(errors[2].message.starts_with("lua/broken.lua:1:"), "{}", errors[2].message);
    assert!(g.printed().contains("[ERROR] lua/b.lua:1: attempt to call global 'Undefined' (a nil value)\n  1. inner - lua/b.lua:1"));
}

#[test]
fn the_gmod_dialect_compiles() {
    let g = game(
        Realm::Server,
        &[(
            "lua/dialect.lua",
            "// comment\nlocal n = 0\nfor i = 1, 5 do if (i % 2 != 0 && !false) then continue end n = n + i end\nprint(n) /* done */",
        )],
    );
    assert!(g.runtime.include("dialect.lua"));
    assert_eq!(g.printed(), "6\n");
    assert_eq!(g.eval::<i32>("return CompileString('return 1 != 2 && 3', 'x')() and 3 or 0"), 3);
    assert_eq!(g.eval::<i32>("return loadstring('return !nil && 7')()"), 7);
}

#[test]
fn require_runs_a_module_once() {
    let g = game(
        Realm::Server,
        &[
            (
                "lua/includes/modules/counter.lua",
                "COUNTED = (COUNTED or 0) + 1\nmodule('counter', package.seeall)\nfunction Get() return COUNTED end",
            ),
            ("lua/main.lua", "require('counter') require('counter') print(counter.Get(), package.loaded.counter == counter)"),
        ],
    );
    assert!(g.runtime.include("main.lua"));
    assert_eq!(g.printed(), "1\ttrue\n");
    assert!(g.runtime.lua().load("require('nothing')").exec().unwrap_err().to_string().contains("Module not found!"));
}

#[test]
fn no_bytecode_and_no_way_out() {
    let g = game(Realm::Server, &[]);
    let (f, message): (Value, String) = g.eval("return loadstring(string.dump(function() return 1 end))");
    assert!(f.is_nil());
    assert!(message.contains("bytecode is not allowed"), "{message}");
    let (f, _): (Value, String) = g.eval("return load(string.dump(function() end), 'x', 'b')");
    assert!(f.is_nil());
    for gone in ["io", "dofile", "loadfile", "os.execute", "os.exit", "os.remove", "os.getenv", "package.loadlib", "debug.getlocal", "ffi"]
    {
        assert!(g.eval::<bool>(&format!("return {gone} == nil")), "{gone} is still there");
    }
    assert_eq!(g.eval::<i32>("local loaders = package.loaders return #loaders"), 1);
    // the upvalues of the Rust functions behind the API stay out of reach
    let error = g.runtime.lua().load("debug.getupvalue(print, 1)").exec().unwrap_err().to_string();
    assert!(error.contains("not allowed"), "{error}");
    let (name, value): (String, i32) = g.eval("local x = 5 local function f() return x end return debug.getupvalue(f, 1)");
    assert_eq!((name.as_str(), value), ("x", 5));
    assert!(g.runtime.lua().load("os.date('%Q')").exec().is_err());
    assert_eq!(g.eval::<String>("return os.date('!%Y', 0)"), "1970");
}

#[test]
fn printing() {
    let g = game(Realm::Server, &[]);
    g.runtime
        .run_string("print(1, nil, true, 'x') Msg('a', 2) MsgN('b') MsgC(Color and Color(1,2,3) or {r=1,g=2,b=3}, 'c', '\\n')", "printing")
        .unwrap();
    assert_eq!(g.printed(), "1\tnil\ttrue\tx\na2b\nc\n");
    g.runtime.run_string("ErrorNoHalt('soft ', 1)", "x").unwrap();
    assert_eq!(g.messages(), ["soft 1"]);
    let error = g.runtime.run_string("Error('hard ', 2)", "hard").unwrap_err();
    assert_eq!(error.message, "hard 2");
}

#[test]
fn realms() {
    let server = game(Realm::Server, &[]);
    assert!(server.eval::<bool>("return SERVER and not CLIENT and not MENU_DLL and GAME_DLL"));
    let client = game(Realm::Client, &[]);
    assert!(client.eval::<bool>("return CLIENT and not SERVER and not MENU_DLL and CLIENT_DLL"));
    let menu = game(Realm::Menu, &[]);
    assert!(menu.eval::<bool>("return CLIENT and MENU_DLL and not SERVER"));
    assert_eq!(Realm::Menu.init_file(), "includes/init_menu.lua");
}

#[test]
fn enumerations() {
    let g = game(Realm::Server, &[]);
    assert_eq!(g.eval::<i64>("return TEAM_SPECTATOR"), 1002);
    assert_eq!(g.eval::<i64>("return FCVAR_ARCHIVE"), 128);
    assert_eq!(g.eval::<i64>("return SENSORBONE.HIP"), 0);
    assert_eq!(g.eval::<i64>("return TYPE_VECTOR"), 10);
}

#[test]
fn named_metatables() {
    let g = game(Realm::Server, &[]);
    assert!(g.eval::<bool>("return getmetatable(Vector()) == FindMetaTable('Vector')"));
    assert!(g.eval::<bool>("return debug.getmetatable(Angle()) == FindMetaTable('Angle')"));
    assert_eq!(g.eval::<String>("return getmetatable(Vector()).MetaName"), "Vector");
    assert_eq!(g.eval::<i64>("return getmetatable(Angle()).MetaID"), 11);
    // scripts extend the engine's types through their metatables
    assert_eq!(
        g.eval::<f64>(
            "local meta = FindMetaTable('Vector') function meta:Sum() return self.x + self.y + self.z end return Vector(1, 2, 3):Sum()"
        ),
        6.0
    );
    assert!(g.eval::<bool>("local t = {} RegisterMetaTable('Thing', t) return FindMetaTable('Thing') == t"));
    assert!(g.eval::<bool>("return getmetatable('').__index == string"));
}

#[test]
fn vectors() {
    let g = game(Realm::Server, &[]);
    // 32-bit floats, as in GMod
    assert_eq!(g.eval::<f64>("return Vector(0.1, 0, 0).x"), 0.1f32 as f64);
    assert_eq!(g.eval::<String>("return tostring(Vector(1, 2, 3) + Vector(1, 1, 1) * 2)"), "3.000000 4.000000 5.000000");
    assert_eq!(g.eval::<String>("return tostring(2 * Vector(1, 2, 3) / 2 - Vector('1 1 1'))"), "0.000000 1.000000 2.000000");
    assert_eq!(g.eval::<String>("return tostring(-Vector(1, 0, 0))"), "-1.000000 -0.000000 -0.000000");
    assert!(g.eval::<bool>("return Vector(1, 2, 3) == Vector(1, 2, 3) and Vector(1, 2, 3) ~= Vector(1, 2, 4)"));
    assert_eq!(g.eval::<f64>("return Vector(3, 4, 0):Length()"), 5.0);
    assert_eq!(g.eval::<f64>("local v = Vector(1, 2, 3) v.y = 7 v[3] = 9 return v[1] + v.y + v.z"), 17.0);
    assert_eq!(g.eval::<String>("local v = Vector(0, 0, 5) v:Normalize() return tostring(v)"), "0.000000 0.000000 1.000000");
    assert_eq!(g.eval::<String>("return tostring(Vector(1, 0, 0):Cross(Vector(0, 1, 0)))"), "0.000000 0.000000 1.000000");
    assert!(g.eval::<bool>("local a = Vector(0, 1, 0):Angle() return a.p == 0 and a.y == 90 and a.r == 0"));
    assert_eq!(g.eval::<String>("return tostring(Vector(0, 0, 1):Angle())"), "270.000 0.000 0.000");
    let error = g.runtime.lua().load("Vector(1, 2, 3):Dot(5)").set_name("@lua/x.lua").exec().unwrap_err();
    assert!(
        ScriptError::from_lua(&error).message.contains("lua/x.lua:1: bad argument #2 to 'Dot' (Vector expected, got number)"),
        "{error}"
    );
    let error = g.runtime.lua().load("return Vector() + 1").exec().unwrap_err().to_string();
    assert!(error.contains("attempt to perform arithmetic (add) on Vector and number"), "{error}");
}

#[test]
fn angles() {
    let g = game(Realm::Server, &[]);
    let near = |code: &str, expected: [f64; 3]| {
        let (x, y, z): (f64, f64, f64) = g.eval(code);
        for (got, want) in [x, y, z].into_iter().zip(expected) {
            assert!((got - want).abs() < 1e-4, "{code}: {x} {y} {z}");
        }
    };
    near("return Angle(0, 90, 0):Forward():Unpack()", [0.0, 1.0, 0.0]);
    near("return Angle(0, 0, 0):Right():Unpack()", [0.0, -1.0, 0.0]);
    near("return Angle(-90, 0, 0):Forward():Unpack()", [0.0, 0.0, 1.0]);
    near("return Angle(0, 0, 90):Up():Unpack()", [0.0, -1.0, 0.0]);
    near("local a = Angle(0, 0, 0) a:RotateAroundAxis(Vector(0, 0, 1), 90) return a:Unpack()", [0.0, 90.0, 0.0]);
    near("local a = Angle(10, 20, 30) local f, u = a:Forward(), a:Up() return f:AngleEx(u):Unpack()", [10.0, 20.0, 30.0]);
    near("local v = Vector(1, 0, 0) v:Rotate(Angle(0, 90, 0)) return v:Unpack()", [0.0, 1.0, 0.0]);
    near("local a = Angle(190, -270, 720) a:Normalize() return a:Unpack()", [-170.0, 90.0, 0.0]);
    assert_eq!(g.eval::<String>("return tostring(Angle(1, 2, 3) * 2)"), "2.000 4.000 6.000");
    assert_eq!(g.eval::<f64>("local a = Angle(1, 2, 3) return a.p + a.yaw + a[3]"), 6.0);
}
