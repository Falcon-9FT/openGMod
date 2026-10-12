//! Shots: the hitboxes of players and NPCs, bullets, and the weapons that fire them.

use super::*;

/// `models/man.mdl`: a root at the feet, a spine 30 up, a head 30 above that, legs 8 to the
/// left (+y) and the right. Its hitboxes: the chest from 30 to 60 high, the head 60 to 72, the
/// legs from the ground to 30, each 6 wide.
pub(super) fn man() -> Vec<u8> {
    use gmod_model::testmodel::{TestBone, TestRig};
    let bone = |name: &str, parent: i32, position: [f32; 3]| TestBone { name: name.into(), parent, position, euler: [0.0; 3] };
    TestRig {
        name: "man.mdl".into(),
        checksum: 11,
        bones: vec![
            bone("root", -1, [0.0; 3]),
            bone("spine", 0, [0.0, 0.0, 30.0]),
            bone("head", 1, [0.0, 0.0, 30.0]),
            bone("leg_l", 0, [0.0, 8.0, 0.0]),
            bone("leg_r", 0, [0.0, -8.0, 0.0]),
        ],
        hitboxes: vec![
            (1, 2, [-8.0, -10.0, 0.0], [8.0, 10.0, 30.0]),
            (2, 1, [-5.0, -5.0, 0.0], [5.0, 5.0, 12.0]),
            (3, 6, [-3.0, -3.0, 0.0], [3.0, 3.0, 30.0]),
            (4, 7, [-3.0, -3.0, 0.0], [3.0, 3.0, 30.0]),
        ],
        ..TestRig::default()
    }
    .files()
    .0
}

/// The engine's crowbar, as its script has it.
const CROWBAR: &str = r##"WeaponData
{
    "printname"   "#HL2_Crowbar"
    "bucket"      "0"
    "clip_size"   "-1"
    "primary_ammo" "None"
    SoundData
    {
        "single_shot"     "Weapon_Crowbar.Single"
        "melee_hit"       "Weapon_Crowbar.Melee_Hit"
        "melee_hit_world" "Weapon_Crowbar.Melee_HitWorld"
    }
}"##;

/// A game of `max` players with `models/man.mdl`, the crowbar and the crate of the ragdoll
/// tests (25 kg) among the files.
fn shooting_game(max: u32) -> Game {
    let mdl = gmod_model::testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let mut files: Vec<(&str, Vec<u8>)> = vec![
        ("lua/includes/init.lua", ENTITY_LUA.as_bytes().to_vec()),
        ("models/box.mdl", mdl),
        ("models/man.mdl", man()),
        ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes().to_vec()),
        ("scripts/weapons/weapon_crowbar.txt", CROWBAR.as_bytes().to_vec()),
    ];
    files.extend(super::ragdoll::ragdoll_files().into_iter().filter(|(p, _)| p.starts_with("models/crate")));
    let files: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (*p, b.as_slice())).collect();
    let g = game_with(Realm::Server, &files);
    g.runtime.set_max_players(max);
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime.spawn_map(&player_map());
    g
}

/// Alice at the spawn point looking along y, and Bob - a man of hitboxes - 200 ahead of her.
fn alice_and_bob() -> (Game, EntityHandle, EntityHandle) {
    let g = shooting_game(2);
    let alice = g.runtime.join(alice()).unwrap();
    let bob = g.runtime.join(PlayerInfo { name: "Bob".into(), ..PlayerInfo::default() }).unwrap();
    g.runtime.think(0.05);
    place_bob(&g);
    (g, alice, bob)
}

fn place_bob(g: &Game) {
    g.runtime
        .run_string("local b = Entity(2) b:SetModel('models/man.mdl') b:SetPos(Vector(0, 200, 0)) b:SetRenderAngles(Angle(0, 0, 0))", "bob")
        .unwrap();
}

