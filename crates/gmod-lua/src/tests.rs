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
    let files: Vec<(&str, &[u8])> = files.iter().map(|(path, text)| (*path, text.as_bytes())).collect();
    game_with(realm, &files)
}

fn game_with(realm: Realm, files: &[(&str, &[u8])]) -> Game {
    let dir = TempDir::new();
    for (path, bytes) in files {
        let path = dir.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
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

#[test]
fn matrices() {
    let g = game(Realm::Server, &[]);
    let near = |code: &str, expected: &[f64]| {
        let got: Vec<f64> = g.eval::<mlua::Variadic<f64>>(code).to_vec();
        assert_eq!(got.len(), expected.len(), "{code}: {got:?}");
        for (got_one, want) in got.iter().zip(expected) {
            assert!((got_one - want).abs() < 1e-4, "{code}: {got:?}");
        }
    };
    assert!(g.eval::<bool>("local m = Matrix() return m:IsIdentity() and m:IsRotationMatrix() and not m:IsZero()"));
    assert!(g.eval::<bool>("return getmetatable(Matrix()) == FindMetaTable('VMatrix') and getmetatable(Matrix()).MetaID == TYPE_MATRIX"));
    near(
        "return Matrix({{1, 2, 3, 4}, {5, 6, 7, 8}}):Unpack()",
        &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    );
    near("local m = Matrix() m:SetField(2, 4, 7) return m:GetField(2, 4), m:GetTranslation():Unpack()", &[7.0, 0.0, 7.0, 0.0]);
    // the axes of an angle; right is the second column negated
    near("local m = Matrix() m:SetAngles(Angle(0, 90, 0)) return m:GetForward():Unpack()", &[0.0, 1.0, 0.0]);
    near("local m = Matrix() m:SetAngles(Angle(0, 90, 0)) return m:GetRight():Unpack()", &[1.0, 0.0, 0.0]);
    near("local m = Matrix() m:SetAngles(Angle(0, 90, 0)) return m:GetField(1, 2), m:GetField(2, 2)", &[-1.0, 0.0]);
    near("local m = Matrix() m:SetAngles(Angle(10, 20, 30)) return m:GetAngles():Unpack()", &[10.0, 20.0, 30.0]);
    near("local m = Matrix() m:SetAngles(Angle(-90, 45, 0)) return m:GetAngles():Unpack()", &[-90.0, 45.0, 0.0]);
    // a point is turned, then moved; methods multiply on the right
    near(
        "local m = Matrix() m:SetTranslation(Vector(10, 0, 0)) m:Rotate(Angle(0, 90, 0)) return (m * Vector(1, 0, 0)):Unpack()",
        &[10.0, 1.0, 0.0],
    );
    near(
        "local m = Matrix() m:Translate(Vector(1, 2, 3)) m:Translate(Vector(1, 1, 1)) return m:GetTranslation():Unpack()",
        &[2.0, 3.0, 4.0],
    );
    near("local m = Matrix() m:Scale(Vector(2, 3, 4)) return (m * Vector(1, 1, 1)):Unpack()", &[2.0, 3.0, 4.0]);
    near("local m = Matrix() m:SetAngles(Angle(0, 30, 0)) m:SetScale(Vector(2, 2, 2)) return m:GetScale():Unpack()", &[2.0, 2.0, 2.0]);
    near("local m = Matrix() m:SetAngles(Angle(0, 30, 0)) m:SetScale(Vector(2, 2, 2)) return m:GetAngles():Unpack()", &[0.0, 30.0, 0.0]);
    near("local m = Matrix() m:SetTranslation(Vector(1, 2, 3)) m:ScaleTranslation(2) return m:GetTranslation():Unpack()", &[2.0, 4.0, 6.0]);
    // inverses
    near(
        "local m = Matrix() m:SetAngles(Angle(10, 20, 30)) m:SetTranslation(Vector(5, 6, 7)) m:Scale(Vector(2, 1, 3))
         local i = m:GetInverse() return (i * (m * Vector(1, 2, 3))):Unpack()",
        &[1.0, 2.0, 3.0],
    );
    near(
        "local m = Matrix() m:SetAngles(Angle(10, 20, 30)) m:SetTranslation(Vector(5, 6, 7))
         local i = Matrix() i:Set(m) i:InvertTR() return (i * m):Unpack()",
        &[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    );
    assert!(
        g.eval::<bool>("local m = Matrix() m:Zero() return m:IsZero() and m:GetInverse() == nil and m:Invert() == false and m:IsZero()")
    );
    assert!(g.eval::<bool>(
        "local m = Matrix() m:Scale(Vector(2, 2, 2)) return m:Invert() and m:GetField(1, 1) == 0.5 and not m:IsRotationMatrix()"
    ));
    // arithmetic and conversions
    near("return (Matrix() + Matrix() - Matrix()):GetField(1, 1), (-Matrix()):GetField(2, 2)", &[1.0, -1.0]);
    near("local m = Matrix() m:Add(Matrix()) m:Sub(Matrix({{0, 0, 0, 1}})) return m:GetField(1, 1), m:GetField(1, 4)", &[2.0, -1.0]);
    near("local m = Matrix({{1, 2}, {3, 4}}) return m:GetTransposed():GetField(1, 2), m:ToTable()[2][1]", &[3.0, 3.0]);
    near(
        "local m = Matrix() m:SetUnpacked(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16) return Matrix(m:ToTable()):GetField(4, 3)",
        &[15.0],
    );
    assert!(g.eval::<bool>("return Matrix() == Matrix() and Matrix() ~= Matrix({{2}})"));
    assert!(g.eval::<String>("return tostring(Matrix())").starts_with("[1.000000,\t0.000000,\t0.000000,\t0.000000]\n"));
    let error = g.runtime.lua().load("Matrix():GetField(5, 1)").exec().unwrap_err().to_string();
    assert!(error.contains("bad argument #2 to 'GetField'"), "{error}");

    // over the network, sixteen floats
    let server = game(Realm::Server, &[]);
    server
        .runtime
        .run_string("util.AddNetworkString('m') net.Start('m') net.WriteMatrix(Matrix({{1, 2, 3, 4}})) net.Broadcast()", "send")
        .unwrap();
    let message = server.runtime.take_net_messages().remove(0);
    assert_eq!(message.data.len, 16 * 32);
    let client = game(Realm::Client, &[]);
    client.runtime.run_string("function net.Incoming() net.ReadHeader() print(net.ReadMatrix():GetField(1, 4)) end", "receive").unwrap();
    client.runtime.receive_net_message(&message.name, &message.data, entity::EntityHandle::NULL);
    assert_eq!(client.printed(), "4\n");
}

#[test]
fn sql_database() {
    let g = game(Realm::Server, &[]);
    let home = TempDir::new();
    g.runtime.set_data_dir(gmod_fs::UserDir::open(&home.0, None).unwrap());
    g.runtime
        .run_string(
            r#"
            print(sql.Query("CREATE TABLE t (id INTEGER, name TEXT, score REAL, note TEXT); INSERT INTO t VALUES (1, 'a', 1.5, NULL);"))
            print(sql.Query("INSERT INTO t VALUES (2, 'b', 2, 'x'); -- a comment"))
            local rows = sql.Query("SELECT * FROM t ORDER BY id")
            print(#rows, rows[1].id, type(rows[1].id), rows[1].name, rows[1].score, rows[1].note, rows[2].score)
            print(sql.Query("SELECT * FROM t WHERE id = 3"), sql.Query("SELECT nope"), sql.m_strError)
            local typed = sql.QueryTyped("SELECT * FROM t WHERE name = ? OR id = ? ORDER BY id", "b", 1)
            print(#typed, typed[1].id, type(typed[1].id), typed[1].note, typed[2].note, typed[2].score)
            print(#sql.QueryTyped("SELECT * FROM t WHERE id = ?", 9), sql.QueryTyped("SELECT ?", 1, 2))
            local row = sql.QueryTyped("SELECT ? AS yes, ? AS none, 9007199254740993 AS big", true, nil)[1]
            print(row.yes, row.none, row.big, type(row.big))
            print(sql.QueryTyped("SELECT 1; SELECT 2"))
            print(sql.Query("ATTACH DATABASE 'other.db' AS other"), sql.Query("VACUUM INTO 'copy.db'"), sql.Query("VACUUM"))
            "#,
            "sql",
        )
        .unwrap();
    let printed = g.printed();
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines[0], "nil");
    assert_eq!(lines[1], "nil");
    assert_eq!(lines[2], "2\t1\tstring\ta\t1.5\tNULL\t2.0");
    assert_eq!(lines[3], "nil\tfalse\tno such column: nope");
    assert_eq!(lines[4], "2\t1\tnumber\tnil\tx\t2");
    assert_eq!(lines[5], "0\tfalse");
    assert_eq!(lines[6], "1\tnil\t9007199254740993\tstring");
    assert_eq!(lines[7], "false");
    assert_eq!(lines[8], "false\tfalse\tfalse");
    assert!(!home.0.join("other.db").exists() && !home.0.join("copy.db").exists());
    let error = g.runtime.lua().load("sql.QueryTyped('SELECT ?', {})").exec().unwrap_err().to_string();
    assert!(error.contains("bad argument #2 to 'QueryTyped'"), "{error}");

    // the database is a file in openGMOD's folder, there for the next runtime
    assert!(home.0.join(sql::file_name(Realm::Server)).is_file());
    let again = game(Realm::Server, &[]);
    again.runtime.set_data_dir(gmod_fs::UserDir::open(&home.0, None).unwrap());
    assert_eq!(again.eval::<String>("return sql.Query('SELECT name FROM t WHERE id = 2')[1].name"), "b");
    // without the folder, in memory
    let memory = game(Realm::Client, &[]);
    assert!(memory.eval::<bool>("return sql.Query('CREATE TABLE x (a)') == nil and sql.Query('SELECT * FROM x') == nil"));
}

#[test]
fn timers_and_hooks() {
    let g = game(
        Realm::Server,
        &[(
            "lua/main.lua",
            "hook = { Call = function(name) if name == 'Think' then THINKS = (THINKS or 0) + 1 end end }
             timer.Simple(0.5, function() print('simple', CurTime()) end)
             timer.Create('rep', 0.25, 3, function() print('rep', timer.RepsLeft('rep')) end)
             timer.Create('bad', 0.1, 1, function() error('in a timer') end)",
        )],
    );
    assert!(g.runtime.include("main.lua"));
    for _ in 0..10 {
        g.runtime.think(0.1);
    }
    let printed = g.printed();
    let reps: Vec<&str> = printed.lines().filter(|l| l.starts_with("rep")).collect();
    assert_eq!(reps, ["rep\t2", "rep\t1", "rep\t0"], "{printed}");
    assert!(printed.contains("simple\t0.5"), "{printed}");
    assert_eq!(printed.matches("rep\t").count(), 3, "{printed}");
    assert!(!g.eval::<bool>("return timer.Exists('rep')"));
    assert_eq!(g.eval::<i32>("return THINKS"), 10);
    assert!((g.runtime.cur_time() - 1.0).abs() < 1e-9);
    assert_eq!(g.messages().len(), 1);
    assert!(g.messages()[0].ends_with("in a timer"), "{:?}", g.messages());
    // paused timers keep their time
    g.runtime.run_string("timer.Create('p', 1, 0, function() print('p') end) timer.Pause('p')", "x").unwrap();
    g.runtime.think(5.0);
    assert!((g.eval::<f64>("return timer.TimeLeft('p')") - 1.0).abs() < 1e-9);
}

#[test]
fn convars_and_commands() {
    let g = game(
        Realm::Server,
        &[(
            "lua/main.lua",
            "cvars = { OnConVarChanged = function(name, old, new) print('changed', name, old, new) end }
             concommand = { Run = function(ply, cmd, args, str) print('command', cmd, #args, str, ply == NULL) end }
             local cv = CreateConVar('sbox_test', '5', { FCVAR_ARCHIVE, FCVAR_NOTIFY }, 'help', 0, 10)
             print(cv:GetInt(), cv:GetFloat(), cv:GetBool(), cv:GetDefault(), cv:GetFlags(), cv:IsFlagSet(FCVAR_ARCHIVE))
             cv:SetInt(50)
             print(cv:GetString(), GetConVar_Internal('SBOX_TEST') == cv, ConVarExists('sv_gravity'))
             AddConsoleCommand('say_hi')
             RunConsoleCommand('say_hi', 'a b', 2)
             RunConsoleCommand('sbox_test', '3')",
        )],
    );
    assert!(g.runtime.include("main.lua"));
    let printed = g.printed();
    assert!(printed.contains("5\t5\ttrue\t5\t384\ttrue\n"), "{printed}");
    assert!(printed.contains("changed\tsbox_test\t5\t10\n10\ttrue\ttrue\n"), "{printed}");
    assert!(!printed.contains("command"), "commands wait for the next frame");
    g.runtime.think(0.01);
    let printed = g.printed();
    assert!(printed.contains("command\tsay_hi\t2\ta b 2\ttrue\n"), "{printed}");
    assert!(printed.contains("changed\tsbox_test\t10\t3\n"), "{printed}");
    g.runtime.run_command("sv_gravity 300; nonsense");
    assert_eq!(g.eval::<String>("return GetConVar_Internal('sv_gravity'):GetString()"), "300");
    assert!(g.printed().contains("Unknown command \"nonsense\""));
    assert_eq!(convar::atof("  12.5e1abc"), 125.0);
    assert_eq!(convar::atof("abc"), 0.0);
    assert_eq!(convar::split_commands("a \"b c\";d // e\nf"), [vec!["a", "b c"], vec!["d"], vec!["f"]]);
}

#[test]
fn files() {
    let g = game(
        Realm::Server,
        &[("lua/autorun/a.lua", "x"), ("lua/autorun/b.lua", "y"), ("lua/autorun/server/c.lua", "z"), ("data_static/x.txt", "game")],
    );
    let home = TempDir::new();
    g.runtime.set_data_dir(gmod_fs::UserDir::open(&home.0, None).unwrap());
    g.runtime
        .run_string(
            r#"
            local files, dirs = file.Find("autorun/*", "LUA")
            print(table.concat(files, ","), table.concat(dirs, ","))
            print(file.Exists("autorun/a.lua", "LUA"), file.IsDir("autorun", "lsv"), file.Exists("nope.lua", "LUA"))
            local f = file.Open("Saves/Test.txt", "wb", "DATA")
            f:Write("hello\nworld") f:WriteByte(33) f:Close()
            print(file.Open("bad.exe", "wb", "DATA"), file.Open("x.txt", "wb", "GAME"))
            local r = file.Open("saves/test.txt", "rb", "DATA")
            print(r:Size(), r:ReadLine(), r:Read(5), r:ReadByte(), r:Read(1), r:EndOfFile())
            r:Close()
            local a = file.Open("saves/test.txt", "ab", "DATA") a:Write("!") a:Close()
            print(file.Open("saves/test.txt", "rb", "DATA"):Read())
            print(file.Size("saves/test.txt", "DATA"), file.Size("nope", "DATA"), file.Open("data_static/x.txt", "r", "GAME"):Read())
            print(file.Rename("saves/test.txt", "saves/moved.txt"), file.Delete("saves/moved.txt"), file.Exists("saves/moved.txt", "DATA"))
            "#,
            "files",
        )
        .unwrap();
    assert_eq!(
        g.printed(),
        "a.lua,b.lua\tserver\ntrue\ttrue\tfalse\nnil\tnil\n12\thello\n\tworld\t33\tnil\ttrue\nhello\nworld!!\n13\t-1\tgame\ntrue\ttrue\tfalse\n"
    );
    assert!(home.0.join("data").is_dir());
}

#[test]
fn util_library() {
    let g = game(Realm::Server, &[]);
    assert_eq!(g.eval::<i64>("return util.AddNetworkString('a')"), 1);
    assert_eq!(g.eval::<i64>("util.AddNetworkString('b') return util.AddNetworkString('A')"), 1);
    assert_eq!(g.eval::<String>("return util.NetworkIDToString(2)"), "b");
    assert_eq!(g.eval::<String>("return util.CRC('123456789')"), "3421780262");
    assert_eq!(g.eval::<String>("return util.Base64Encode('hello world')"), "aGVsbG8gd29ybGQ=");
    assert_eq!(g.eval::<String>("return util.Base64Decode('aGVsbG8gd29ybGQ=')"), "hello world");
    assert_eq!(g.eval::<String>("return util.TableToJSON({1, 2, 'x', true})"), "[1,2,\"x\",true]");
    assert_eq!(g.eval::<String>("return util.TableToJSON({a = {b = 1.5}})"), "{\"a\":{\"b\":1.5}}");
    assert_eq!(g.eval::<String>("return util.TableToJSON({v = Vector(1, 2, 3)})"), "{\"v\":\"[1 2 3]\"}");
    assert!(g.eval::<bool>(
        "local t = util.JSONToTable('{\"1\": \"a\", \"v\": \"[1 2 3]\", \"l\": [true, null, 2], \"s\": \"\\\\u00e9\"}')
         return t[1] == 'a' and t.v == Vector(1, 2, 3) and t.l[1] == true and t.l[3] == 2 and t.s == 'é'"
    ));
    assert!(g.eval::<bool>("return util.JSONToTable('[1, 2') == nil and util.JSONToTable('\"x\"') == nil"));
    assert!(g.eval::<bool>(
        "local t = util.KeyValuesToTable('\"root\" { \"Name\" \"x\" \"Count\" \"3\" \"sub\" { \"1\" \"a\" } }')
         return t.name == 'x' and t.count == 3 and t.sub[1] == 'a'"
    ));
    assert!(g.eval::<bool>("return util.KeyValuesToTable('\"r\" { \"Name\" \"x\" }', false, true).Name == 'x'"));
    let kv: String = g.eval("return util.TableToKeyValues({ name = 'x' }, 'root')");
    assert_eq!(kv, "\"root\"\n{\n\t\"name\"\t\t\"x\"\n}\n");
    assert_eq!(g.eval::<String>("return util.SteamIDTo64('STEAM_0:1:123')"), "76561197960265975");
    assert_eq!(g.eval::<String>("return util.SteamIDFrom64('76561197960265975')"), "STEAM_0:1:123");
}

#[test]
fn net_messages() {
    let server = game(Realm::Server, &[]);
    let error = server.runtime.lua().load("net.Start('unknown')").exec().unwrap_err().to_string();
    assert!(error.contains("unpooled message name"), "{error}");
    server
        .runtime
        .run_string(
            "util.AddNetworkString('hello')
             net.Start('hello') net.WriteBit(true) net.WriteUInt(300, 9) net.WriteInt(-5, 4) net.WriteString('héllo')
             net.WriteFloat(1.5) net.WriteDouble(0.1) net.WriteVector(Vector(1, 2, 3)) net.WriteData('ab\\0c', 4)
             net.WriteUInt64('18446744073709551615') net.WriteEntity(NULL)
             print(net.BytesWritten())
             net.Broadcast()",
            "send",
        )
        .unwrap();
    let messages = server.runtime.take_net_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].name, "hello");

    let client = game(Realm::Client, &[]);
    client
        .runtime
        .run_string(
            "net.Receivers = {}
             function net.Incoming(len, from)
                 local name = util.NetworkIDToString(net.ReadHeader())
                 print(name, len, from == NULL, net.ReadBit(), net.ReadUInt(9), net.ReadInt(4), net.ReadString(), net.ReadFloat(),
                     net.ReadDouble(), net.ReadVector(), net.ReadData(4) == 'ab\\0c', net.ReadUInt64(), net.ReadEntity() == NULL,
                     net.BytesLeft(), net.ReadUInt(8))
             end",
            "receive",
        )
        .unwrap();
    client.runtime.receive_net_message(&messages[0].name, &messages[0].data, entity::EntityHandle::NULL);
    let printed = client.printed();
    assert_eq!(
        printed,
        format!(
            "hello\t{}\ttrue\t1\t300\t-5\théllo\t1.5\t0.1\t1.000000 2.000000 3.000000\ttrue\t18446744073709551615\tfalse\t0\t0\n",
            messages[0].data.len + 16
        )
    );
    assert!(server.printed().starts_with(&format!("{}\n", (messages[0].data.len + 16 + 24).div_ceil(8))));
    assert!(client.eval::<bool>("return net.ReadString() == ''"), "nothing to read outside a message");
}

#[test]
fn materials() {
    let g = game(
        Realm::Server,
        &[(
            "materials/test/thing.vmt",
            "\"VertexLitGeneric\" { \"$basetexture\" \"test/thing\" \"$alpha\" \"0.5\" \"$color\" \"[1 0 0]\" }",
        )],
    );
    assert!(g.eval::<bool>(
        "local m, t = Material('test/thing')
         return type(t) == 'number' and not m:IsError() and m:GetShader() == 'vertexlitgeneric' and m:GetString('$BaseTexture') == 'test/thing'
             and m:GetFloat('$alpha') == 0.5 and m:GetVector('$color') == Vector(1, 0, 0) and m:GetName() == 'test/thing'"
    ));
    assert!(g.eval::<bool>("local m = Material('missing/thing') return m:IsError() and getmetatable(m) == FindMetaTable('IMaterial')"));
    assert!(g.eval::<bool>("local m = Material('test/thing') m:SetFloat('$alpha', 1) return m:GetKeyValues()['$alpha'] == 1"));
}

#[test]
fn empty_world() {
    let g = game(Realm::Server, &[]);
    assert!(g.eval::<bool>("return #player.GetAll() == 0 and #ents.GetAll() == 1 and Entity(1) == NULL and not NULL:IsValid()"));
    assert!(g.eval::<bool>("return game.GetWorld():IsValid() and game.GetWorld():IsWorld() and tostring(NULL) == '[NULL Entity]'"));
    assert!(g.eval::<bool>("return ents.GetAll()[1] == game.GetWorld() and tostring(Entity(0)) == 'Entity [0][worldspawn]'"));
    assert!(g.eval::<bool>(
        "return FindMetaTable('Player').MetaName == 'Player' and FindMetaTable('Player').MetaBaseClass == FindMetaTable('Entity')"
    ));
    assert!(g.eval::<bool>("return FindMetaTable('Panel') == nil"), "panels are the client's");
    assert!(g.eval::<bool>("return ai.GetScheduleID('a') == ai.GetScheduleID('a') and ai.GetScheduleID('a') ~= ai.GetScheduleID('b')"));
    let error = g.runtime.lua().load("NULL.x = 1").exec().unwrap_err().to_string();
    assert!(error.contains("Tried to use a NULL entity!"), "{error}");
}

#[test]
fn a_gamemode_starts() {
    let g = game(
        Realm::Server,
        &[
            ("lua/includes/init.lua", "include('modules.lua')"),
            (
                "lua/modules.lua",
                "gamemode = { list = {} }
                 function gamemode.Register(t, name, base) t.Base = base gamemode.list[name] = t print('gamemode', name, base) end
                 function gamemode.Get(name) return gamemode.list[name] end
                 hook = { Call = function(name, gm) if gm and gm[name] then return gm[name](gm) end print('hook', name, gm ~= nil) end }
                 scripted_ents = { Register = function(t, name) print('entity', name, t.Folder, t.Base) end }
                 weapons = { Register = function(t, name) print('weapon', name, t.Primary ~= nil) end }",
            ),
            ("lua/autorun/first.lua", "print('autorun')"),
            ("lua/autorun/client/no.lua", "print('not on the server')"),
            ("lua/autorun/server/second.lua", "print('autorun server')"),
            ("gamemodes/base/base.txt", "\"base\" { \"title\" \"Base\" }"),
            ("gamemodes/base/gamemode/init.lua", "GM.Name = 'Base' print('base', GM.FolderName, GM.Folder)"),
            ("gamemodes/base/entities/entities/base_anim.lua", "ENT.Type = 'anim'"),
            ("gamemodes/mine/mine.txt", "\"mine\" { \"base\" \"base\" }"),
            ("gamemodes/mine/gamemode/init.lua", "AddCSLuaFile('cl_init.lua') include('shared.lua')"),
            ("gamemodes/mine/gamemode/cl_init.lua", "include('shared.lua')"),
            ("gamemodes/mine/gamemode/shared.lua", "function GM:Initialize() print('initialize', self.FolderName) end"),
            ("gamemodes/mine/entities/entities/thing/init.lua", "include('shared.lua')"),
            ("gamemodes/mine/entities/entities/thing/shared.lua", "ENT.Base = 'base_anim'"),
            ("lua/weapons/gun.lua", "SWEP.PrintName = 'Gun'"),
        ],
    );
    g.runtime.start("mine");
    assert_eq!(g.messages(), Vec::<String>::new());
    assert_eq!(
        g.printed(),
        "autorun\nautorun server\nhook\tPreGamemodeLoaded\tfalse\nbase\tbase\tgamemodes/base\ngamemode\tbase\tnil\ngamemode\tmine\tbase\n\
         entity\tbase_anim\tbase/entities/entities/base_anim\tnil\nentity\tthing\tmine/entities/entities/thing\tbase_anim\n\
         weapon\tgun\ttrue\nhook\tOnGamemodeLoaded\ttrue\nhook\tPostGamemodeLoaded\ttrue\n\
         hook\tCreateTeams\ttrue\ninitialize\tmine\n"
    );
    assert!(g.eval::<bool>("return GAMEMODE == gamemode.Get('mine') and GM == nil and ENT == nil and SWEP == nil"));
    assert_eq!(g.eval::<String>("return engine.ActiveGamemode()"), "mine");
}

/// A web server on this machine for `HTTP` to talk to: it answers each request with what it
/// was asked, one request per connection. `/missing` is a 404; `/slow` answers too late.
fn web_server() -> String {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    // requests being answered now, and the most there ever were
    let busy = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let busy = busy.clone();
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                let head_end = loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                };
                let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
                let header = |name: &str| {
                    head.lines()
                        .find_map(|line| {
                            line.split_once(':').filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string())
                        })
                        .unwrap_or_default()
                };
                let length: usize = header("content-length").parse().unwrap_or(0);
                while request.len() < head_end + length {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..n]);
                }
                let body = String::from_utf8_lossy(&request[head_end..]).into_owned();
                let target = head.split(' ').nth(1).unwrap_or("").to_string();
                let method = head.split(' ').next().unwrap_or("").to_string();
                let (status, text) = match target.as_str() {
                    "/missing" => ("404 Not Found", "nope".to_string()),
                    "/slow" => {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        ("200 OK", "late".to_string())
                    }
                    "/busy" => {
                        let now = busy.0.fetch_add(1, Ordering::SeqCst) + 1;
                        busy.1.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        busy.0.fetch_sub(1, Ordering::SeqCst);
                        ("200 OK", "done".to_string())
                    }
                    "/peak" => ("200 OK", busy.1.load(Ordering::SeqCst).to_string()),
                    _ => ("200 OK", format!("{method} {target} [{}] [{}] [{body}]", header("content-type"), header("x-test"))),
                };
                let _ =
                    write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nx-echo: yes\r\nConnection: close\r\n\r\n{text}", text.len());
            });
        }
    });
    format!("http://{address}")
}

