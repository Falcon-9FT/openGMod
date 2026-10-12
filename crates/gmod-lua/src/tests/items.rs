//! HL2's items, chargers, props that break and blow up, the map's breakable brushes.

use super::*;
use crate::movedata::UserCmd;
use crate::sound::SoundEvent;
use crate::vector::Angle;

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A floor with its top at 0, a wall from x 100 to 132, and brush models of their own: `*1` a
/// pane at x 300, `*2` one at x 400 (both 4 thick, 128 wide and high), `*3` a block at
/// x -108 above the floor.
fn breakable_map(entities: Vec<Vec<(String, String)>>) -> Map {
    use gmod_bsp::testmap::{TestBrush, TestMap};
    let models = vec![
        vec![
            TestBrush::cuboid([-2048.0, -1024.0, -64.0], [2048.0, 1024.0, 0.0]),
            TestBrush::cuboid([100.0, -1024.0, 0.0], [132.0, 1024.0, 256.0]),
        ],
        vec![TestBrush::cuboid([300.0, -64.0, 0.0], [304.0, 64.0, 128.0])],
        vec![TestBrush::cuboid([400.0, -64.0, 0.0], [404.0, 64.0, 128.0])],
        vec![TestBrush::cuboid([-112.0, -8.0, 48.0], [-104.0, 8.0, 64.0])],
    ];
    let mut all = vec![
        pairs(&[("classname", "worldspawn")]),
        pairs(&[("classname", "info_player_start"), ("origin", "0 0 0"), ("angles", "0 0 0")]),
        pairs(&[("classname", "info_target"), ("targetname", "rec")]),
    ];
    all.extend(entities);
    let lump: String =
        all.iter().map(|e| format!("{{\n{}}}\n", e.iter().map(|(k, v)| format!("\"{k}\" \"{v}\"\n")).collect::<String>())).collect();
    let test = TestMap { models, entities: lump, ..TestMap::default() };
    let bsp = gmod_bsp::Bsp::parse("test.bsp".into(), Arc::from(test.bytes())).unwrap();
    Map::from_bsp("gm_items", &bsp, Arc::new(gmod_bsp::Collision::new(&bsp)))
}

const PROPDATA: &str = r#""PropData.txt"
{
    "Wooden.Small" { "health" "20" "dmg.bullets" "2.0" "dmg.club" "1.5" }
}"#;

const SURFACES: &str = r#"
"default" { "density" "2000" }
"wood" { "density" "700" "break" "Wood.Break" }
"metal" { "density" "2700" "break" "Metal_Box.Break" }
"glass" { "density" "2700" "break" "Glass.Break" }
"#;