#[test]
fn shots_hit_the_hitboxes_of_players() {
    let g = shooting_game(1);
    g.runtime.join(alice()).unwrap();
    g.runtime.run_string("Entity(1):SetModel('models/man.mdl') Entity(1):SetRenderAngles(Angle(0, 0, 0))", "model").unwrap();
    assert!(g.eval::<bool>(
        "local p = Entity(1)
         local mins, maxs = p:GetHitBoxBounds(1)
         local set, name = p:GetHitboxSet()
         return p:GetHitBoxCount() == 4 and p:GetHitBoxBone(1) == 2 and p:GetHitBoxHitGroup(1) == HITGROUP_HEAD
             and mins == Vector(-5, -5, 0) and maxs == Vector(5, 5, 12) and set == 0 and name == 'default'
             and p:GetHitboxSetCount() == 1 and p:GetHitBoxCount(3) == 0 and p:GetHitBoxBone(9) == nil"
    ));
    // through the head, the chest, a leg; between the legs nothing
    assert!(g.eval::<bool>(
        "local function shot(z, y, mask)
             return util.TraceLine({ start = Vector(-90, y, z), endpos = Vector(90, y, z), mask = mask or MASK_SHOT })
         end
         local head, chest, between, leg = shot(66, 0), shot(45, 0), shot(15, 0), shot(15, 8)
         return head.Entity == Entity(1) and head.HitGroup == HITGROUP_HEAD and head.HitBox == 1
             and math.abs(head.HitPos.x + 5) < 0.1 and head.HitNormal == Vector(-1, 0, 0)
             and chest.HitGroup == HITGROUP_CHEST and leg.HitGroup == HITGROUP_LEFTLEG and leg.HitBox == 2
             and between.Entity ~= Entity(1) and not between.Hit
             and shot(15, 0, MASK_SOLID).Entity == Entity(1) and shot(15, 0, MASK_SOLID).HitGroup == HITGROUP_GENERIC"
    ));
    // the model turns as its legs do: the left leg is then behind
    g.runtime.run_string("Entity(1):SetRenderAngles(Angle(0, 90, 0))", "turn").unwrap();
    assert!(g.eval::<bool>(
        "local function shot(x) return util.TraceLine({ start = Vector(x, -90, 15), endpos = Vector(x, 90, 15), mask = MASK_SHOT }) end
         return shot(-8).HitGroup == HITGROUP_LEFTLEG and shot(8).HitGroup == HITGROUP_RIGHTLEG and not shot(0).Hit"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn bullets_go_through_the_hooks_and_hurt_by_hit_group() {
    let (g, _alice, _bob) = alice_and_bob();
    g.runtime
        .run_string(
            "HOOKS.ScalePlayerDamage = function(p, group, dmg) GROUP = group if group == HITGROUP_HEAD then dmg:ScaleDamage(2) end end
             HOOKS.EntityFireBullets = function(e, data)
                 FIRED = (FIRED or 0) + 1
                 if data.Damage == 99 then return false end
                 data.Damage = data.Damage + 1 return true
             end
             HOOKS.PostEntityFireBullets = function(e, data) POST = data.Trace.Entity end
             local a = Entity(1)
             a:FireBullets({ Src = a:EyePos(), Dir = Vector(0, 1, 0), Damage = 9, Tracer = 0,
                 Callback = function(att, tr, dmg) CB = { att, tr.Entity, tr.HitGroup, dmg:GetDamage(), dmg:GetDamageType() } end })",
            "shoot",
        )
        .unwrap();
    assert!(g.eval::<bool>(
        "local a, b = Entity(1), Entity(2)
         return b:Health() == 80 and GROUP == HITGROUP_HEAD and b:LastHitGroup() == HITGROUP_HEAD and FIRED == 1 and POST == b
             and CB[1] == a and CB[2] == b and CB[3] == HITGROUP_HEAD and CB[4] == 10 and CB[5] == DMG_BULLET"
    ));
    // stopped by the hook, by the callback, by the distance; a chest shot as it is
    place_bob(&g);
    g.runtime
        .run_string(
            "local a = Entity(1)
             a:FireBullets({ Src = a:EyePos(), Dir = Vector(0, 1, 0), Damage = 99 })
             a:FireBullets({ Src = a:EyePos(), Dir = Vector(0, 1, 0), Damage = 5, Callback = function() return { damage = false } end })
             a:FireBullets({ Src = a:EyePos(), Dir = Vector(0, 1, 0), Damage = 5, Distance = 100 })
             a:FireBullets({ Src = Vector(0, 0, 45), Dir = Vector(0, 1, 0), Damage = 4 })",
            "more",
        )
        .unwrap();
    assert!(g.eval::<bool>("return Entity(2):Health() == 75 and GROUP == HITGROUP_CHEST"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn bullets_spread_and_push_what_they_hit() {
    let (g, _alice, _bob) = alice_and_bob();
    g.runtime.run_string("Entity(2):SetPos(Vector(500, 500, 0))", "away").unwrap();
    // ten bullets at the wall at x 100, each somewhere else within the cone
    g.runtime
        .run_string(
            "HITS = {}
             HOOKS.PostEntityFireBullets = function(e, data) HITS[#HITS + 1] = data.Trace.HitPos end
             Entity(1):FireBullets({ Src = Vector(0, 0, 64), Dir = Vector(1, 0, 0), Num = 10, Spread = Vector(0.1, 0.1, 0), Damage = 1 })",
            "spread",
        )
        .unwrap();
    assert!(g.eval::<bool>(
        "local seen, apart = {}, 0
         for _, p in ipairs(HITS) do
             local key = math.floor(p.y * 10) .. ':' .. math.floor(p.z * 10)
             if not seen[key] then seen[key] = true apart = apart + 1 end
             if math.abs(p.x - 100) > 0.5 or math.abs(p.y) > 10.5 or math.abs(p.z - 64) > 10.5 then return false end
         end
         return #HITS == 10 and apart >= 8"
    ));
    // a crate of 25 kg goes the way the bullet went
    g.runtime
        .run_string(
            "C = ents.Create('prop_physics') C:SetModel('models/crate.mdl') C:SetPos(Vector(0, 120, 0)) C:Spawn()
             Entity(1):FireBullets({ Src = Vector(0, 0, 8), Dir = Vector(0, 1, 0), Damage = 20 })",
            "crate",
        )
        .unwrap();
    g.runtime.think(0.1);
    assert!(
        g.eval::<bool>("return C:GetVelocity().y > 10 or C:GetPos().y > 121"),
        "{}",
        g.eval::<String>("return tostring(C:GetVelocity())")
    );
}

#[test]
fn the_engines_pistol_and_crowbar() {
    let (g, alice, _bob) = alice_and_bob();
    let attack = |buttons: u32| UserCmd { view_angles: vector::Angle::new(0.0, 90.0, 0.0), buttons, ..UserCmd::default() };
    // a press: one shot, held: no more
    g.runtime.set_input(alice, attack(crate::player::key::ATTACK));
    place_bob(&g);
    g.runtime.think(0.015);
    place_bob(&g);
    g.runtime.think(0.3);
    assert!(
        g.eval::<bool>("return Entity(2):Health() == 92 and Entity(1):GetActiveWeapon():Clip1() == 17"),
        "{}",
        g.eval::<String>("return Entity(2):Health() .. ' ' .. Entity(1):GetActiveWeapon():Clip1()")
    );
    // the view kicked up
    assert!(g.eval::<bool>("return Entity(1):GetViewPunchAngles().p < 0 or Entity(1):GetViewPunchVelocity().p < 0"));
    g.runtime.set_input(alice, attack(0));
    g.runtime.think(0.3);
    assert!(
        g.eval::<bool>("local p = Entity(1):GetViewPunchAngles() return math.abs(p.p) < 0.1 and math.abs(p.y) < 0.1"),
        "the kick springs back"
    );
    g.runtime.set_input(alice, attack(crate::player::key::ATTACK));
    place_bob(&g);
    g.runtime.think(0.015);
    assert!(g.eval::<bool>("return Entity(2):Health() == 84 and Entity(1):GetActiveWeapon():Clip1() == 16"));
    // empty: a click, and the clip refills from what she carries
    g.runtime.set_input(alice, attack(0));
    g.runtime.run_string("Entity(1):GetActiveWeapon():SetClip1(0) AMMO = Entity(1):GetAmmoCount('Pistol')", "empty").unwrap();
    g.runtime.think(0.5);
    g.runtime.set_input(alice, attack(crate::player::key::ATTACK));
    g.runtime.think(0.1);
    g.runtime.set_input(alice, attack(0));
    g.runtime.think(0.1);
    assert!(g.eval::<bool>(
        "local w = Entity(1):GetActiveWeapon() return Entity(2):Health() == 84 and w:Clip1() == 18 and Entity(1):GetAmmoCount('Pistol') == AMMO - 18"
    ));

    // the crowbar: a crate within reach is hit and pushed; nothing within reach is a miss
    g.runtime
        .run_string(
            "local a = Entity(1) a:StripWeapons() a:Give('weapon_crowbar') a:SelectWeapon('weapon_crowbar')
             C = ents.Create('prop_physics') C:SetModel('models/crate.mdl') C:SetPos(Vector(0, 50, 50)) C:Spawn()
             C:GetPhysicsObject():EnableGravity(false)
             HOOKS.EntityTakeDamage = function(e, dmg) if e == C then CLUB = dmg:GetDamageType() HIT = dmg:GetDamage() end end",
            "crowbar",
        )
        .unwrap();
    g.runtime.think(0.1);
    g.runtime.set_input(alice, attack(crate::player::key::ATTACK));
    g.runtime.think(0.015);
    g.runtime.set_input(alice, attack(0));
    g.runtime.think(0.2);
    assert!(g.eval::<bool>(
        "return Entity(1):GetActiveWeapon():GetClass() == 'weapon_crowbar' and CLUB == DMG_CLUB and HIT == 10 and C:GetVelocity().y > 1"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn shots_hit_where_the_shooter_saw_the_target() {
    let (g, alice, _bob) = alice_and_bob();
    // Bob stood in Alice's line of fire when her client showed the game, and has moved since
    g.runtime.think(0.05);
    let seen = g.runtime.cur_time() - 0.02;
    g.runtime.run_string("Entity(2):SetPos(Vector(40, 200, 0))", "away").unwrap();
    g.runtime.think(0.2);
    let shoot = "local a = Entity(1) a:LagCompensation(true)
                 a:FireBullets({ Src = a:EyePos(), Dir = Vector(0, 1, 0), Damage = 10 }) a:LagCompensation(false)";
    g.runtime.run_string(shoot, "now").unwrap();
    assert_eq!(g.eval::<i64>("return Entity(2):Health()"), 100, "where he is, she misses");
    g.runtime.set_view_time(alice, seen);
    g.runtime.run_string(shoot, "then").unwrap();
    assert!(
        g.eval::<bool>("return Entity(2):Health() == 90 and Entity(2):GetPos().x == 40"),
        "where she saw him, she hits; he is back: {}",
        g.eval::<String>("return Entity(2):Health() .. ' ' .. tostring(Entity(2):GetPos())")
    );
    // not compensated: where he is
    g.runtime.run_string("local a = Entity(1) a:FireBullets({ Src = a:EyePos(), Dir = Vector(0, 1, 0), Damage = 10 })", "plain").unwrap();
    assert_eq!(g.eval::<i64>("return Entity(2):Health()"), 90);
    // the engine's pistol compensates by itself
    g.runtime.set_input(
        alice,
        UserCmd { view_angles: vector::Angle::new(0.0, 90.0, 0.0), buttons: crate::player::key::ATTACK, ..UserCmd::default() },
    );
    g.runtime.think(0.015);
    assert!(g.eval::<bool>("return Entity(2):Health() == 82 and Entity(2):GetPos().x == 40 and Entity(2):IsLagCompensated()"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
