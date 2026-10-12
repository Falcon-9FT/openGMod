//! Touching: the map's triggers, scripted triggers, weapons on the ground.

use super::*;
use crate::vector::Vector;

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// [`player_map`]'s floor, with five trigger volumes along x (`*1` to `*5`, each 64 wide from
/// x = 200 + 100 i) and the entities given.
fn trigger_map(entities: Vec<Vec<(String, String)>>) -> Map {
    use gmod_bsp::testmap::{TestBrush, TestMap};
    let mut models = vec![vec![TestBrush::cuboid([-2048.0, -1024.0, -64.0], [2048.0, 1024.0, 0.0])]];
    for i in 0..5 {
        let x = 200.0 + 100.0 * i as f32;
        models.push(vec![TestBrush::cuboid([x, -64.0, 0.0], [x + 64.0, 64.0, 128.0]).material("TOOLS/TOOLSTRIGGER", 0)]);
    }
    let test = TestMap { models, ..TestMap::default() };
    let bsp = gmod_bsp::Bsp::parse("test.bsp".into(), Arc::from(test.bytes())).unwrap();
    let mut all = vec![
        pairs(&[("classname", "worldspawn")]),
        pairs(&[("classname", "info_player_start"), ("origin", "0 0 0"), ("angles", "0 90 0")]),
        pairs(&[("classname", "info_target"), ("targetname", "rec")]),
    ];
    all.extend(entities);
    Map {
        name: "gm_triggers".into(),
        entities: all,
        brush_models: bsp.models.iter().map(|m| (m.mins, m.maxs)).collect(),
        collision: Some(Arc::new(gmod_bsp::Collision::new(&bsp))),
        textures: Vec::new(),
        visibility: None,
    }
}