/// A server with the items' models (boxes 16 wide with bodies, the chargers without), a crate
/// that breaks, a barrel that blows up, a box that does neither, and Alice in the game.
fn items_game(map: Map) -> (Game, EntityHandle) {
    use gmod_model::testmodel::{self, TestConvex};
    let hull = ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]);
    let mdl = testmodel::mdl(1, hull);
    let part = || vec![vec![TestConvex::cuboid(hull.0, hull.1)]];
    let phy = |material: &str| {
        testmodel::phy(1, &part(), false, &format!("solid {{ \"index\" \"0\" \"mass\" \"20\" \"surfaceprop\" \"{material}\" }}"))
    };
    let (wood, metal) = (phy("wood"), phy("metal"));
    // `$keyvalues` as the compiler leaves them: in a block `mdlkeyvalue`
    let crate_mdl = testmodel::mdl_with_key_values(1, hull, "mdlkeyvalue { prop_data { \"base\" \"Wooden.Small\" } }");
    let barrel = testmodel::mdl_with_key_values(
        1,
        hull,
        "mdlkeyvalue\n{\nprop_data {\n\"base\" \"Wooden.Small\" \"health\" \"1\" \"explosive_damage\" \"100\" \"explosive_radius\" \"256\" }\n}",
    );
    let charger = testmodel::mdl(1, ([-4.0, -16.0, -16.0], [4.0, 16.0, 16.0]));
    let classes = [
        "item_healthkit",
        "item_healthvial",
        "item_battery",
        "item_ammo_pistol",
        "item_ammo_pistol_large",
        "item_ammo_smg1",
        "item_ammo_smg1_large",
        "item_ammo_ar2",
        "item_ammo_ar2_large",
        "item_ammo_357",
        "item_ammo_357_large",
        "item_ammo_crossbow",
        "item_rpg_round",
        "item_ammo_smg1_grenade",
        "item_box_buckshot",
        "item_ammo_ar2_altfire",
    ];
    let mut owned: Vec<(String, Vec<u8>)> = Vec::new();
    for class in classes {
        let model = crate::items::model_of(class).unwrap();
        owned.push((model.to_string(), mdl.clone()));
        owned.push((model.replace(".mdl", ".phy"), metal.clone()));
    }
    for class in ["item_healthcharger", "item_suitcharger"] {
        owned.push((crate::items::model_of(class).unwrap().to_string(), charger.clone()));
    }
    let mut files: Vec<(&str, &[u8])> = owned.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    files.extend([
        ("lua/includes/init.lua", ENTITY_LUA.as_bytes()),
        ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes()),
        ("scripts/propdata.txt", PROPDATA.as_bytes()),
        ("scripts/surfaceproperties_manifest.txt", b"surfaceproperties_manifest { \"file\" \"scripts/surfaceproperties.txt\" }"),
        ("scripts/surfaceproperties.txt", SURFACES.as_bytes()),
        ("models/crate.mdl", &crate_mdl),
        ("models/crate.phy", &wood),
        ("models/barrel.mdl", &barrel),
        ("models/barrel.phy", &metal),
        ("models/box.mdl", &mdl),
        ("models/box.phy", &wood),
    ]);
    let g = game_with(Realm::Server, &files);
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime
        .run_string(
            "for _, name in ipairs({ 'HealthKit.Touch', 'HealthVial.Touch', 'ItemBattery.Touch', 'BaseCombatCharacter.AmmoPickup',
                 'WallHealth.Start', 'WallHealth.Deny', 'Wood.Break', 'Metal_Box.Break', 'Glass.Break', 'BaseExplosionEffect.Sound' }) do
                 sound.Add({ name = name, sound = 'x.wav' })
             end
             function IsValid(e) return e ~= nil and e:IsValid() end
             HOOKS.AcceptInput = function(ent, input) if ent:GetName() == 'rec' then print(input) end end
             function spawn(class, x, y, z, model)
                 local e = ents.Create(class) if model then e:SetModel(model) end e:SetPos(Vector(x, y, z)) e:Spawn() return e
             end
             function blocked(z) return math.floor(util.TraceLine({ start = Vector(200, 0, z), endpos = Vector(600, 0, z), filter = Entity(1) }).HitPos.x + 0.5) end",
            "items",
        )
        .unwrap();
    // the engine's effects, seen where they go
    let lua = g.runtime.lua();
    let log: mlua::Function =
        lua.load("return function(name, data) print('effect', name, math.floor(data:GetMagnitude() + 0.5)) end").eval().unwrap();
    lua.set_named_registry_value(crate::effect::ENGINE, log).unwrap();
    g.runtime.spawn_map(&map);
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime.think(0.1);
    (g, alice)
}

/// Lines printed since the last call.
fn taken(g: &Game, seen: &mut usize) -> Vec<String> {
    let printed = g.printed();
    let new = printed[*seen..].lines().map(str::to_string).collect();
    *seen = printed.len();
    new
}

/// The sounds played since the last call, by name.
fn sounds(g: &Game) -> Vec<String> {
    g.runtime.take_sounds().into_iter().filter_map(|e| if let SoundEvent::Play(p) = e { Some(p.name) } else { None }).collect()
}