/// Runs frames until no request is waiting.
fn finish_requests(g: &Game) {
    let started = std::time::Instant::now();
    while g.runtime.http_in_flight() > 0 {
        assert!(started.elapsed().as_secs() < 20, "requests still waiting");
        std::thread::sleep(std::time::Duration::from_millis(10));
        g.runtime.think(0.01);
    }
}

#[test]
fn http_requests() {
    let base = web_server();
    let g = game(Realm::Server, &[]);
    g.runtime.allow_local_http(true);
    g.runtime
        .run_string(
            &format!(
                r#"
                local base = "{base}"
                local function show(name) return function(code, body, headers) print(name, code, body, headers["X-Echo"]) end end
                local function fail(name) return function(reason) print(name, reason) end end
                print(HTTP({{ url = base .. "/get", parameters = {{ q = "a b&c" }}, headers = {{ ["X-Test"] = "t" }}, success = show("get") }}))
                HTTP({{ url = base .. "/form", method = "post", parameters = {{ k = "v" }}, success = show("form") }})
                HTTP({{ url = base .. "/raw", method = "PUT", body = "{{}}", type = "application/json", success = show("raw") }})
                HTTP({{ url = base .. "/missing", success = show("missing") }})
                HTTP({{ url = base .. "/slow", timeout = 0.3, success = show("slow"), failed = fail("slow") }})
                HTTP({{ url = "not a url", failed = fail("bad") }})
                HTTP({{ url = base .. "/x", method = "BREW", failed = fail("brew") }})
                print(HTTP("not a table"))
                "#
            ),
            "http",
        )
        .unwrap();
    assert_eq!(g.printed(), "true\nfalse\n", "answers come on a later frame");
    finish_requests(&g);
    let printed = g.printed();
    let mut lines: Vec<&str> = printed.lines().skip(2).collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            "bad\tinvalid url",
            "brew\tunsuccessful",
            "form\t200\tPOST /form [application/x-www-form-urlencoded] [] [k=v]\tyes",
            "get\t200\tGET /get?q=a%20b%26c [] [t] []\tyes",
            "missing\t404\tnope\tyes",
            "raw\t200\tPUT /raw [application/json] [] [{}]\tyes",
            "slow\ttimeout",
        ]
    );
    assert!(g.messages().is_empty(), "{:?}", g.messages());

    // as in GMod, nothing on this machine or the local network unless the server allows it
    for realm in [Realm::Server, Realm::Client, Realm::Menu] {
        let g = game(realm, &[]);
        g.runtime
            .run_string(
                &format!(
                    r#"HTTP({{ url = "{base}/get", success = function() print("reached") end, failed = function(r) print("refused", r) end }})
                       HTTP({{ url = "http://localhost:1/", failed = function(r) print("refused", r) end }})"#
                ),
                "local",
            )
            .unwrap();
        finish_requests(&g);
        assert_eq!(g.printed(), "refused\tunsuccessful\nrefused\tunsuccessful\n", "{realm}");
    }
    let client = game(Realm::Client, &[]);
    client.runtime.allow_local_http(true);
    client.runtime.run_string(&format!(r#"HTTP({{ url = "{base}/get", failed = function(r) print(r) end }})"#), "client").unwrap();
    finish_requests(&client);
    assert_eq!(client.printed(), "unsuccessful\n", "only a server may allow it");
}

#[test]
fn http_waits_its_turn() {
    let base = web_server();
    let g = game(Realm::Server, &[]);
    g.runtime.allow_local_http(true);
    g.runtime
        .run_string(
            &format!(
                r#"
                local done = 0
                for i = 1, 40 do
                    HTTP({{ url = "{base}/busy", success = function() done = done + 1 end, failed = print }})
                end
                function Finished() return done end
                "#
            ),
            "many",
        )
        .unwrap();
    finish_requests(&g);
    assert_eq!(g.eval::<u32>("return Finished()"), 40);
    g.runtime.run_string(&format!(r#"HTTP({{ url = "{base}/peak", success = function(_, body) print(body) end }})"#), "peak").unwrap();
    finish_requests(&g);
    let peak: usize = g.printed().trim().parse().unwrap();
    assert!((1..=http::MAX_PARALLEL).contains(&peak), "{peak} at once");
}

#[test]
fn public_addresses() {
    let public = |text: &str| http::is_public(text.parse().unwrap());
    for address in ["8.8.8.8", "1.1.1.1", "2606:4700::1111", "::ffff:8.8.8.8", "2002:808:808::1"] {
        assert!(public(address), "{address}");
    }
    for address in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "224.0.0.1",
        "255.255.255.255",
        "::1",
        "::",
        "fe80::1",
        "fd00::1",
        "::ffff:127.0.0.1",
        "::ffff:192.168.0.1",
        "64:ff9b::a00:1",
        "2002:c0a8:101::1",
        "ff02::1",
    ] {
        assert!(!public(address), "{address}");
    }
}

/// What the game's Lua gives entities, cut down: `scripted_ents` and `weapons` to register
/// scripts in, `hook.Call` on the table `HOOKS`, and `Entity.__index` as
/// `includes/extensions/entity.lua` has it.
const ENTITY_LUA: &str = r#"
    scripted_ents = { list = {} }
    function scripted_ents.Register(t, name) scripted_ents.list[name] = { t = t, type = t.Type } t.ClassName = name end
    function scripted_ents.GetStored(name) return scripted_ents.list[name] end
    function scripted_ents.GetType(name) local s = scripted_ents.list[name] return s and s.type end
    function scripted_ents.Get(name, t) t = t or {} for k, v in pairs(scripted_ents.list[name].t) do t[k] = v end return t end
    weapons = { list = {} }
    function weapons.GetStored(name) return weapons.list[name] end
    function weapons.Get(name, t) t = t or {} for k, v in pairs(weapons.list[name]) do t[k] = v end return t end
    HOOKS = {}
    hook = { Call = function(name, gm, ...) if HOOKS[name] then return HOOKS[name](...) end end }
    local meta = FindMetaTable('Entity')
    function meta:__index(key)
        local val = meta[key]
        if val ~= nil then return val end
        local tab = meta.GetTable(self)
        if tab then return tab[key] end
    end
    function close(a, b) return (a - b):Length() < 0.001 end
"#;

/// A runtime with [`ENTITY_LUA`] run, and a model `models/box.mdl` 16 units wide and high.
fn entity_game(realm: Realm) -> Game {
    let mdl = gmod_model::testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let g = game_with(realm, &[("lua/includes/init.lua", ENTITY_LUA.as_bytes()), ("models/box.mdl", &mdl)]);
    g.runtime.init();
    g
}

#[test]
fn entities_are_made_and_found() {
    let g = entity_game(Realm::Server);
    assert!(g.eval::<bool>(
        "local e = ents.Create('prop_physics') E = e
         return tostring(e) == 'Entity [2][prop_physics]' and e:EntIndex() == 2 and rawequal(e, Entity(2))
             and ({ [e] = true })[Entity(2)] and e:GetClass() == 'prop_physics' and not e:IsWeapon()"
    ));
    assert!(g.eval::<bool>("return ents.Create('nope') == NULL and Entity(-1) == NULL and Entity(0.5) == NULL"));
    assert_eq!(g.printed(), "Attempted to create unknown entity type nope!\n");
    assert!(g.eval::<bool>(
        "local w = ents.Create('weapon_pistol')
         return getmetatable(w) == FindMetaTable('Weapon') and w:IsWeapon() and tostring(w):sub(1, 6) == 'Weapon'"
    ));

    // the entity's own table, behind the metatable
    assert!(g.eval::<bool>("E.note = 5 return E:GetTable().note == 5 and E.note == 5 and E.GetPos == FindMetaTable('Entity').GetPos"));

    // where it is, and its children
    assert!(g.eval::<bool>(
        "E:SetPos(Vector(1, 2, 3)) E:SetAngles(Angle(0, 90, 0))
         return E:GetPos() == Vector(1, 2, 3) and close(E:LocalToWorld(Vector(10, 0, 0)), Vector(1, 12, 3))
             and close(E:WorldToLocal(Vector(1, 12, 3)), Vector(10, 0, 0)) and close(E:GetForward(), Vector(0, 1, 0))"
    ));
    assert!(g.eval::<bool>(
        "local p, c = ents.Create('prop_dynamic'), ents.Create('prop_dynamic')
         p:SetPos(Vector(100, 0, 0)) c:SetPos(Vector(110, 0, 0)) c:SetParent(p) p:SetPos(Vector(200, 0, 0))
         P, C = p, c
         return c:GetPos() == Vector(210, 0, 0) and c:GetLocalPos() == Vector(10, 0, 0) and c:GetParent() == p
             and p:GetChildren()[1] == c and #p:GetChildren() == 1"
    ));
    assert!(g.eval::<bool>(
        "local near = ents.FindInSphere(Vector(205, 0, 0), 6)
         return #ents.FindByClass('prop_*') == 3 and #ents.FindByClass('PROP_DYNAMIC') == 2 and #near == 2
             and #ents.FindInBox(Vector(0.5, 0.5, 0.5), Vector(5, 5, 5)) == 1 and #ents.FindInSphere(Vector(205, 0, 0), 4) == 0"
    ));

    // removed at the end of the frame; the index is used again, the old handle stays NULL
    assert!(g.eval::<bool>("C:Remove() return IsValid == nil and C:IsValid() and C:IsMarkedForDeletion() and #P:GetChildren() == 0"));
    g.runtime.think(0.01);
    assert!(g.eval::<bool>(
        "local index = 5
         local new = ents.Create('info_target')
         return not C:IsValid() and new:EntIndex() == index and C ~= new and tostring(C) == '[NULL Entity]' and #P:GetChildren() == 0"
    ));
    let error = g.runtime.lua().load("C:GetPos()").exec().unwrap_err().to_string();
    assert!(error.contains("Tried to use a NULL entity!"), "{error}");

    // variables kept with it
    assert!(g.eval::<bool>(
        "E:SetNWInt('n', 3.7) E:SetNW2String('s', 12) E:SetDTFloat(0, 1.5) E:SetNWEntity('e', P)
         return E:GetNWInt('n') == 3 and E:GetNWInt('missing', 9) == 9 and E:GetNWString('missing') == ''
             and E:GetNW2String('s') == '12' and E:GetDTFloat(0) == 1.5 and E:GetDTBool(1) == false
             and E:GetNWEntity('e') == P and E:GetNWEntity('missing') == NULL and E:GetNWVarTable().n == 3"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn scripted_entities_live_and_think() {
    let g = entity_game(Realm::Server);
    g.runtime
        .run_string(
            "local ENT = { Type = 'anim' }
             function ENT:SetupDataTables() print('setup', self:GetClass()) end
             function ENT:Initialize() print('initialize', self.Entity == self) self:SetModel('models/box.mdl') end
             function ENT:Think() self.thoughts = (self.thoughts or 0) + 1 if self.fast then self:NextThink(CurTime()) return true end end
             function ENT:OnRemove() print('removed', self:GetName()) end
             function ENT:KeyValue(k, v) print('key', k, v) if k == 'secret' then return true end end
             scripted_ents.Register(ENT, 'sent_test')
             HOOKS.OnEntityCreated = function(e) print('created', e:GetClass()) end
             HOOKS.EntityRemoved = function(e) print('hook removed', e:GetClass(), e:IsValid()) end
             SLOW = ents.Create('sent_test') SLOW:SetKeyValue('secret', 1) SLOW:SetKeyValue('targetname', 'slow') SLOW:Spawn()
             FAST = ents.Create('sent_test') FAST.fast = true FAST:Spawn()",
            "sent",
        )
        .unwrap();
    assert_eq!(
        g.printed(),
        "setup\tsent_test\ncreated\tsent_test\nkey\tsecret\t1\nkey\ttargetname\tslow\ninitialize\ttrue\n\
         setup\tsent_test\ncreated\tsent_test\ninitialize\ttrue\n"
    );
    assert!(g.eval::<bool>(
        "return SLOW:GetName() == 'slow' and SLOW:GetKeyValues().secret == nil and SLOW:IsScripted()
             and SLOW:OBBMaxs() == Vector(8, 8, 16) and SLOW:OBBMins() == Vector(-8, -8, 0) and SLOW:GetModel() == 'models/box.mdl'"
    ));
    // a second and a half: every tick for one, every 0.2 s for the other
    for _ in 0..100 {
        g.runtime.think(0.015);
    }
    let (slow, fast): (u32, u32) = g.eval("return SLOW.thoughts, FAST.thoughts");
    assert!((7..=9).contains(&slow), "{slow}");
    assert!((98..=102).contains(&fast), "{fast}");

    g.printed.borrow_mut().clear();
    g.runtime.run_string("SLOW:Remove() SLOW:Remove()", "remove").unwrap();
    assert_eq!(g.printed(), "removed\tslow\nhook removed\tsent_test\ttrue\n", "once, while it is still valid");
    g.runtime.think(0.015);
    assert!(g.eval::<bool>("return not SLOW:IsValid() and FAST:IsValid() and #ents.FindByClass('sent_test') == 1"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn map_entities() {
    let g = entity_game(Realm::Server);
    g.runtime
        .run_string(
            "local ENT = { Type = 'point' }
             function ENT:KeyValue(k, v) if k == 'code' then self.code = v end end
             function ENT:Initialize() RunString(self.code) end
             scripted_ents.Register(ENT, 'sent_runner')
             HOOKS.EntityKeyValue = function(e, k, v) if k == 'skin' then return '3' end end
             HOOKS.InitPostEntity = function() print('post', #ents.GetAll()) end",
            "map",
        )
        .unwrap();
    let pairs = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
    let map = Map {
        name: "gm_test".into(),
        entities: vec![
            pairs(&[("classname", "worldspawn"), ("skyname", "sky_day01_01")]),
            pairs(&[("classname", "info_player_start"), ("origin", "0 0 16"), ("angles", "0 90 0")]),
            pairs(&[("classname", "prop_physics"), ("targetname", "barrel"), ("origin", "100 0 0"), ("skin", "1")]),
            pairs(&[("classname", "func_door"), ("model", "*1"), ("parentname", "barrel"), ("targetname", "door")]),
            pairs(&[("classname", "sent_runner"), ("code", "print('runner', ents.FindByName('door')[1]:GetParent():GetName())")]),
            pairs(&[("classname", "some_class_of_another_game"), ("origin", "1 2 3")]),
        ],
        brush_models: vec![([-512.0, -512.0, -64.0], [512.0, 512.0, 0.0]), ([0.0, 0.0, 0.0], [64.0, 8.0, 96.0])],
        ..Map::default()
    };
    g.runtime.spawn_map(&map);
    assert_eq!(g.printed(), "runner\tbarrel\npost\t6\n", "parents are set before anything spawns");
    assert!(g.eval::<bool>(
        "local world, start = Entity(0), ents.FindByClass('info_player_start')[1]
         return game.GetMap() == 'gm_test' and world:GetModel() == 'maps/gm_test.bsp' and world:OBBMaxs() == Vector(512, 512, 0)
             and world:GetKeyValues().skyname == 'sky_day01_01' and world:MapCreationID() == 1234
             and start:GetPos() == Vector(0, 0, 16) and start:GetAngles() == Angle(0, 90, 0)
             and start:CreatedByMap() and start:MapCreationID() == 1235 and ents.GetMapCreatedEntity(1235) == start"
    ));
    assert!(g.eval::<bool>(
        "local barrel, door = ents.FindByName('barrel')[1], ents.FindByName('door')[1]
         return barrel:GetSkin() == 3 and door:GetParent() == barrel and door:OBBMaxs() == Vector(64, 8, 96)
             and door:GetPos() == Vector(0, 0, 0) and not ents.Create('info_target'):CreatedByMap()
             and ents.FindByClass('some_class_of_another_game')[1]:GetPos() == Vector(1, 2, 3)"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

/// The engine's pistol as `scripts/weapons/weapon_pistol.txt` has it, cut down.
const PISTOL: &str = r##"WeaponData
{
    "printname"         "#HL2_Pistol"
    "bucket"            "1"
    "bucket_position"   "0"
    "clip_size"         "18"
    "default_clip"      "36"
    "primary_ammo"      "Pistol"
    "secondary_ammo"    "None"
    "weight"            "2"
}"##;

/// What the gamemode does for players in these tests: a spawn point, speeds, and a pistol with
/// some rounds more.
const PLAYER_LUA: &str = r#"
    HOOKS.PlayerSelectSpawn = function(p) return ents.FindByClass('info_player_start')[1] end
    HOOKS.PlayerSpawn = function(p)
        p:SetWalkSpeed(200) p:SetRunSpeed(400)
        p:GiveAmmo(10, 'Pistol', true)
        p:Give('weapon_pistol')
    end
"#;

/// A map for players: a floor with its top at 0, a wall from x 100 to 132, a spawn point at
/// the middle looking along y.
fn player_map() -> Map {
    use gmod_bsp::testmap::{TestBrush, TestMap};
    let brushes = vec![
        TestBrush::cuboid([-1024.0, -1024.0, -64.0], [1024.0, 1024.0, 0.0]),
        TestBrush::cuboid([100.0, -1024.0, 0.0], [132.0, 1024.0, 256.0]).material("TEST/WALL", 0),
    ];
    let test = TestMap { models: vec![brushes], ..TestMap::default() };
    let bsp = gmod_bsp::Bsp::parse("test.bsp".into(), Arc::from(test.bytes())).unwrap();
    let texture =
        |info: &gmod_bsp::TexInfo| usize::try_from(info.texdata).ok().and_then(|i| bsp.materials.get(i)).cloned().unwrap_or_default();
    let pairs = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
    Map {
        name: "gm_players".into(),
        entities: vec![
            pairs(&[("classname", "worldspawn")]),
            pairs(&[("classname", "info_player_start"), ("origin", "0 0 0"), ("angles", "0 90 0")]),
        ],
        brush_models: bsp.models.iter().map(|m| (m.mins, m.maxs)).collect(),
        collision: Some(Arc::new(gmod_bsp::Collision::new(&bsp))),
        textures: bsp.texinfo.iter().map(texture).collect(),
    }
}

/// A server with [`ENTITY_LUA`] and [`PLAYER_LUA`] on [`player_map`], the pistol's script and
/// `models/box.mdl`.
fn player_game() -> Game {
    let mdl = gmod_model::testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let g = game_with(
        Realm::Server,
        &[
            ("lua/includes/init.lua", ENTITY_LUA.as_bytes()),
            ("models/box.mdl", &mdl),
            ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes()),
        ],
    );
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime.spawn_map(&player_map());
    g
}

fn alice() -> PlayerInfo {
    PlayerInfo { name: "Alice".into(), steam_id64: Some(76561197960265731), ..PlayerInfo::default() }
}

/// Walking forward along a yaw, with these keys held too.
fn walking(yaw: f32, buttons: u32) -> UserCmd {
    UserCmd { view_angles: vector::Angle::new(0.0, yaw, 0.0), forward_move: gmod_movement::FULL_MOVE, buttons, ..UserCmd::default() }
}

#[test]
fn players_join_spawn_and_leave() {
    let g = player_game();
    g.runtime
        .run_string(
            "local spawn = HOOKS.PlayerSpawn
             HOOKS.PlayerConnect = function(name, address) print('connect', name, address) end
             HOOKS.OnEntityCreated = function(e) if e:IsPlayer() then print('created', e:Nick()) end end
             HOOKS.PlayerAuthed = function(p, id, unique) print('authed', p:Nick(), id, unique) end
             HOOKS.PlayerInitialSpawn = function(p, transition) print('initial', p:Nick(), transition) end
             HOOKS.PlayerSpawn = function(p, transition) print('spawn', p:Nick(), transition) spawn(p) end
             HOOKS.PlayerDisconnected = function(p) print('disconnected', p:Nick()) end
             HOOKS.EntityRemoved = function(e) if e:IsPlayer() then print('removed', e:Nick()) end end",
            "hooks",
        )
        .unwrap();
    let alice = g.runtime.join(alice()).expect("a free slot");
    assert_eq!(
        g.printed(),
        "connect\tAlice\tloopback\ncreated\tAlice\nauthed\tAlice\tSTEAM_0:1:1\t1\ninitial\tAlice\tfalse\nspawn\tAlice\tfalse\n"
    );
    assert_eq!(alice.index, Some(1));
    assert!(g.runtime.join(PlayerInfo { name: "Bob".into(), ..PlayerInfo::default() }).is_none(), "one slot");
    assert_eq!(g.runtime.players(), vec![alice]);
    assert!(g.eval::<bool>(
        "local p = Entity(1)
         return p:IsPlayer() and tostring(p) == 'Player [1][player]' and getmetatable(p) == FindMetaTable('Player')
             and p:Nick() == 'Alice' and p:UserID() == 1 and Player(1) == p and player.GetAll()[1] == p and player.GetCount() == 1
             and p:SteamID64() == '76561197960265731' and p:AccountID() == 3 and p:IsFullyAuthenticated() and not p:IsBot()
             and p:IsListenServerHost() and p:IPAddress() == 'loopback' and game.MaxPlayers() == 1 and #player.GetHumans() == 1"
    ));
    // standing on the floor at the spawn point, looking where it looks, with a pistol
    assert!(g.eval::<bool>(
        "local p = Entity(1)
         return p:Alive() and p:Health() == 100 and p:GetPos().z < 0.1 and p:GetPos().z > 0 and p:GetPos():Length2D() == 0
             and p:EyeAngles() == Angle(0, 90, 0) and p:GetAngles() == Angle(0, 90, 0) and math.abs(p:EyePos().z - 64) < 0.1
             and p:GetActiveWeapon():GetClass() == 'weapon_pistol' and #p:GetWeapons() == 1"
    ));
    assert!(g.eval::<bool>(
        "local p = Entity(1) local w = p:GetActiveWeapon()
         return w:GetOwner() == p and w:Clip1() == 18 and w:GetMaxClip1() == 18 and w:GetPrimaryAmmoType() == 3
             and w:GetSecondaryAmmoType() == -1 and w:Clip2() == -1 and w:GetPrintName() == '#HL2_Pistol' and w:GetSlot() == 1
             and p:GetAmmoCount('Pistol') == 28 and p:GetAmmoCount(3) == 28 and w:HasAmmo() and game.GetAmmoID('PISTOL') == 3
             and game.GetAmmoName(3) == 'Pistol' and game.GetAmmoMax(3) == 9999"
    ));
    assert!(
        g.eval::<bool>("return Entity(1):GetInfo('cl_playercolor') == '0.24 0.34 0.41' and Entity(1):GetInfoNum('cl_playerskin', 5) == 0")
    );

    g.printed.borrow_mut().clear();
    g.runtime.leave(alice);
    assert_eq!(g.printed(), "disconnected\tAlice\nremoved\tAlice\n");
    g.runtime.think(0.015);
    assert!(g.eval::<bool>("return Entity(1) == NULL and player.GetCount() == 0 and #ents.FindByClass('weapon_pistol') == 0"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn players_move_by_their_commands() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    let pos = |g: &Game| g.runtime.position(alice).unwrap();

    // walking along y for a second: up to speed at once, on the ground all the way, and
    // through a weapon lying in the way
    g.runtime
        .run_string(
            "local w = ents.Create('weapon_pistol') w:SetPos(Vector(0, 100, 0)) w:Spawn()
             w:SetSolid(SOLID_BBOX) w:SetCollisionBounds(Vector(-8, -8, 0), Vector(8, 8, 64))",
            "weapon",
        )
        .unwrap();
    g.runtime.set_input(alice, walking(90.0, 0));
    g.runtime.think(1.0);
    let at = pos(&g);
    assert!(at.y > 170.0 && at.y < 200.0 && at.x.abs() < 0.01, "{at:?}");
    assert!(g.eval::<bool>(
        "local p = Entity(1) return p:OnGround() and math.abs(p:GetVelocity().y - 200) < 1 and p:KeyDown(IN_SPEED) == false"
    ));

    // running, as far as the hooks see it too
    g.runtime
        .run_string("HOOKS.SetupMove = function(p, mv, cmd) MAX, FORWARD = mv:GetMaxSpeed(), cmd:GetForwardMove() end", "setup")
        .unwrap();
    g.runtime.set_input(alice, walking(90.0, player::key::SPEED));
    g.runtime.think(1.0);
    assert!(pos(&g).y - at.y > 350.0, "{:?}", pos(&g));
    assert_eq!(g.eval::<(f32, f32)>("return MAX, FORWARD"), (400.0, gmod_movement::FULL_MOVE));

    // into the wall at x 100: the box stops against it
    g.runtime.set_input(alice, walking(0.0, 0));
    g.runtime.think(1.0);
    assert!(pos(&g).x > 83.0 && pos(&g).x < 84.0, "{:?}", pos(&g));

    // a jump: the key once, and up
    g.runtime.run_string("HOOKS.KeyPress = function(p, key) print('key', key) end", "keys").unwrap();
    g.runtime.set_input(alice, UserCmd { buttons: player::key::JUMP, ..UserCmd::default() });
    let before = pos(&g).z;
    g.runtime.think(0.15);
    assert!(pos(&g).z - before > 15.0, "{:?}", pos(&g));
    assert_eq!(g.printed(), "key\t2\n");
    g.runtime.set_input(alice, UserCmd::default());
    g.runtime.think(1.0);

    // what the hooks can do: change the command, take the movement over, keep its result
    let still = |g: &Game, hook: &str| {
        g.runtime.run_string(hook, "hook").unwrap();
        g.runtime.set_input(alice, walking(180.0, 0));
        let before = pos(g);
        g.runtime.think(0.5);
        let after = pos(g);
        g.runtime.run_string("HOOKS.StartCommand, HOOKS.Move, HOOKS.FinishMove = nil, nil, nil", "reset").unwrap();
        (before - after).length() < 0.01
    };
    assert!(still(&g, "HOOKS.StartCommand = function(p, cmd) cmd:ClearMovement() end"));
    assert!(still(&g, "HOOKS.Move = function(p, mv) return true end"));
    assert!(still(&g, "HOOKS.FinishMove = function(p, mv) return true end"));
    assert!(!still(&g, ""), "and without them, the player walks");

    // noclip flies where the player looks, through the wall
    g.runtime.run_string("Entity(1):SetMoveType(MOVETYPE_NOCLIP)", "noclip").unwrap();
    g.runtime.set_input(alice, UserCmd { view_angles: vector::Angle::new(-30.0, 0.0, 0.0), ..walking(0.0, 0) });
    g.runtime.think(1.0);
    assert!(pos(&g).x > 150.0 && pos(&g).z > 100.0, "{:?}", pos(&g));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn players_are_hurt_die_and_spawn_again() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime
        .run_string(
            "HOOKS.PlayerHurt = function(p, attacker, left, taken) print('hurt', left, taken) end
             HOOKS.DoPlayerDeath = function(p, attacker, info) print('do death', attacker == p, info:GetDamage(), info:IsFallDamage()) end
             HOOKS.PlayerDeath = function(p, inflictor, attacker) print('death', inflictor:GetClass(), attacker:GetClass()) end
             HOOKS.PlayerSilentDeath = function(p) print('silent death') end
             HOOKS.PostPlayerDeath = function(p) print('post death', p:Alive()) end
             HOOKS.PlayerDeathThink = function(p) if p:KeyPressed(IN_ATTACK) then p:Spawn() end end",
            "hooks",
        )
        .unwrap();
    assert!(g.eval::<bool>("local p = Entity(1) p:TakeDamage(30) return p:Health() == 70"));
    // armor takes 80%, at a point of armor for two of damage
    assert!(g.eval::<bool>("local p = Entity(1) p:SetArmor(100) p:TakeDamage(50) return p:Health() == 60 and p:Armor() == 80"));
    assert!(g.eval::<bool>(
        "HOOKS.EntityTakeDamage = function(e, info) return info:GetDamage() > 1000 end
         local p = Entity(1) p:TakeDamage(5000) HOOKS.EntityTakeDamage = nil return p:Health() == 60"
    ));
    assert_eq!(g.printed(), "hurt\t70\t30\nhurt\t60\t10\n");

    // a fall goes past the armor, and kills
    g.printed.borrow_mut().clear();
    assert!(g.eval::<bool>(
        "local p = Entity(1) local info = DamageInfo()
         info:SetDamage(100) info:SetDamageType(DMG_FALL) info:SetAttacker(game.GetWorld()) info:SetInflictor(game.GetWorld())
         p:TakeDamageInfo(info)
         return not p:Alive() and p:Armor() == 80 and #p:GetWeapons() == 0 and p:GetActiveWeapon() == NULL and p:GetAmmoCount('Pistol') == 0"
    ));
    assert_eq!(g.printed(), "hurt\t-40\t100\ndo death\tfalse\t100\ttrue\ndeath\tworldspawn\tworldspawn\npost death\tfalse\n");
    assert!(g.eval::<bool>("return #ents.FindByClass('weapon_pistol') == 0"));

    // dead, the player only thinks of coming back: walking does nothing, a click spawns them
    g.printed.borrow_mut().clear();
    let dead_at = g.runtime.position(alice).unwrap();
    g.runtime.set_input(alice, walking(90.0, 0));
    g.runtime.think(0.5);
    assert_eq!(g.runtime.position(alice).unwrap(), dead_at);
    g.runtime.set_input(alice, UserCmd { buttons: player::key::ATTACK, ..UserCmd::default() });
    g.runtime.think(0.015);
    assert!(
        g.eval::<bool>("local p = Entity(1) return p:Alive() and p:Health() == 100 and p:Armor() == 0 and p:HasWeapon('weapon_pistol')")
    );

    // `Kill` is by the player itself; `KillSilent` tells no one but `PlayerSilentDeath`
    g.printed.borrow_mut().clear();
    g.runtime.run_string("Entity(1):Kill()", "kill").unwrap();
    assert_eq!(g.printed(), "do death\ttrue\t100\tfalse\ndeath\tplayer\tplayer\npost death\tfalse\n");
    g.runtime.run_string("Entity(1):Spawn() Entity(1):KillSilent()", "silent").unwrap();
    assert!(g.printed().ends_with("silent death\npost death\tfalse\n"), "{}", g.printed());

    // a weapon that should drop stays in the world
    assert!(g.eval::<bool>(
        "local p = Entity(1) p:Spawn() p:ShouldDropWeapon(true) local w = p:GetActiveWeapon() p:Kill()
         return w:IsValid() and not w:IsMarkedForDeletion() and w:GetOwner() == NULL and w:GetParent() == NULL"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn weapons_in_a_players_hands() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime
        .run_string(
            "weapons.list.weapon_test = {
                 PrintName = 'Test', Slot = 3, Weight = 10,
                 Primary = { ClipSize = 5, DefaultClip = 10, Ammo = 'SMG1', Automatic = false },
                 Secondary = { ClipSize = -1, DefaultClip = -1, Ammo = 'none' },
                 PrimaryAttack = function(self)
                     print('bang', self:Clip1()) self:SetClip1(self:Clip1() - 1) self:SetNextPrimaryFire(CurTime() + 0.2)
                 end,
                 Reload = function(self) self:DefaultReload(ACT_VM_RELOAD) end,
                 Deploy = function(self) print('deploy') end,
                 Holster = function(self, to) print('holster', to:GetClass()) return true end,
                 Equip = function(self, owner) print('equip', owner:Nick()) end,
                 Think = function(self) THINKS = (THINKS or 0) + 1 end,
             }",
            "weapon",
        )
        .unwrap();
    // heavier than the pistol: in hand at once
    assert!(g.eval::<bool>(
        "local p = Entity(1) local w = p:Give('weapon_test') TEST = w
         return w:IsValid() and p:GetActiveWeapon() == w and p:GetPreviousWeapon():GetClass() == 'weapon_pistol'
             and w:Clip1() == 5 and w:GetMaxClip1() == 5 and p:GetAmmoCount('SMG1') == 5 and w:GetPrimaryAmmoType() == 4
             and w:GetPrintName() == 'Test' and w:GetSlot() == 3 and w:IsScripted() and #p:GetWeapons() == 2
             and p:Give('weapon_test') == NULL and p:GetWeapon('weapon_test') == w and p:HasWeapon('weapon_pistol')"
    ));
    assert_eq!(g.printed(), "equip\tAlice\ndeploy\n");

    // a press fires once, however long the key is held; then it may fire again
    g.printed.borrow_mut().clear();
    g.runtime.set_input(alice, UserCmd { buttons: player::key::ATTACK, ..UserCmd::default() });
    g.runtime.think(0.5);
    g.runtime.set_input(alice, UserCmd::default());
    g.runtime.think(0.015);
    g.runtime.set_input(alice, UserCmd { buttons: player::key::ATTACK, ..UserCmd::default() });
    g.runtime.think(0.015);
    assert_eq!(g.printed(), "bang\t5\nbang\t4\n");
    assert!(g.eval::<bool>("return THINKS > 30 and TEST:Clip1() == 3"));

    // a reload fills the clip from what the player carries
    g.runtime.set_input(alice, UserCmd { buttons: 8192, ..UserCmd::default() });
    g.runtime.think(0.05);
    g.runtime.set_input(alice, UserCmd::default());
    assert!(g.eval::<bool>("return TEST:Clip1() == 5 and Entity(1):GetAmmoCount('SMG1') == 3"));

    // back to the pistol, by its class; dropped; and the rest stripped
    g.printed.borrow_mut().clear();
    assert!(g.eval::<bool>(
        "local p = Entity(1) p:SelectWeapon('weapon_pistol') local pistol = p:GetActiveWeapon()
         p:DropWeapon(pistol)
         local dropped = pistol:GetOwner() == NULL and #p:GetWeapons() == 1 and p:GetActiveWeapon() == NULL
         p:StripWeapons()
         return pistol:GetClass() == 'weapon_pistol' and dropped and #p:GetWeapons() == 0 and TEST:IsMarkedForDeletion()"
    ));
    assert_eq!(g.printed(), "holster\tweapon_pistol\n");

    // ammo types the game adds come after the engine's
    assert!(g.eval::<bool>(
        "function game.BuildAmmoTypes() return { { name = 'laser', dmgtype = DMG_ENERGYBEAM, maxcarry = 50 } } end
         local p = Entity(1)
         return game.GetAmmoID('laser') == 38 and game.GetAmmoTypes()[38] == 'laser' and p:GiveAmmo(20000, 'laser') == 9999
             and p:GetAmmo()[38] == 9999 and game.GetAmmoID('nothing') == -1"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn traces_through_the_world_and_entities() {
    let g = player_game();
    g.runtime.join(alice()).unwrap();
    // the floor, the wall and its material
    assert!(g.eval::<bool>(
        "local t = util.TraceLine({ start = Vector(-300, 0, 100), endpos = Vector(-300, 0, -100) })
         local w = util.TraceLine({ start = Vector(0, 50, 50), endpos = Vector(500, 50, 50) })
         return t.Hit and t.HitWorld and t.Entity == game.GetWorld() and math.abs(t.HitPos.z) < 0.1 and t.HitNormal == Vector(0, 0, 1)
             and math.abs(t.Fraction - 0.5) < 0.01 and w.HitTexture == 'TEST/WALL' and math.abs(w.HitPos.x - 100) < 0.1
             and util.PointContents(Vector(0, 0, -10)) == CONTENTS_SOLID and util.IsInWorld(Vector(0, 0, 10))
             and not util.IsInWorld(Vector(0, 0, -10))"
    ));
    // a solid prop's box; the filter lets traces through it
    assert!(g.eval::<bool>(
        "local prop = ents.Create('prop_physics') prop:SetModel('models/box.mdl') prop:SetPos(Vector(50, -50, 0)) prop:SetSolid(SOLID_BBOX)
         local line = { start = Vector(0, -50, 8), endpos = Vector(90, -50, 8) }
         local t = util.TraceLine(line)
         line.filter = prop local through = util.TraceLine(line)
         line.filter = function(e) return e ~= prop end local by_function = util.TraceLine(line)
         local hull = util.TraceHull({ start = Vector(0, -50, 8), endpos = Vector(90, -50, 8), mins = Vector(-4, -4, -4), maxs = Vector(4, 4, 4) })
         return t.Entity == prop and t.HitNonWorld and math.abs(t.HitPos.x - 42) < 0.1 and t.HitTexture == '**studio**'
             and not through.Hit and not by_function.Hit and hull.Entity == prop and math.abs(hull.HitPos.x - 38) < 0.1"
    ));
    // collision groups: a player's movement goes through a weapon, a trace does not; debris is
    // hit only by a mask with CONTENTS_DEBRIS
    assert!(g.eval::<bool>(
        "local w = ents.Create('weapon_pistol') w:SetModel('models/box.mdl') w:SetPos(Vector(-200, 0, 0)) w:Spawn() w:SetSolid(SOLID_BBOX)
         local line = { start = Vector(-250, 0, 8), endpos = Vector(-150, 0, 8) }
         local hit = util.TraceLine(line)
         line.collisiongroup = COLLISION_GROUP_PLAYER_MOVEMENT local walked = util.TraceLine(line)
         local was = w:GetCollisionGroup()
         w:SetCollisionGroup(COLLISION_GROUP_DEBRIS) line.collisiongroup = nil local debris = util.TraceLine(line)
         line.mask = MASK_SHOT local shot = util.TraceLine(line)
         return was == COLLISION_GROUP_WEAPON and hit.Entity == w and not walked.Hit and not debris.Hit and shot.Entity == w"
    ));
    // what the player looks at, the player left out
    assert!(g.eval::<bool>(
        "local p = Entity(1) p:SetEyeAngles(Angle(90, 0, 0))
         local t = p:GetEyeTrace()
         return t.HitWorld and t.HitPos:Distance(p:GetPos()) < 1 and p:GetAimVector():Distance(Vector(0, 0, -1)) < 0.001"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn what_a_player_types_and_what_is_shown_of_them() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime
        .run_string(
            "HOOKS.PlayerNoClip = function(p, on) print('noclip', on) return true end
             ALLOW_SUICIDE = false HOOKS.CanPlayerSuicide = function(p) return ALLOW_SUICIDE end
             concommand = { Run = function(p, cmd, args, line) print(p:Nick(), cmd, #args, line) end }
             AddConsoleCommand('whoami')
             weapons.list.weapon_test = {
                 PrintName = 'Test', Slot = 3,
                 Primary = { ClipSize = 5, DefaultClip = 5, Ammo = 'SMG1' }, Secondary = { ClipSize = -1, DefaultClip = -1, Ammo = 'none' },
             }",
            "setup",
        )
        .unwrap();

    // the commands of Lua get the player who typed them
    g.runtime.run_player_command(alice, "whoami a b; noclip");
    assert_eq!(g.printed(), "Alice\twhoami\t2\ta b\nnoclip\ttrue\n");
    assert!(g.eval::<bool>("return Entity(1):GetMoveType() == MOVETYPE_NOCLIP"));
    g.runtime.run_player_command(alice, "noclip");
    assert!(g.eval::<bool>("return Entity(1):GetMoveType() == MOVETYPE_WALK"));

    // what is shown: the eyes, between the ticks, and the weapon in hand
    g.runtime.run_player_command(alice, "give weapon_test");
    let view = g.runtime.view(alice).unwrap();
    assert_eq!(view.eye, g.runtime.position(alice).unwrap() + vector::Vector::new(0.0, 0.0, 64.0));
    assert!(view.alive && view.health == 100 && view.angles == vector::Angle::new(0.0, 90.0, 0.0));
    let weapon = view.weapon.unwrap();
    assert_eq!((weapon.class.as_str(), weapon.name.as_str(), weapon.clip, weapon.ammo), ("weapon_test", "Test", 5, 0));
    g.runtime.set_input(alice, walking(90.0, 0));
    g.runtime.think(0.0225);
    let view = g.runtime.view(alice).unwrap();
    assert!(view.previous_eye.y < view.eye.y && (view.previous_eye.x - view.eye.x).abs() < 0.001, "{view:?}");
    assert!((g.runtime.tick_fraction() - 0.5).abs() < 0.01, "{}", g.runtime.tick_fraction());
    g.runtime.set_input(alice, UserCmd::default());

    // choosing a weapon: by slot, one after another, the last one, by class
    let active = |g: &Game| g.eval::<String>("return Entity(1):GetActiveWeapon():GetClass()");
    g.runtime.run_player_command(alice, "invnext");
    assert_eq!(active(&g), "weapon_pistol");
    g.runtime.run_player_command(alice, "slot4");
    assert_eq!(active(&g), "weapon_test");
    g.runtime.run_player_command(alice, "invprev");
    assert_eq!(active(&g), "weapon_pistol");
    g.runtime.run_player_command(alice, "lastinv");
    assert_eq!(active(&g), "weapon_test");
    g.runtime.run_player_command(alice, "use weapon_pistol");
    assert_eq!(active(&g), "weapon_pistol");

    // `kill` asks CanPlayerSuicide
    g.runtime.run_player_command(alice, "kill");
    assert!(g.runtime.view(alice).unwrap().alive);
    g.runtime.run_string("ALLOW_SUICIDE = true", "allow").unwrap();
    g.runtime.run_player_command(alice, "kill");
    assert!(!g.runtime.view(alice).unwrap().alive);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn what_is_drawn_of_the_entities() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime
        .run_string(
            "local function make(model, pos, spawn)
                 local e = ents.Create('prop_physics') e:SetModel(model) e:SetPos(pos) e:SetAngles(Angle(0, 90, 0))
                 if spawn ~= false then e:Spawn() end
                 return e
             end
             BOX = make('models/box.mdl', Vector(10, 20, 30))
             BOX:SetSkin(1) BOX:SetModelScale(2)
             make('models/box.mdl', Vector(0, 0, 0)):SetNoDraw(true)
             make('models/box.mdl', Vector(0, 0, 0)):AddEffects(EF_NODRAW)
             make('models/box.mdl', Vector(0, 0, 0), false)
             make('*1', Vector(0, 0, 0))
             ON_TOP = make('models/box.mdl', Vector(0, 0, 0))
             ON_TOP:SetParent(BOX) ON_TOP:SetLocalPos(Vector(10, 0, 0))
             Entity(1):SetModel('models/box.mdl') Entity(1):GetActiveWeapon():SetModel('models/box.mdl')",
            "props",
        )
        .unwrap();
    let drawn = g.runtime.drawn(alice);
    let found: Vec<(&str, vector::Vector)> = drawn.iter().map(|d| (d.model.as_str(), d.origin)).collect();
    // the hidden, the one not spawned and the brush model are not drawn; neither is Alice nor
    // the pistol in her hands
    assert_eq!(found.len(), 2, "{drawn:?}");
    let first = &drawn[0];
    assert_eq!((first.model.as_str(), first.skin, first.scale), ("models/box.mdl", 1, 2.0));
    assert_eq!((first.origin, first.angles), (vector::Vector::new(10.0, 20.0, 30.0), vector::Angle::new(0.0, 90.0, 0.0)));
    // what is parented is drawn where its parent puts it: turned with it
    let second = &drawn[1];
    assert!((second.origin - vector::Vector::new(10.0, 30.0, 30.0)).length() < 0.001, "{second:?}");
    // others see Alice; neither they nor she see her view model, nor the hands on it
    g.runtime
        .run_string(
            "local vm = Entity(1):GetViewModel() vm:SetModel('models/box.mdl')
             local hands = ents.Create('prop_dynamic') hands:SetModel('models/box.mdl') hands:Spawn() hands:SetParent(vm)",
            "hands",
        )
        .unwrap();
    assert_eq!(g.runtime.drawn(alice).len(), 2);
    let others = g.runtime.drawn(EntityHandle::NULL);
    assert!(others.iter().any(|d| d.entity == alice));
    assert_eq!(others.len(), 4, "{others:?}");

    // the host's console runs Lua
    g.runtime.run_player_command(alice, "lua_run print(GetHostName(), #ents.FindByClass('prop_physics'))");
    assert_eq!(g.printed(), "openGMOD\t6\n");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

/// A server with [`ENTITY_LUA`] on [`player_map`] with these entities more, and two models of
/// a box 16 units wide and high: `models/crate.mdl`, a prop of 25 kg of wood, and
/// `models/doll.mdl`, a ragdoll of two such boxes.
fn physics_game(entities: &[&[(&str, &str)]]) -> Game {
    use gmod_model::testmodel::{self, TestConvex};
    let mdl = testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let part = || vec![TestConvex::cuboid([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0])];
    let crate_phy = testmodel::phy(1, &[part()], false, "solid { \"index\" \"0\" \"mass\" \"25\" \"surfaceprop\" \"wood\" }");
    let doll_phy = testmodel::phy(1, &[part(), part()], false, "");
    let g = game_with(
        Realm::Server,
        &[
            ("lua/includes/init.lua", ENTITY_LUA.as_bytes()),
            ("models/crate.mdl", &mdl),
            ("models/crate.phy", &crate_phy),
            ("models/doll.mdl", &mdl),
            ("models/doll.phy", &doll_phy),
        ],
    );
    g.runtime.init();
    let mut map = player_map();
    map.entities.extend(entities.iter().map(|list| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()));
    g.runtime.spawn_map(&map);
    g
}

#[test]
fn props_fall_and_come_to_rest() {
    let g = physics_game(&[]);
    assert!(g.eval::<bool>(
        "local p = ents.Create('prop_physics') p:SetModel('models/crate.mdl') p:SetPos(Vector(-200, 0, 100)) p:Spawn() CRATE = p
         local phys = p:GetPhysicsObject()
         return phys:IsValid() and phys:GetEntity() == p and math.abs(phys:GetMass() - 25) < 0.01 and phys:GetMaterial() == 'wood'
             and p:GetSolid() == SOLID_VPHYSICS and p:GetMoveType() == MOVETYPE_VPHYSICS and p:GetPhysicsObjectCount() == 1
             and tostring(phys):match('^PhysObject %[Entity %[%d+%]%[prop_physics%]%]$') ~= nil and phys == p:GetPhysicsObject()
             and phys:GetPos() == Vector(-200, 0, 100) and phys:IsMotionEnabled() and not phys:IsAsleep()"
    ));
    // a third of a second in it falls as gravity says, v = g t; it then lies on the floor
    g.runtime.think(1.0 / 3.0);
    assert!(g.eval::<bool>("return math.abs(CRATE:GetVelocity().z + 200) < 15 and CRATE:GetPos().z < 100"));
    g.runtime.think(4.0);
    assert!(g.eval::<bool>(
        "local phys = CRATE:GetPhysicsObject()
         return math.abs(CRATE:GetPos().z) < 0.5 and CRATE:GetAngles():IsEqualTol(Angle(0, 0, 0), 0.5) and phys:IsAsleep()"
    ));
    // the server's gravity
    assert!(g.eval::<bool>(
        "RunConsoleCommand('sv_gravity', '0') CRATE:SetPos(Vector(-200, 0, 100)) CRATE:GetPhysicsObject():Wake() return true"
    ));
    g.runtime.think(1.0);
    assert!(g.eval::<bool>("return math.abs(CRATE:GetPos().z - 100) < 0.01"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn physics_objects() {
    let g = physics_game(&[]);
    assert!(g.eval::<bool>(
        "local p = ents.Create('prop_physics') p:SetModel('models/crate.mdl') p:SetPos(Vector(-200, 0, 100)) p:Spawn() CRATE = p
         PHYS = p:GetPhysicsObject()
         -- the entity moved, the body goes along
         p:SetPos(Vector(-300, 0, 200)) p:SetAngles(Angle(0, 90, 0))
         local moved = PHYS:GetPos() == Vector(-300, 0, 200) and PHYS:GetAngles():IsEqualTol(Angle(0, 90, 0), 0.01)
         PHYS:SetPos(Vector(-300, 0, 150))
         local back = p:GetPos() == Vector(-300, 0, 150)
         local mins, maxs = PHYS:GetAABB()
         PHYS:EnableMotion(false)
         return moved and back and mins == Vector(-8, -8, 0) and maxs == Vector(8, 8, 16) and not PHYS:IsMotionEnabled()
             and PHYS:LocalToWorld(Vector(8, 0, 0)):IsEqualTol(Vector(-300, 8, 150), 0.01)
             and PHYS:WorldToLocal(Vector(-300, 8, 150)):IsEqualTol(Vector(8, 0, 0), 0.01)
             and math.abs(PHYS:GetVolume() - 16 * 16 * 16) < 1"
    ));
    g.runtime.think(1.0);
    // frozen, it stays; a push of 2500 kg·units/s gives its 25 kg 100 units/s
    assert!(g.eval::<bool>(
        "local stayed = CRATE:GetPos() == Vector(-300, 0, 150)
         PHYS:EnableMotion(true) PHYS:EnableGravity(false) PHYS:Wake()
         PHYS:ApplyForceCenter(Vector(0, 0, 2500))
         local pushed = PHYS:GetVelocity():IsEqualTol(Vector(0, 0, 100), 0.01)
         PHYS:SetVelocity(Vector(10, 0, 0)) PHYS:SetAngleVelocity(Vector(0, 0, 90))
         return stayed and pushed and not PHYS:IsGravityEnabled() and PHYS:GetVelocity():IsEqualTol(Vector(10, 0, 0), 0.01)
             and PHYS:GetAngleVelocity():IsEqualTol(Vector(0, 0, 90), 0.01)"
    ));
    g.runtime.think(1.0);
    assert!(g.eval::<bool>(
        "return CRATE:GetPos():IsEqualTol(Vector(-290, 0, 150), 0.5) and math.abs(math.abs(CRATE:GetAngles().y) - 180) < 1
             and CRATE:GetVelocity():IsEqualTol(Vector(10, 0, 0), 0.01)"
    ));
    // a box and a ball of Lua's; a prop without a physics model has no body
    assert!(g.eval::<bool>(
        "local b = ents.Create('prop_physics') b:SetPos(Vector(-400, 0, 50))
         local boxed = b:PhysicsInitBox(Vector(-4, -4, -4), Vector(4, 4, 4)) and b:GetPhysicsObject():IsValid()
         local s = ents.Create('prop_physics') s:SetPos(Vector(-400, 100, 50))
         local ball = s:PhysicsInitSphere(8) and s:GetPhysicsObject():IsValid()
         local none = ents.Create('prop_physics') none:SetModel('models/none.mdl') none:Spawn()
         return boxed and ball and b:GetSolid() == SOLID_VPHYSICS and not none:GetPhysicsObject():IsValid()
             and not none:PhysicsInit(SOLID_VPHYSICS)"
    ));
    // a NULL physics object says so, and one whose entity is gone is NULL
    assert!(g.eval::<bool>(
        "local null = ents.Create('info_target'):GetPhysicsObject()
         local ok, err = pcall(null.GetMass, null)
         CRATE:Remove()
         local gone, err2 = pcall(PHYS.Wake, PHYS)
         return not null:IsValid() and tostring(null) == '[NULL PhysObj]' and not ok and tostring(err):find('Tried to use a NULL physics object!', 1, true)
             and not PHYS:IsValid() and not gone and tostring(err2):find('NULL physics object', 1, true)"
    ));
    // what sandbox asks of a model before it spawns one
    assert!(g.eval::<bool>(
        "return util.IsValidProp('models/crate.mdl') and not util.IsValidRagdoll('models/crate.mdl')
             and util.IsValidRagdoll('models/doll.mdl') and not util.IsValidProp('models/doll.mdl') and not util.IsValidProp('models/none.mdl')"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn props_of_the_map() {
    let g = physics_game(&[
        &[
            ("classname", "prop_physics"),
            ("targetname", "frozen"),
            ("model", "models/crate.mdl"),
            ("origin", "-200 0 100"),
            ("spawnflags", "8"),
        ],
        &[
            ("classname", "prop_physics_multiplayer"),
            ("targetname", "asleep"),
            ("model", "models/crate.mdl"),
            ("origin", "-300 0 100"),
            ("spawnflags", "1"),
        ],
        &[
            ("classname", "prop_physics_override"),
            ("targetname", "falls"),
            ("model", "models/crate.mdl"),
            ("origin", "-400 0 100"),
            ("angles", "0 45 0"),
        ],
    ]);
    g.runtime.think(3.0);
    assert!(g.eval::<bool>(
        "local frozen, asleep, falls = ents.FindByName('frozen')[1], ents.FindByName('asleep')[1], ents.FindByName('falls')[1]
         return frozen:GetPos() == Vector(-200, 0, 100) and not frozen:GetPhysicsObject():IsMotionEnabled()
             and asleep:GetPos() == Vector(-300, 0, 100) and asleep:GetPhysicsObject():IsAsleep()
             and math.abs(falls:GetPos().z) < 0.5 and math.abs(falls:GetAngles().y - 45) < 0.5"
    ));
    // woken, it falls too
    g.runtime.run_string("ents.FindByName('asleep')[1]:GetPhysicsObject():Wake()", "wake").unwrap();
    g.runtime.think(3.0);
    assert!(g.eval::<bool>("return math.abs(ents.FindByName('asleep')[1]:GetPos().z) < 0.5"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

/// Looking along `pitch` and `yaw` with these keys held.
fn aiming(pitch: f32, yaw: f32, buttons: u32) -> UserCmd {
    UserCmd { view_angles: vector::Angle::new(pitch, yaw, 0.0), buttons, ..UserCmd::default() }
}

#[test]
fn the_physics_gun() {
    use player::key::{ATTACK, ATTACK2, RELOAD, USE};
    let g = physics_game(&[]);
    g.runtime
        .run_string(
            "HOOKS.PlayerSelectSpawn = function(p) return ents.FindByClass('info_player_start')[1] end
             HOOKS.PlayerSpawn = function(p) p:Give('weapon_physgun') end
             HOOKS.PhysgunPickup = function(p, e) return not e:IsPlayer() and not e.NoPickup end
             HOOKS.OnPhysgunPickup = function(p, e) print('picked', e:GetName()) end
             HOOKS.PhysgunDrop = function(p, e) print('dropped', e:GetName()) end
             HOOKS.OnPhysgunFreeze = function(w, phys, e, p) phys:EnableMotion(false) print('froze', w:GetClass(), e:GetName()) end
             HOOKS.OnPhysgunReload = function(w, p) print('reload', w:GetClass(), p:Nick()) end
             local function crate(name, pos) local c = ents.Create('prop_physics') c:SetModel('models/crate.mdl') c:SetName(name) c:SetPos(pos) c:Spawn() return c end
             CRATE = crate('crate', Vector(0, 100, 0))
             crate('heavy', Vector(100, 0, 0)).NoPickup = true",
            "physgun",
        )
        .unwrap();
    let alice = g.runtime.join(alice()).unwrap();
    assert_eq!(g.runtime.view(alice).unwrap().weapon.map(|w| w.class), Some("weapon_physgun".to_string()));
    // the crate's near side, 8 units up: 92 units along y and 52 down from the eyes at 64
    let down = (52.0f32 / 92.0).atan().to_degrees();
    g.runtime.set_input(alice, aiming(down, 90.0, ATTACK));
    g.runtime.think(0.02);
    assert_eq!(g.printed(), "picked\tcrate\n");
    assert!(g.runtime.view(alice).unwrap().holding);
    // looking straight ahead it comes up there, by the point it was taken at
    g.runtime.set_input(alice, aiming(0.0, 90.0, ATTACK));
    g.runtime.think(1.0);
    let far = (92.0f32 * 92.0 + 52.0 * 52.0).sqrt();
    assert!(g.eval::<bool>(&format!(
        "return CRATE:GetPos():IsEqualTol(Vector(0, {} + 8, 64 - 12), 1) and CRATE:GetAngles():IsEqualTol(Angle(0, 0, 0), 1)",
        far
    )));
    // turning round, it goes round with the player and turns as they turn
    g.runtime.set_input(alice, aiming(0.0, 180.0, ATTACK));
    g.runtime.think(1.0);
    assert!(g.eval::<bool>(&format!(
        "return CRATE:GetPos():IsEqualTol(Vector(-{far} - 8, 0, 52), 1) and math.abs(CRATE:GetAngles().y - 90) < 1"
    )));
    // the wheel takes it farther; with E the mouse turns it, and the view stays
    g.runtime.set_input(alice, UserCmd { mouse_wheel: 2, ..aiming(0.0, 180.0, ATTACK) });
    g.runtime.think(1.0);
    assert!(g.eval::<bool>(&format!("return CRATE:GetPos():IsEqualTol(Vector(-{far} - 48, 0, 52), 1)")));
    g.runtime.set_input(alice, UserCmd { mouse_x: 50, ..aiming(0.0, 200.0, ATTACK | USE) });
    g.runtime.think(1.0);
    assert_eq!(g.runtime.view(alice).unwrap().angles, vector::Angle::new(0.0, 180.0, 0.0));
    assert!(g.eval::<bool>("return math.abs(CRATE:GetAngles().y - 80) < 1"), "{}", g.eval::<String>("return tostring(CRATE:GetAngles())"));
    // the right button freezes it there and lets go; the gun takes nothing until the left
    // one comes up
    g.runtime.set_input(alice, aiming(0.0, 180.0, ATTACK | ATTACK2));
    g.runtime.think(0.5);
    assert_eq!(g.printed(), "picked\tcrate\nfroze\tweapon_physgun\tcrate\ndropped\tcrate\n");
    assert!(!g.runtime.view(alice).unwrap().holding);
    assert!(g.eval::<bool>("return not CRATE:GetPhysicsObject():IsMotionEnabled() and CRATE:GetPos().z > 40"));
    // R asks the gamemode to unfreeze; what the hook will not let go is not taken
    g.runtime.set_input(alice, aiming(0.0, 180.0, RELOAD));
    g.runtime.think(0.02);
    let heavy_down = (56.0f32 / 92.0).atan().to_degrees();
    g.runtime.set_input(alice, aiming(heavy_down, 0.0, ATTACK));
    g.runtime.think(0.02);
    assert_eq!(g.printed(), "picked\tcrate\nfroze\tweapon_physgun\tcrate\ndropped\tcrate\nreload\tweapon_physgun\tAlice\n");
    // taken again, it moves again, and is dropped when the player dies
    g.runtime.set_input(alice, aiming(0.0, 180.0, 0));
    g.runtime.think(0.02);
    g.runtime.set_input(alice, aiming(0.0, 180.0, ATTACK));
    g.runtime.think(0.02);
    assert!(g.eval::<bool>("return CRATE:GetPhysicsObject():IsMotionEnabled() and CRATE:GetPhysicsObject():IsGravityEnabled() == false"));
    g.runtime.run_string("Entity(1):Kill()", "kill").unwrap();
    g.runtime.think(0.02);
    assert!(g.printed().ends_with("picked\tcrate\ndropped\tcrate\n"), "{}", g.printed());
    assert!(g.eval::<bool>("return CRATE:GetPhysicsObject():IsGravityEnabled()"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn effects_and_persistence() {
    let g = entity_game(Realm::Server);
    assert!(g.eval::<bool>(
        "local e = EffectData() e:SetOrigin(Vector(1, 2, 3)) e:SetScale(2) e:SetEntity(Entity(0)) e:SetFlags(3) e:SetAngles(Angle(0, 90, 0))
         util.Effect('phys_freeze', e, true, true)
         return e:GetOrigin() == Vector(1, 2, 3) and e:GetScale() == 2 and e:GetEntity() == Entity(0) and e:GetFlags() == 3
             and e:GetAngles() == Angle(0, 90, 0) and e:GetNormal() == Vector(0, 0, 0) and e:GetEntIndex() == 0
             and getmetatable(e).MetaName == 'CEffectData' and not pcall(util.Effect, 'phys_freeze')"
    ));
    assert!(g.eval::<bool>(
        "local p = ents.Create('prop_physics') local was = p:GetPersistent() p:SetPersistent(true) return not was and p:GetPersistent()"
    ));
}

#[test]
fn constraints_hold_bodies_together() {
    // a crate of the map welded to the world by name
    let g = physics_game(&[
        &[("classname", "prop_physics"), ("targetname", "hung"), ("model", "models/crate.mdl"), ("origin", "-300 300 100")],
        &[("classname", "phys_constraint"), ("attach1", "hung"), ("origin", "-300 300 100")],
    ]);
    // the world's body is there to join to, and does not move
    assert!(g.eval::<bool>(
        "local world = game.GetWorld() WPHYS = world:GetPhysicsObject() WPHYS:EnableMotion(true) WPHYS:SetPos(Vector(0, 0, 50))
         return WPHYS:IsValid() and WPHYS:GetEntity() == world and world:GetPhysicsObjectNum(0) == WPHYS and not WPHYS:IsMoveable()
             and WPHYS:GetPos() == Vector(0, 0, 0)"
    ));
    // welded to the world a crate stays in the air, and one on a rope from it hangs below;
    // a weld that takes 5 kg breaks under 25, one that takes 100 does not
    assert!(g.eval::<bool>(
        "local function crate(pos) local c = ents.Create('prop_physics') c:SetModel('models/crate.mdl') c:SetPos(pos) c:Spawn() return c end
         local function constraint(class, first, second, keys, pos)
             local c = ents.Create(class) if pos then c:SetPos(pos) end
             for k, v in pairs(keys) do c:SetKeyValue(k, v) end
             c:SetPhysConstraintObjects(first:GetPhysicsObject(), second:GetPhysicsObject()) c:Spawn() c:Activate()
             return c
         end
         local world = game.GetWorld()
         A = crate(Vector(-200, 0, 100)) B = crate(Vector(-200, 0, 60))
         WELD = constraint('phys_constraint', world, A, {})
         ROPE = constraint('phys_lengthconstraint', A, B, { attachpoint = tostring(Vector(-200, 0, 76)), length = 40 }, Vector(-200, 0, 100))
         C = crate(Vector(-100, -300, 100)) WEAK = constraint('phys_constraint', world, C, { forcelimit = 5 })
         D = crate(Vector(-100, -200, 100)) STRONG = constraint('phys_constraint', world, D, { forcelimit = 100 })
         local first, second = WELD:GetConstrainedEntities()
         return WELD:IsConstraint() and ROPE:IsConstraint() and first == world and second == A
             and not ents.Create('info_target'):IsConstraint()"
    ));
    g.runtime.think(2.0);
    assert!(g.eval::<bool>(
        "return A:GetPos():IsEqualTol(Vector(-200, 0, 100), 0.5) and B:GetPos():IsEqualTol(Vector(-200, 0, 44), 1)
             and ents.FindByName('hung')[1]:GetPos():IsEqualTol(Vector(-300, 300, 100), 0.5)
             and not WEAK:IsValid() and C:GetPos().z < 1 and STRONG:IsValid() and D:GetPos():IsEqualTol(Vector(-100, -200, 100), 0.5)"
    ));
    // broken, the weld is gone and they fall together; two that do not collide go through
    // each other
    assert!(g.eval::<bool>(
        "local function crate(pos) local c = ents.Create('prop_physics') c:SetModel('models/crate.mdl') c:SetPos(pos) c:Spawn() return c end
         WELD:Fire('Break')
         F = crate(Vector(-100, 300, 0)) F:GetPhysicsObject():EnableMotion(false)
         G = crate(Vector(-100, 300, 50))
         local pair = ents.Create('logic_collision_pair') pair:SetKeyValue('startdisabled', 1)
         pair:SetPhysConstraintObjects(F:GetPhysicsObject(), G:GetPhysicsObject()) pair:Spawn() pair:Activate() pair:Input('DisableCollisions')
         return WELD:IsValid()"
    ));
    g.runtime.think(2.0);
    assert!(g.eval::<bool>(
        "return not WELD:IsValid() and ROPE:IsValid() and A:GetPos().z < 50 and B:GetPos().z < 1 and math.abs(G:GetPos().z) < 1"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn inputs_of_entities() {
    let g = entity_game(Realm::Server);
    assert!(g.eval::<bool>(
        "HOOKS.AcceptInput = function(e, input, activator, caller, value)
             print('input', e:GetName(), input, value, activator == CALLER)
             if e:GetName() == 'kept' and input == 'Kill' then return true end
         end
         CALLER = ents.Create('info_target')
         A = ents.Create('info_target') A:SetName('a')
         KEPT = ents.Create('info_target') KEPT:SetName('kept')
         CHILD = ents.Create('info_target') CHILD:SetParent(A)
         A:Input('AddOutput', CALLER, CALLER, 'targetname renamed')
         A:Fire('Color', '255 0 0')
         KEPT:Fire('Kill')
         A:Fire('KillHierarchy', nil, 0.5)
         local r, g, b = A:GetColor4Part()
         return A:GetName() == 'renamed' and g == 255"
    ));
    assert_eq!(g.printed(), "input\ta\tAddOutput\ttargetname renamed\ttrue\n");
    g.runtime.think(0.02);
    assert!(g.eval::<bool>(
        "local r, g, b, a = A:GetColor4Part() return r == 255 and g == 0 and b == 0 and a == 255 and KEPT:IsValid() and A:IsValid()"
    ));
    g.runtime.think(0.6);
    assert!(g.eval::<bool>("return not A:IsValid() and not CHILD:IsValid() and KEPT:IsValid()"));
    assert_eq!(
        g.printed(),
        "input\ta\tAddOutput\ttargetname renamed\ttrue\ninput\trenamed\tColor\t255 0 0\tfalse\ninput\tkept\tKill\t\tfalse\n\
         input\trenamed\tKillHierarchy\t\tfalse\n"
    );
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn what_a_players_client_would_say() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    // what a client has not set is the default, or the default of a sandbox tool's setting
    assert!(g.eval::<bool>(
        "weapons.list.gmod_tool = { Tool = { weld = { ClientConVar = { forcelimit = '0', nocollide = 1 } }, rope = {} } }
         local p = Entity(1)
         return p:GetInfo('gmod_toolmode') == 'rope' and p:GetInfo('weld_forcelimit') == '0' and p:GetInfoNum('weld_nocollide', 5) == 1
             and p:GetInfo('nope') == '' and p:GetInfoNum('nope', 5) == 5"
    ));
    // typed in the console, or run by `ConCommand`, it is set
    g.runtime.run_player_command(alice, "gmod_toolmode weld; weld_forcelimit 1000; gmod_toolmode");
    g.runtime.run_string("Entity(1):ConCommand('cl_playerskin 3; finger_0 1 2')", "concommand").unwrap();
    assert!(g.eval::<bool>(
        "local p = Entity(1)
         return p:GetInfo('gmod_toolmode') == 'weld' and p:GetInfoNum('weld_forcelimit', 0) == 1000 and p:GetInfoNum('cl_playerskin', 0) == 3"
    ));
    assert_eq!(g.printed(), "\"gmod_toolmode\" = \"weld\"\nUnknown command \"finger_0\"\n");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn what_scripts_ask_of_entities() {
    let g = entity_game(Realm::Server);
    // GMod's `table.insert` says where; `ProtectedCall` reports and goes on
    assert!(g.eval::<bool>(
        "local t = {} local first, second = table.insert(t, 'a'), table.insert(t, 1, 'b')
         local ok, err = pcall(table.insert, t)
         return first == 1 and second == 1 and t[1] == 'b' and t[2] == 'a' and not ok and tostring(err):find('wrong number', 1, true)
             and ProtectedCall(function(a) assert(a == 1) end, 1) and not ProtectedCall(error, 'on purpose')"
    ));
    assert_eq!(g.messages(), ["on purpose"]);
    // the methods that give more than one value; the world's spaces
    assert!(g.eval::<bool>(
        "local e = ents.Create('prop_physics') e:SetModel('models/box.mdl') e:SetPos(Vector(10, 0, 0)) e:SetAngles(Angle(0, 90, 0)) E = e
         local mins, maxs = e:GetCollisionBounds()
         local lo, hi = e:GetModelBounds()
         e:SetColor4Part(1, 2, 3, 4)
         local r, g, b, a = e:GetColor4Part()
         local pos, ang = LocalToWorld(Vector(1, 0, 0), Angle(0, 10, 0), Vector(10, 0, 0), Angle(0, 90, 0))
         local back, back_ang = WorldToLocal(pos, ang, Vector(10, 0, 0), Angle(0, 90, 0))
         local small, big = Vector(1, 5, -1), Vector(2, -5, 3) OrderVectors(small, big)
         return mins == Vector(-8, -8, 0) and maxs == Vector(8, 8, 16) and lo == mins and hi == maxs and r == 1 and g == 2 and b == 3 and a == 4
             and pos:IsEqualTol(Vector(10, 1, 0), 0.001) and ang:IsEqualTol(Angle(0, 100, 0), 0.01)
             and back:IsEqualTol(Vector(1, 0, 0), 0.001) and back_ang:IsEqualTol(Angle(0, 10, 0), 0.01)
             and small == Vector(1, -5, -1) and big == Vector(2, 5, 3)
             and e:NearestPoint(Vector(10, 100, 8)):IsEqualTol(Vector(10, 8, 8), 0.001)
             and e:AlignAngles(Angle(0, 90, 0), Angle(0, 0, 0)):IsEqualTol(Angle(0, 0, 0), 0.01)"
    ));
    // what is kept of how it looks: bones, attachments and flexes as the model has them
    assert!(g.eval::<bool>(
        "local e = E
         e:SetSubMaterial(0, 'models/debug/debugwhite') e:SetSubMaterial(1, 'a') e:SetSubMaterial(1, '')
         local sub = e:GetSubMaterial(0) == 'models/debug/debugwhite' and e:GetSubMaterial(1) == ''
         e:SetSubMaterial() local cleared = e:GetSubMaterial(0) == ''
         e:ManipulateBonePosition(0, Vector(1, 2, 3)) local moved = e:HasBoneManipulations() and e:GetManipulateBonePosition(0) == Vector(1, 2, 3)
         e:ManipulateBonePosition(0, Vector(0, 0, 0)) local back = not e:HasBoneManipulations() and e:GetManipulateBoneScale(0) == Vector(1, 1, 1)
         e:SetFlexScale(2) e:SetFlexWeight(3, 0.5) e:SetRenderFX(kRenderFxHologram or 16) e:SetSpawnEffect(true)
         local t = { Type = 'Weld' } e:SetTable(t)
         return sub and cleared and moved and back and e:GetFlexScale() == 2 and e:GetFlexWeight(3) == 0.5 and e:GetRenderFX() == 16
             and e:GetSpawnEffect() and e:GetTable() == t and e.Type == 'Weld'
             and e:GetBoneCount() == 0 and e:LookupBone('root') == nil and e:LookupAttachment('muzzle') == 0 and e:GetFlexNum() == 0
             and ents.Create('info_target'):LookupAttachment('muzzle') == -1 and e:GetWorkshopID() == nil"
    ));
    // sounds are taken and kept quiet
    assert!(g.eval::<bool>(
        "E:EmitSound('Weapon_Pistol.Single') E:StopSound('Weapon_Pistol.Single') EmitSound('a.wav', Vector(), 0)
         sound.Add({ name = 'Test.Sound', sound = 'test.wav', level = 80 })
         local patch = CreateSound(E, 'Test.Sound')
         patch:PlayEx(0.5, 120) local playing = patch:IsPlaying() and patch:GetVolume() == 0.5 and patch:GetPitch() == 120
         patch:Stop()
         return playing and not patch:IsPlaying() and sound.GetProperties('test.sound').level == 80 and sound.GetTable()[1] == 'Test.Sound'
             and E:StartLoopingSound('a.wav') == 1 and E:StartLoopingSound('b.wav') == 2"
    ));
    assert_eq!(g.messages(), ["on purpose"]);
}