fn trigger_game(map: Map) -> Game {
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
    g.runtime
        .run_string(
            "HOOKS.AcceptInput = function(ent, input, activator, caller, param)
                 if ent:GetName() == 'rec' then print(input, activator:IsValid() and activator:GetClass() or '-') end
             end",
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
fn triggers_of_the_map() {
    let g = trigger_game(trigger_map(vec![
        pairs(&[
            ("classname", "trigger_multiple"),
            ("model", "*1"),
            ("spawnflags", "1"),
            ("wait", "1"),
            ("OnStartTouch", "rec,In,,0,-1"),
            ("OnEndTouchAll", "rec,AllOut,,0,-1"),
            ("OnTrigger", "rec,Trigger,,0,-1"),
        ]),
        pairs(&[("classname", "trigger_once"), ("model", "*2"), ("spawnflags", "1"), ("OnTrigger", "rec,Once,,0,-1")]),
        pairs(&[("classname", "trigger_teleport"), ("model", "*3"), ("spawnflags", "1"), ("target", "dest")]),
        pairs(&[("classname", "info_teleport_destination"), ("targetname", "dest"), ("origin", "-300 0 0"), ("angles", "0 180 0")]),
        pairs(&[("classname", "trigger_hurt"), ("model", "*4"), ("spawnflags", "1"), ("damage", "10"), ("OnHurtPlayer", "rec,Hurt,,0,-1")]),
        pairs(&[("classname", "trigger_push"), ("model", "*5"), ("spawnflags", "1"), ("pushdir", "-90 0 0"), ("speed", "500")]),
    ]));
    let alice = g.runtime.join(alice()).unwrap();
    let mut seen = 0;
    let go = |x: f32| g.runtime.run_string(&format!("Entity(1):SetPos(Vector({x}, 0, 0))"), "go").unwrap();
    g.runtime.think(0.1);
    taken(&g, &mut seen);

    // in: touched and triggered; again a second later while still in; out
    go(232.0);
    g.runtime.think(0.1);
    assert_eq!(taken(&g, &mut seen), ["In\tplayer", "Trigger\tplayer"]);
    g.runtime.think(1.0);
    assert_eq!(taken(&g, &mut seen), ["Trigger\tplayer"], "after its wait");
    go(0.0);
    g.runtime.think(0.1);
    assert_eq!(taken(&g, &mut seen), ["AllOut\tplayer"]);

    // once, and gone
    go(332.0);
    g.runtime.think(0.1);
    assert_eq!(taken(&g, &mut seen), ["Once\tplayer"]);
    assert!(g.eval::<bool>("return #ents.FindByClass('trigger_once') == 0"));

    // through the teleport: at the destination, looking its way, still
    g.runtime.run_string("Entity(1):SetVelocity(Vector(300, 0, 0))", "run").unwrap();
    go(432.0);
    g.runtime.think(0.05);
    let at = g.runtime.position(alice).unwrap();
    assert!((at - Vector::new(-300.0, 0.0, 0.0)).length() < 1.0, "{at:?}");
    assert!(g.eval::<bool>("return Entity(1):EyeAngles().y == 180 and Entity(1):GetVelocity():Length() < 1"));

    // hurt every half second
    go(532.0);
    g.runtime.think(1.2);
    let health = g.eval::<i64>("return Entity(1):Health()");
    assert!((70..=80).contains(&health), "{health}");
    assert!(taken(&g, &mut seen).iter().filter(|l| l.starts_with("Hurt")).count() >= 2);

    // pushed up and off the ground
    go(632.0);
    g.runtime.think(0.3);
    let at = g.runtime.position(alice).unwrap();
    assert!(at.z > 40.0, "{at:?}");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn triggers_take_what_their_flags_and_filters_say() {
    let g = trigger_game(trigger_map(vec![
        // props only: the player walks through without a word
        pairs(&[("classname", "trigger_multiple"), ("model", "*1"), ("spawnflags", "8"), ("OnStartTouch", "rec,Prop,,0,-1")]),
        // players named bob only
        pairs(&[("classname", "filter_activator_name"), ("targetname", "only_bob"), ("filtername", "bob")]),
        pairs(&[
            ("classname", "trigger_multiple"),
            ("model", "*2"),
            ("spawnflags", "1"),
            ("filtername", "only_bob"),
            ("OnStartTouch", "rec,Bob,,0,-1"),
        ]),
        pairs(&[
            ("classname", "trigger_multiple"),
            ("model", "*3"),
            ("spawnflags", "1"),
            ("StartDisabled", "1"),
            ("OnStartTouch", "rec,Late,,0,-1"),
            ("targetname", "late"),
        ]),
    ]));
    g.runtime.join(alice()).unwrap();
    let mut seen = 0;
    let go = |x: f32| g.runtime.run_string(&format!("Entity(1):SetPos(Vector({x}, 0, 0))"), "go").unwrap();
    g.runtime.think(0.1);
    taken(&g, &mut seen);
    go(232.0);
    g.runtime.think(0.1);
    assert!(taken(&g, &mut seen).is_empty());
    g.runtime.run_string("local p = ents.Create('prop_physics') p:SetModel('models/box.mdl') p:SetPos(Vector(232, 40, 40)) p:Spawn() p:PhysicsInitBox(Vector(-8, -8, 0), Vector(8, 8, 16))", "prop").unwrap();
    g.runtime.think(0.1);
    assert_eq!(taken(&g, &mut seen), ["Prop\tprop_physics"]);
    go(332.0);
    g.runtime.think(0.1);
    assert!(taken(&g, &mut seen).is_empty(), "not bob");
    go(0.0);
    g.runtime.run_string("Entity(1):SetName('bob')", "bob").unwrap();
    g.runtime.think(0.1);
    go(332.0);
    g.runtime.think(0.1);
    assert_eq!(taken(&g, &mut seen), ["Bob\tplayer"]);
    go(432.0);
    g.runtime.think(0.1);
    assert!(taken(&g, &mut seen).is_empty(), "disabled");
    g.runtime.run_string("ents.FindByName('late')[1]:Fire('Enable')", "enable").unwrap();
    g.runtime.think(0.1);
    assert_eq!(taken(&g, &mut seen), ["Late\tplayer"]);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn scripted_triggers_and_weapons_on_the_ground() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    g.runtime
        .run_string(
            "local ENT = { Type = 'anim' }
             function ENT:Initialize() self:SetSolid(SOLID_BBOX) self:SetCollisionBounds(Vector(-16, -16, 0), Vector(16, 16, 32)) self:SetTrigger(true) end
             function ENT:StartTouch(e) print('start', e:GetClass()) end
             function ENT:Touch(e) self.touches = (self.touches or 0) + 1 end
             function ENT:EndTouch(e) print('end', e:GetClass()) end
             scripted_ents.Register(ENT, 'sent_pad')
             PAD = ents.Create('sent_pad') PAD:SetPos(Vector(-300, 0, 0)) PAD:Spawn()",
            "pad",
        )
        .unwrap();
    g.runtime.think(0.1);
    let start = g.printed().len();
    g.runtime.run_string("Entity(1):SetPos(Vector(-300, 0, 0))", "go").unwrap();
    g.runtime.think(0.5);
    g.runtime.run_string("Entity(1):SetPos(Vector(0, 0, 0))", "go").unwrap();
    g.runtime.think(0.1);
    assert_eq!(&g.printed()[start..], "start\tplayer\nend\tplayer\n");
    assert!(g.eval::<bool>("return PAD.touches > 20"), "every tick while in");

    // the player has a pistol: one on the ground gives its ammo and goes
    let ammo = g.eval::<i64>("return Entity(1):GetAmmoCount('Pistol')");
    g.runtime.run_string("W = ents.Create('weapon_pistol') W:SetPos(Vector(0, 0, 0)) W:Spawn()", "drop").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not W:IsValid()"), "it went");
    assert!(g.eval::<i64>("return Entity(1):GetAmmoCount('Pistol')") > ammo, "took its ammo");
    // without one, it is picked up - when the gamemode lets them
    g.runtime
        .run_string(
            "Entity(1):StripWeapons()
             HOOKS.PlayerCanPickupWeapon = function(p, w) return ALLOW == true end
             W = ents.Create('weapon_pistol') W:SetPos(Vector(0, 0, 0)) W:Spawn()",
            "drop",
        )
        .unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return W:GetOwner() == NULL and not Entity(1):HasWeapon('weapon_pistol')"), "not allowed");
    g.runtime.run_string("ALLOW = true", "allow").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return W:GetOwner() == Entity(1) and Entity(1):HasWeapon('weapon_pistol')"));
    let _ = alice;
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
