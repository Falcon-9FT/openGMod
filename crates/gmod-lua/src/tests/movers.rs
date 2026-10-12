//! What moves in a map: doors, buttons, brushes turned off, the use key.

use super::*;
use crate::movedata::UserCmd;
use crate::vector::{Angle, Vector};

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A floor, and brush models in their own spaces: `*1` a slab 8 thick, 128 wide and high
/// (a sliding door), `*2` a slab 4 thick and 64 wide from its hinge at its origin (a door that
/// turns), `*3` a small block (a button), `*4` a wall.
fn mover_map(entities: Vec<Vec<(String, String)>>) -> Map {
    use gmod_bsp::testmap::{TestBrush, TestMap};
    let models = vec![
        vec![TestBrush::cuboid([-2048.0, -1024.0, -64.0], [2048.0, 1024.0, 0.0])],
        vec![TestBrush::cuboid([200.0, -64.0, 0.0], [208.0, 64.0, 128.0])],
        vec![TestBrush::cuboid([0.0, 0.0, 0.0], [4.0, 64.0, 96.0])],
        vec![TestBrush::cuboid([400.0, -8.0, 40.0], [404.0, 8.0, 56.0])],
        vec![TestBrush::cuboid([600.0, -64.0, 0.0], [608.0, 64.0, 128.0])],
    ];
    let mut all = vec![
        pairs(&[("classname", "worldspawn")]),
        pairs(&[("classname", "info_player_start"), ("origin", "0 0 0"), ("angles", "0 0 0")]),
        pairs(&[("classname", "info_target"), ("targetname", "rec")]),
    ];
    all.extend(entities);
    // the map's brush entities in its entity lump, as the compiler leaves them
    let lump: String =
        all.iter().map(|e| format!("{{\n{}}}\n", e.iter().map(|(k, v)| format!("\"{k}\" \"{v}\"\n")).collect::<String>())).collect();
    let test = TestMap { models, entities: lump, ..TestMap::default() };
    let bsp = gmod_bsp::Bsp::parse("test.bsp".into(), Arc::from(test.bytes())).unwrap();
    Map::from_bsp("gm_movers", &bsp, Arc::new(gmod_bsp::Collision::new(&bsp)))
}

fn mover_game(map: Map) -> Game {
    use gmod_model::testmodel::{self, TestConvex};
    let mdl = testmodel::mdl(1, ([0.0, 0.0, 0.0], [4.0, 48.0, 96.0]));
    // crates 32 wide, a light one and a heavy one
    let crate_mdl = testmodel::mdl(2, ([-16.0, -16.0, 0.0], [16.0, 16.0, 32.0]));
    let shape = [vec![TestConvex::cuboid([-16.0, -16.0, 0.0], [16.0, 16.0, 32.0])]];
    let light = testmodel::phy(2, &shape, false, "solid { \"index\" \"0\" \"mass\" \"20\" }");
    let heavy = testmodel::phy(2, &shape, false, "solid { \"index\" \"0\" \"mass\" \"800\" }");
    let g = game_with(
        Realm::Server,
        &[
            ("lua/includes/init.lua", ENTITY_LUA.as_bytes()),
            ("models/door.mdl", &mdl),
            ("models/light.mdl", &crate_mdl),
            ("models/light.phy", &light),
            ("models/heavy.mdl", &crate_mdl),
            ("models/heavy.phy", &heavy),
            ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes()),
        ],
    );
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime
        .run_string(
            "HOOKS.AcceptInput = function(ent, input, activator, caller, param)
                 if ent:GetName() == 'rec' then print(input) end
             end
             function blocked(x, z) return math.floor(util.TraceLine({ start = Vector(x, 0, z), endpos = Vector(x + 600, 0, z), filter = Entity(1) }).HitPos.x + 0.5) end",
            "rec",
        )
        .unwrap();
    g.runtime.spawn_map(&map);
    g
}

/// Lines printed since the last call.
fn taken(g: &Game, seen: &mut usize) -> Vec<String> {
    let printed = g.printed();
    let new = printed[*seen..].lines().map(str::to_string).collect();
    *seen = printed.len();
    new
}

