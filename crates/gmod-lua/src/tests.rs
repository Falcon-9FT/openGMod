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
    assert!(g.eval::<bool>("return #player.GetAll() == 0 and #ents.GetAll() == 0 and Entity(1) == NULL and not NULL:IsValid()"));
    assert!(g.eval::<bool>("return game.GetWorld():IsValid() and game.GetWorld():IsWorld() and tostring(NULL) == '[NULL Entity]'"));
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
         hook\tOnGamemodeLoaded\ttrue\nentity\tbase_anim\tbase/entities/entities/base_anim\tnil\n\
         entity\tthing\tmine/entities/entities/thing\tbase_anim\nweapon\tgun\ttrue\nhook\tPostGamemodeLoaded\ttrue\n\
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