#[test]
fn items_are_taken_when_they_are_of_use() {
    let (g, _) = items_game(breakable_map(Vec::new()));
    sounds(&g);
    // a kit away from the player has its model and its body: it falls
    assert!(g.eval::<bool>(
        "AWAY = spawn('item_healthkit', -300, 0, 100)
         return AWAY:GetModel() == 'models/items/healthkit.mdl' and AWAY:GetPhysicsObject():IsValid()"
    ));
    g.runtime.think(0.5);
    assert!(g.eval::<bool>("return IsValid(AWAY) and AWAY:GetPos().z < 50"));
    // health, up to the most
    g.runtime.run_string("Entity(1):SetHealth(50) KIT = spawn('item_healthkit', 0, 0, 20)", "kit").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(KIT) and Entity(1):Health() == 75"));
    assert_eq!(sounds(&g), ["HealthKit.Touch"]);
    g.runtime.run_string("Entity(1):SetHealth(95) VIAL = spawn('item_healthvial', 0, 0, 20)", "vial").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(VIAL) and Entity(1):Health() == 100"));
    // a player at their most leaves a kit
    g.runtime.run_string("FULL = spawn('item_healthkit', 0, 0, 20)", "full").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return IsValid(FULL) and Entity(1):Health() == 100"));
    g.runtime.run_string("FULL:Remove()", "gone").unwrap();
    // armor, up to 100
    g.runtime.run_string("BATTERY = spawn('item_battery', 0, 0, 20)", "battery").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(BATTERY) and Entity(1):Armor() == 15"));
    g.runtime.run_string("Entity(1):SetArmor(95) BATTERY = spawn('item_battery', 0, 0, 20)", "battery").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(BATTERY) and Entity(1):Armor() == 100"));
    g.runtime.run_string("BATTERY = spawn('item_battery', 0, 0, 20)", "battery").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("local ok = IsValid(BATTERY) and Entity(1):Armor() == 100 BATTERY:Remove() return ok"));
    sounds(&g);
    // each box of rounds
    for (class, ammo, amount) in [
        ("item_ammo_pistol", "Pistol", 20),
        ("item_ammo_pistol_large", "Pistol", 100),
        ("item_ammo_smg1", "SMG1", 45),
        ("item_ammo_smg1_large", "SMG1", 225),
        ("item_ammo_ar2", "AR2", 20),
        ("item_ammo_ar2_large", "AR2", 100),
        ("item_ammo_357", "357", 6),
        ("item_ammo_357_large", "357", 20),
        ("item_ammo_crossbow", "XBowBolt", 6),
        ("item_rpg_round", "RPG_Round", 1),
        ("item_ammo_smg1_grenade", "SMG1_Grenade", 1),
        ("item_box_buckshot", "Buckshot", 20),
        ("item_ammo_ar2_altfire", "AR2AltFire", 1),
        ("item_large_box_mrounds", "SMG1", 225),
    ] {
        g.runtime.run_string(&format!("Entity(1):SetAmmo(0, '{ammo}') BOX = spawn('{class}', 0, 0, 20)"), "box").unwrap();
        g.runtime.think(0.1);
        assert_eq!(g.eval::<(bool, i64)>(&format!("return IsValid(BOX), Entity(1):GetAmmoCount('{ammo}')")), (false, amount), "{class}");
        assert_eq!(sounds(&g), ["BaseCombatCharacter.AmmoPickup"], "{class}");
    }
    // rounds a player cannot carry more of, and a gamemode that says no
    g.runtime
        .run_string(
            "Entity(1):SetAmmo(9999, 'SMG1') SMG = spawn('item_ammo_smg1', 0, 0, 20)
             HOOKS.PlayerCanPickupItem = function(p, item) return false end
             Entity(1):SetHealth(50) KIT = spawn('item_healthkit', 0, 0, 20)",
            "no",
        )
        .unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return IsValid(SMG) and IsValid(KIT) and Entity(1):Health() == 50"));
    g.runtime.run_string("HOOKS.PlayerCanPickupItem = nil", "yes").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return IsValid(SMG) and not IsValid(KIT) and Entity(1):Health() == 75"));
    // a kit a hook removes as it is asked is not taken
    g.runtime
        .run_string(
            "HOOKS.PlayerCanPickupItem = function(p, item) item:Remove() end
             Entity(1):SetHealth(50) KIT = spawn('item_healthkit', 0, 0, 20)",
            "removed",
        )
        .unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(KIT) and Entity(1):Health() == 50"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn players_walk_through_items() {
    let (g, alice) = items_game(breakable_map(Vec::new()));
    // a full player, a kit in their way (lower than a step)
    g.runtime.run_string("KIT = spawn('item_healthkit', -60, 0, 20)", "kit").unwrap();
    g.runtime.think(0.5);
    let before = g.eval::<f64>("return KIT:GetPos().x");
    g.runtime.set_input(
        alice,
        UserCmd { view_angles: Angle::new(0.0, 180.0, 0.0), forward_move: gmod_movement::FULL_MOVE, ..UserCmd::default() },
    );
    let mut highest = 0.0f64;
    for _ in 0..30 {
        g.runtime.think(0.05);
        highest = highest.max(g.eval::<f64>("return Entity(1):GetPos().z"));
    }
    assert!(g.eval::<bool>("return Entity(1):GetPos().x < -100"), "they went past it");
    assert!(highest < 1.0, "not over it: {highest}");
    assert!(g.eval::<bool>("return IsValid(KIT) and Entity(1):Health() == 100"));
    assert!((g.eval::<f64>("return KIT:GetPos().x") - before).abs() < 1.0, "nor pushing it");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn chargers_give_while_the_use_key_is_held() {
    let (g, alice) = items_game(breakable_map(Vec::new()));
    let mut seen = 0;
    g.runtime
        .run_string(
            "CHARGER = spawn('item_healthcharger', -60, 0, 48)
             CHARGER:SetKeyValue('OnEmpty', 'rec,Empty,,0,-1')
             Entity(1):SetHealth(30) Entity(1):SetEyeAngles(Angle(15, 180, 0))",
            "charger",
        )
        .unwrap();
    taken(&g, &mut seen);
    sounds(&g);
    let look = Angle::new(15.0, 180.0, 0.0);
    g.runtime.set_input(alice, UserCmd { view_angles: look, buttons: 32, ..UserCmd::default() });
    g.runtime.think(1.0);
    let health = g.eval::<i64>("return Entity(1):Health()");
    assert!((38..=42).contains(&health), "ten a second: {health}");
    assert_eq!(sounds(&g), ["WallHealth.Start"]);
    // let go, then held on until it is empty: 50 in all
    g.runtime.set_input(alice, UserCmd { view_angles: look, ..UserCmd::default() });
    g.runtime.think(0.5);
    g.runtime.set_input(alice, UserCmd { view_angles: look, buttons: 32, ..UserCmd::default() });
    g.runtime.think(6.0);
    assert_eq!(g.eval::<i64>("return Entity(1):Health()"), 80);
    assert_eq!(taken(&g, &mut seen), ["Empty"]);
    assert_eq!(sounds(&g), ["WallHealth.Start"]);
    // empty, it says no; filled again, it gives
    g.runtime.set_input(alice, UserCmd { view_angles: look, ..UserCmd::default() });
    g.runtime.think(0.5);
    g.runtime.set_input(alice, UserCmd { view_angles: look, buttons: 32, ..UserCmd::default() });
    g.runtime.think(0.2);
    assert_eq!(sounds(&g), ["WallHealth.Deny"]);
    g.runtime.run_string("CHARGER:Fire('Recharge')", "recharge").unwrap();
    g.runtime.think(0.5);
    assert!(g.eval::<i64>("return Entity(1):Health()") > 80);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn props_break_after_enough_damage() {
    let (g, _) = items_game(breakable_map(Vec::new()));
    let mut seen = 0;
    g.runtime
        .run_string(
            "CRATE = spawn('prop_physics', -300, 0, 0, 'models/crate.mdl')
             CRATE:SetKeyValue('OnBreak', 'rec,Broke,,0,-1')
             BOX = spawn('prop_physics', -300, 100, 0, 'models/box.mdl')
             HOOKS.PropBreak = function(attacker, prop) print('PropBreak', attacker == Entity(1), prop == CRATE) end",
            "crate",
        )
        .unwrap();
    // the class its model names: 20 health, bullets do twice their damage
    assert!(g.eval::<bool>("return CRATE:Health() == 20 and CRATE:GetMaxHealth() == 20 and BOX:Health() == 0"));
    g.runtime
        .run_string(
            "CRATE:TakeDamage(5, Entity(1))
             local d = DamageInfo() d:SetDamage(5) d:SetDamageType(2) d:SetAttacker(Entity(1)) CRATE:TakeDamageInfo(d)
             BOX:TakeDamage(1000, Entity(1))",
            "hit",
        )
        .unwrap();
    assert!(g.eval::<bool>("return CRATE:Health() == 5 and IsValid(BOX) and BOX:Health() == 0"));
    taken(&g, &mut seen);
    sounds(&g);
    g.runtime.run_string("CRATE:TakeDamage(10, Entity(1))", "break").unwrap();
    assert_eq!(taken(&g, &mut seen), ["PropBreak\ttrue\ttrue"]);
    assert_eq!(sounds(&g), ["Wood.Break"]);
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(CRATE) and IsValid(BOX)"));
    assert_eq!(taken(&g, &mut seen), ["Broke"]);
    // the Break input breaks one at once
    // the Break input breaks one at once, and once
    taken(&g, &mut seen);
    g.runtime
        .run_string("CRATE = spawn('prop_physics', -300, 0, 0, 'models/crate.mdl') CRATE:Fire('Break') CRATE:Fire('Break')", "input")
        .unwrap();
    g.runtime.think(0.1);
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(CRATE)"));
    let lines = taken(&g, &mut seen);
    assert_eq!(lines.iter().filter(|l| l.starts_with("PropBreak")).count(), 1, "{lines:?}");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn explosive_props_blow_up() {
    let (g, _) = items_game(breakable_map(Vec::new()));
    let mut seen = 0;
    // a barrel 84 from the player's box, a crate 52 from the barrel's middle the other way
    g.runtime
        .run_string(
            "BARREL = spawn('prop_physics', -100, 0, 0, 'models/barrel.mdl')
             CRATE = spawn('prop_physics', -100, -60, 0, 'models/box.mdl')
             FAR = spawn('prop_physics', -200, 0, 0, 'models/barrel.mdl')
             BEHIND = spawn('prop_physics', 200, 0, 0, 'models/barrel.mdl')",
            "barrels",
        )
        .unwrap();
    g.runtime.think(0.5);
    taken(&g, &mut seen);
    sounds(&g);
    // an addon's detour of util.Effect does not see the engine's
    g.runtime
        .run_string("FAR:Remove() BEHIND:Remove() util.Effect = function() error('the engine went through Lua') end", "aside")
        .unwrap();
    g.runtime.run_string("BARREL:TakeDamage(5)", "boom").unwrap();
    let near = 100 - g.eval::<i64>("return Entity(1):Health()");
    assert!((55..=80).contains(&near), "{near}");
    assert_eq!(taken(&g, &mut seen), ["effect\tExplosion\t100"]);
    assert_eq!(sounds(&g), ["Metal_Box.Break", "BaseExplosionEffect.Sound"]);
    g.runtime.think(0.2);
    assert!(g.eval::<bool>("return not IsValid(BARREL) and CRATE:GetPos().y < -80"), "the crate is thrown away");
    // farther, less
    g.runtime.run_string("Entity(1):SetHealth(100) FAR = spawn('prop_physics', -200, 0, 0, 'models/barrel.mdl')", "far").unwrap();
    g.runtime.think(0.1);
    g.runtime.run_string("FAR:TakeDamage(5)", "boom").unwrap();
    let far = 100 - g.eval::<i64>("return Entity(1):Health()");
    assert!(far > 0 && far < near, "{far} {near}");
    // behind the wall, nothing
    g.runtime.run_string("Entity(1):SetHealth(100) BEHIND = spawn('prop_physics', 200, 0, 0, 'models/barrel.mdl')", "behind").unwrap();
    g.runtime.think(0.1);
    g.runtime.run_string("BEHIND:TakeDamage(5)", "boom").unwrap();
    assert_eq!(g.eval::<i64>("return Entity(1):Health()"), 100);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn barrels_in_a_pile_blow_up_once_each() {
    let (g, _) = items_game(breakable_map(Vec::new()));
    let mut seen = 0;
    g.runtime
        .run_string(
            "for _, y in ipairs({ -40, 0, 40 }) do spawn('prop_physics', -100, y, 0, 'models/barrel.mdl') end
             HOOKS.PropBreak = function(attacker, prop) print('PropBreak') end",
            "pile",
        )
        .unwrap();
    g.runtime.think(0.5);
    g.runtime
        .run_string(
            "Entity(1):SetHealth(1000) for _, e in ipairs(ents.FindByModel('models/barrel.mdl')) do e:TakeDamage(5) break end",
            "boom",
        )
        .unwrap();
    let lines = taken(&g, &mut seen);
    let count = |line: &str| lines.iter().filter(|l| *l == line).count();
    assert_eq!((count("PropBreak"), count("effect\tExplosion\t100")), (3, 3), "{lines:?}");
    // some 66 from each of three
    let hurt = 1000 - g.eval::<i64>("return Entity(1):Health()");
    assert!((150..=240).contains(&hurt), "{hurt}");
    g.runtime.think(0.1);
    assert_eq!(g.eval::<i64>("return #ents.FindByModel('models/barrel.mdl')"), 0);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn breakable_brushes_break_and_go() {
    let (g, _) = items_game(breakable_map(vec![
        pairs(&[("classname", "func_breakable"), ("model", "*1"), ("health", "10"), ("material", "0")]),
        pairs(&[
            ("classname", "func_breakable"),
            ("model", "*2"),
            ("targetname", "pane"),
            ("health", "50"),
            ("spawnflags", "1"),
            ("material", "1"),
            ("OnBreak", "rec,Broke,,0,-1"),
        ]),
        pairs(&[("classname", "func_breakable"), ("model", "*3"), ("health", "1"), ("explodemagnitude", "100")]),
    ]));
    let mut seen = 0;
    g.runtime
        .run_string(
            "for _, e in ipairs(ents.FindByClass('func_breakable')) do
                 if e:GetModel() == '*1' then GLASS = e elseif e:GetModel() == '*2' then PANE = e else BLOCK = e end
             end",
            "find",
        )
        .unwrap();
    assert_eq!(g.eval::<i64>("return blocked(64)"), 300, "the glass stops a trace");
    taken(&g, &mut seen);
    sounds(&g);
    // damage breaks the glass
    g.runtime.run_string("GLASS:TakeDamage(5)", "hit").unwrap();
    assert!(g.eval::<bool>("return IsValid(GLASS) and GLASS:Health() == 5 and GLASS:GetMaxHealth() == 10"));
    g.runtime.run_string("GLASS:TakeDamage(5)", "hit").unwrap();
    assert_eq!(sounds(&g), ["Glass.Break"]);
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(GLASS)"));
    assert_eq!(g.eval::<i64>("return blocked(64)"), 400, "gone, the trace goes on to the pane");
    // the pane only breaks by inputs
    g.runtime.run_string("PANE:TakeDamage(1000) PANE:Fire('RemoveHealth', '20')", "hit").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return IsValid(PANE) and PANE:Health() == 30"));
    g.runtime.run_string("PANE:Fire('Break')", "break").unwrap();
    g.runtime.think(0.1);
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(PANE)"));
    assert_eq!(taken(&g, &mut seen), ["Broke"]);
    assert_eq!(sounds(&g), ["Wood.Break"]);
    assert_eq!(g.eval::<i64>("return blocked(64)"), 600);
    // one that explodes hurts the player near it
    g.runtime.run_string("BLOCK:TakeDamage(5)", "boom").unwrap();
    assert!(g.eval::<bool>("return Entity(1):Health() < 100"));
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not IsValid(BLOCK)"));
    assert_eq!(taken(&g, &mut seen), ["effect\tExplosion\t100"]);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