#[test]
fn doors_slide_and_close_by_themselves() {
    let g = mover_game(mover_map(vec![pairs(&[
        ("classname", "func_door"),
        ("model", "*1"),
        ("targetname", "door"),
        ("movedir", "-90 0 0"),
        ("speed", "128"),
        ("wait", "2"),
        ("OnOpen", "rec,Opening,,0,-1"),
        ("OnFullyOpen", "rec,Open,,0,-1"),
        ("OnFullyClosed", "rec,Closed,,0,-1"),
    ])]));
    let mut seen = 0;
    g.runtime.think(0.1);
    assert_eq!(g.eval::<f32>("return blocked(100, 64)"), 200.0, "closed, it stops a trace");
    let drawn = g.runtime.drawn(EntityHandle::NULL);
    assert!(drawn.iter().any(|d| d.model == "*1"), "drawn where its entity is: {drawn:?}");
    g.runtime.run_string("ents.FindByName('door')[1]:Fire('Open')", "open").unwrap();
    g.runtime.think(0.5);
    let z = g.eval::<f32>("return ents.FindByName('door')[1]:GetPos().z");
    assert!((55.0..75.0).contains(&z), "half way up at 128 a second: {z}");
    g.runtime.think(1.0);
    assert_eq!(taken(&g, &mut seen), ["Opening", "Open"]);
    assert!(g.eval::<f32>("return blocked(100, 64)") > 690.0, "open, a trace goes under it");
    // two seconds open, then down again
    g.runtime.think(3.5);
    assert_eq!(taken(&g, &mut seen), ["Closed"]);
    assert_eq!(g.eval::<f32>("return blocked(100, 64)"), 200.0);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn the_use_key_presses_buttons_and_opens_doors_away() {
    let g = mover_game(mover_map(vec![
        pairs(&[
            ("classname", "func_button"),
            ("model", "*3"),
            ("spawnflags", "1025"),
            ("wait", "1"),
            ("OnPressed", "rec,Pressed,,0,-1"),
            ("OnPressed", "hinge,Open,,0,-1"),
        ]),
        pairs(&[
            ("classname", "func_door_rotating"),
            ("model", "*2"),
            ("targetname", "hinge"),
            ("origin", "300 0 0"),
            ("speed", "180"),
            ("wait", "-1"),
        ]),
    ]));
    let alice = g.runtime.join(alice()).unwrap();
    let mut seen = 0;
    g.runtime.think(0.1);
    taken(&g, &mut seen);
    // standing before the button, looking at it, pressing the use key for a moment
    let look = Angle::new(17.7, 0.0, 0.0);
    g.runtime.run_string("Entity(1):SetPos(Vector(350, 0, 0)) Entity(1):SetEyeAngles(Angle(17.7, 0, 0))", "go").unwrap();
    g.runtime.set_input(alice, UserCmd { view_angles: look, buttons: 32, ..UserCmd::default() });
    g.runtime.think(0.05);
    g.runtime.set_input(alice, UserCmd { view_angles: look, ..UserCmd::default() });
    g.runtime.think(1.0);
    assert_eq!(taken(&g, &mut seen), ["Pressed"]);
    // the door turned away from the player, who stands on its +x side: it swings to -x
    let yaw = g.eval::<f32>("return ents.FindByName('hinge')[1]:GetAngles().y");
    assert!((yaw - 90.0).abs() < 0.5, "{yaw}");
    // closed again from the other side, it opens the other way
    g.runtime.run_string("ents.FindByName('hinge')[1]:Fire('Close')", "back").unwrap();
    g.runtime.think(1.0);
    g.runtime
        .run_string("Entity(1):SetPos(Vector(250, 32, 0)) ents.FindByName('hinge')[1]:Fire('Open', '', 0, Entity(1))", "again")
        .unwrap();
    g.runtime.think(1.0);
    let yaw = g.eval::<f32>("return ents.FindByName('hinge')[1]:GetAngles().y");
    assert!((yaw - 270.0).abs() < 0.5 || (yaw + 90.0).abs() < 0.5, "{yaw}");
    // a scripted entity is used: once for SIMPLE_USE, every tick held for the default
    g.runtime
        .run_string(
            "local ENT = { Type = 'anim' }
             function ENT:Initialize() self:SetSolid(SOLID_BBOX) self:SetCollisionBounds(Vector(-8, -8, 0), Vector(8, 8, 16)) end
             function ENT:Use(activator, caller, kind, value) self.uses = (self.uses or 0) + 1 USER = activator end
             scripted_ents.Register(ENT, 'sent_knob')
             KNOB = ents.Create('sent_knob') KNOB:SetPos(Vector(-60, 0, 40)) KNOB:Spawn()
             Entity(1):SetPos(Vector(0, 0, 0)) Entity(1):SetEyeAngles(Angle(20, 180, 0))
             HOOKS.PlayerUse = function(p, e) print('use', e:GetClass()) end",
            "knob",
        )
        .unwrap();
    let look = Angle::new(20.0, 180.0, 0.0);
    g.runtime.set_input(alice, UserCmd { view_angles: look, buttons: 32, ..UserCmd::default() });
    g.runtime.think(0.3);
    g.runtime.set_input(alice, UserCmd { view_angles: look, ..UserCmd::default() });
    g.runtime.think(0.1);
    assert!(taken(&g, &mut seen).iter().any(|l| l == "use\tsent_knob"));
    assert!(g.eval::<bool>("return KNOB.uses > 10 and USER == Entity(1)"), "used every tick while held");
    g.runtime.run_string("KNOB.uses = 0 KNOB:SetUseType(SIMPLE_USE)", "simple").unwrap();
    g.runtime.set_input(alice, UserCmd { view_angles: look, buttons: 32, ..UserCmd::default() });
    g.runtime.think(0.3);
    assert!(g.eval::<bool>("return KNOB.uses == 1"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn brushes_turned_off_spinning_and_gone() {
    let g = mover_game(mover_map(vec![
        pairs(&[("classname", "func_brush"), ("model", "*4"), ("targetname", "wall")]),
        pairs(&[("classname", "func_rotating"), ("model", "*2"), ("targetname", "fan"), ("origin", "-300 0 0"), ("maxspeed", "90")]),
    ]));
    g.runtime.think(0.1);
    assert_eq!(g.eval::<f32>("return blocked(500, 64)"), 600.0);
    g.runtime.run_string("ents.FindByName('wall')[1]:Fire('Disable')", "off").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<f32>("return blocked(500, 64)") > 1090.0, "off: neither solid");
    assert!(!g.runtime.drawn(EntityHandle::NULL).iter().any(|d| d.model == "*4"), "nor seen");
    g.runtime.run_string("ents.FindByName('wall')[1]:Fire('Enable')", "on").unwrap();
    g.runtime.think(0.1);
    assert_eq!(g.eval::<f32>("return blocked(500, 64)"), 600.0);
    g.runtime.run_string("ents.FindByName('wall')[1]:Fire('Kill')", "kill").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<f32>("return blocked(500, 64)") > 1090.0, "gone");
    // a fan turned on spins at its speed
    g.runtime.run_string("ents.FindByName('fan')[1]:Fire('Start')", "start").unwrap();
    g.runtime.think(1.0);
    let yaw = g.eval::<f32>("return ents.FindByName('fan')[1]:GetAngles().y");
    assert!((80.0..100.0).contains(&yaw), "{yaw}");
    let _ = Vector::default();
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn players_push_props_and_ride_what_moves() {
    let g = mover_game(mover_map(vec![
        pairs(&[
            ("classname", "func_door"),
            ("model", "*1"),
            ("targetname", "lift"),
            ("movedir", "-90 0 0"),
            ("speed", "64"),
            ("wait", "-1"),
        ]),
        pairs(&[
            ("classname", "func_movelinear"),
            ("model", "*4"),
            ("targetname", "belt"),
            ("movedir", "0 90 0"),
            ("movedistance", "256"),
            ("speed", "64"),
        ]),
    ]));
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime.think(0.1);
    let walk = |y: f32, seconds: f64| {
        g.runtime.run_string(&format!("Entity(1):SetPos(Vector(40, {y}, 0)) Entity(1):SetEyeAngles(Angle(0, 0, 0))"), "go").unwrap();
        g.runtime.set_input(alice, UserCmd { forward_move: gmod_movement::FULL_MOVE, ..UserCmd::default() });
        g.runtime.think(seconds);
        g.runtime.set_input(alice, UserCmd::default());
        g.runtime.think(0.5);
    };
    // walking into crates: a light one goes along, a heavy one hardly
    g.runtime
        .run_string(
            "LIGHT = ents.Create('prop_physics') LIGHT:SetModel('models/light.mdl') LIGHT:SetPos(Vector(110, -300, 0)) LIGHT:Spawn()
             HEAVY = ents.Create('prop_physics') HEAVY:SetModel('models/heavy.mdl') HEAVY:SetPos(Vector(110, 300, 0)) HEAVY:Spawn()",
            "crates",
        )
        .unwrap();
    g.runtime.think(0.5);
    walk(-300.0, 1.0);
    walk(300.0, 1.0);
    let light = g.eval::<f32>("return LIGHT:GetPos().x - 110");
    let heavy = g.eval::<f32>("return HEAVY:GetPos().x - 110");
    assert!(light > 60.0 && heavy < light / 4.0, "pushed: light {light}, heavy {heavy}");

    // standing on the top of the door when it rises: lifted with it
    g.runtime.run_string("Entity(1):SetPos(Vector(204, 0, 129))", "on").unwrap();
    g.runtime.think(0.3);
    assert!(g.eval::<bool>("return Entity(1):GetGroundEntity() == ents.FindByName('lift')[1]"));
    g.runtime.run_string("ents.FindByName('lift')[1]:Fire('Open')", "up").unwrap();
    g.runtime.think(2.5);
    let at = g.runtime.position(alice).unwrap();
    assert!((at.z - 256.0).abs() < 2.0 && (at.x - 204.0).abs() < 2.0, "lifted: {at:?}");
    // on the belt: carried along y as it goes
    g.runtime.run_string("Entity(1):SetPos(Vector(604, 0, 129))", "on").unwrap();
    g.runtime.think(0.3);
    g.runtime.run_string("ents.FindByName('belt')[1]:Fire('Open')", "go").unwrap();
    g.runtime.think(4.5);
    let at = g.runtime.position(alice).unwrap();
    assert!((at.y - 256.0).abs() < 3.0 && (at.x - 604.0).abs() < 2.0 && at.z > 120.0, "carried: {at:?}");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn doors_push_players_or_go_back() {
    let g = mover_game(mover_map(vec![pairs(&[
        ("classname", "func_door"),
        ("model", "*1"),
        ("targetname", "door"),
        ("movedir", "-90 0 0"),
        ("spawnpos", "1"),
        ("speed", "128"),
        ("wait", "-1"),
        ("dmg", "10"),
        ("OnBlockedClosing", "rec,Blocked,,0,-1"),
        ("OnFullyOpen", "rec,Open,,0,-1"),
    ])]));
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime.think(0.1);
    // a trace that may not hit the door goes on to the wall behind it
    g.runtime.run_string("ents.FindByName('door')[1]:Fire('Close')", "close").unwrap();
    g.runtime.think(1.5);
    assert!(g.eval::<bool>(
        "local door = ents.FindByName('door')[1]
         return util.TraceLine({ start = Vector(100, 0, 64), endpos = Vector(700, 0, 64), filter = door }).HitPos.x > 599"
    ));
    // under the open door as it comes down: it cannot push the player into the floor
    let start = g.printed().len();
    g.runtime.run_string("local d = ents.FindByName('door')[1] d:Fire('Open')", "open").unwrap();
    g.runtime.think(1.5);
    g.runtime.run_string("Entity(1):SetPos(Vector(204, 0, 0)) ents.FindByName('door')[1]:Fire('Close')", "under").unwrap();
    g.runtime.think(1.5);
    let printed = &g.printed()[start..];
    assert!(printed.contains("Blocked") && printed.matches("Open").count() == 2, "{printed}");
    assert!(g.eval::<i64>("return Entity(1):Health()") <= 90);
    let at = g.runtime.position(alice).unwrap();
    assert!(at.z.abs() < 1.0, "still on the floor: {at:?}");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
