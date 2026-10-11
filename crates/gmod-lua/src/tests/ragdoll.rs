//! Ragdolls: `prop_ragdoll`, the corpses of players, and what Lua does with their parts.

use super::*;

/// `models/doll.mdl`: a ragdoll of a body and an arm. Its bones are a root at the origin, the
/// body 20 above it, the arm 8 along x from the body and a hand at the arm's end, 16 along.
/// The body is a box 8 wide about its bone, of 10 kg of flesh; the arm a bar of 2 kg along its
/// x, 16 long and 4 thick, that does not twist, swings 30 degrees about y and 45 about z.
/// `models/crate.mdl` is a box 16 wide and high on its origin, of 25 kg.
///
/// `models/welded.mdl` has a body, a chest welded onto it 10 above (a joint that lets nothing
/// turn), an arm off the chest as the doll's and a leg 10 below the body, swinging about y.
/// Its bones are root, body, chest, arm, hand and leg; its solids body, chest, arm and leg, of
/// 10, 5, 2 and 3 kg. `models/apart.mdl` is the same with `"selfcollisions" "0"`.
fn ragdoll_files() -> Vec<(&'static str, Vec<u8>)> {
    use gmod_model::testmodel::{self, TestBone, TestConvex, TestRig};
    let bone = |name: &str, parent: i32, position: [f32; 3]| TestBone { name: name.into(), parent, position, euler: [0.0; 3] };
    let rig = TestRig {
        name: "doll.mdl".into(),
        checksum: 7,
        bones: vec![
            bone("root", -1, [0.0; 3]),
            bone("body", 0, [0.0, 0.0, 20.0]),
            bone("arm", 1, [8.0, 0.0, 0.0]),
            bone("hand", 2, [16.0, 0.0, 0.0]),
        ],
        ..TestRig::default()
    };
    let text = r#"solid { "index" "0" "name" "body" "mass" "10" "surfaceprop" "flesh" }
        solid { "index" "1" "name" "arm" "mass" "2" "damping" "0.5" "rotdamping" "2" "inertia" "2" }
        ragdollconstraint { "parent" "0" "child" "1" "xmin" "0" "xmax" "0" "ymin" "-30" "ymax" "30" "zmin" "-45" "zmax" "45" }"#;
    let parts = [vec![TestConvex::cuboid([-4.0; 3], [4.0; 3])], vec![TestConvex::cuboid([0.0, -2.0, -2.0], [16.0, 2.0, 2.0])]];
    let welded = TestRig {
        name: "welded.mdl".into(),
        checksum: 8,
        bones: vec![
            bone("root", -1, [0.0; 3]),
            bone("body", 0, [0.0, 0.0, 20.0]),
            bone("chest", 1, [0.0, 0.0, 10.0]),
            bone("arm", 2, [8.0, 0.0, 0.0]),
            bone("hand", 3, [16.0, 0.0, 0.0]),
            bone("leg", 1, [0.0, 0.0, -10.0]),
        ],
        ..TestRig::default()
    };
    let welded_text = |rules: &str| {
        format!(
            r#"solid {{ "index" "0" "name" "body" "mass" "10" }}
            solid {{ "index" "1" "name" "chest" "mass" "5" }}
            solid {{ "index" "2" "name" "arm" "mass" "2" }}
            solid {{ "index" "3" "name" "leg" "mass" "3" }}
            ragdollconstraint {{ "parent" "0" "child" "1" "xmin" "0" "xmax" "0" "ymin" "0" "ymax" "0" "zmin" "0" "zmax" "0" }}
            ragdollconstraint {{ "parent" "1" "child" "2" "xmin" "0" "xmax" "0" "ymin" "-30" "ymax" "30" "zmin" "-45" "zmax" "45" }}
            ragdollconstraint {{ "parent" "0" "child" "3" "xmin" "0" "xmax" "0" "ymin" "-20" "ymax" "20" "zmin" "0" "zmax" "0" }}
            {rules}"#
        )
    };
    let welded_parts = [
        vec![TestConvex::cuboid([-4.0; 3], [4.0; 3])],
        vec![TestConvex::cuboid([-4.0, -4.0, -3.0], [4.0, 4.0, 3.0])],
        vec![TestConvex::cuboid([0.0, -2.0, -2.0], [16.0, 2.0, 2.0])],
        vec![TestConvex::cuboid([-2.0, -2.0, -8.0], [2.0, 2.0, 0.0])],
    ];
    let crate_mdl = testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let crate_phy =
        testmodel::phy(1, &[vec![TestConvex::cuboid([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0])]], false, r#"solid { "index" "0" "mass" "25" }"#);
    vec![
        ("lua/includes/init.lua", ENTITY_LUA.as_bytes().to_vec()),
        ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes().to_vec()),
        ("models/doll.mdl", rig.files().0),
        ("models/doll.phy", testmodel::phy(7, &parts, false, text)),
        ("models/welded.mdl", welded.files().0),
        ("models/welded.phy", testmodel::phy(8, &welded_parts, false, &welded_text(""))),
        ("models/apart.mdl", TestRig { name: "apart.mdl".into(), ..welded }.files().0),
        ("models/apart.phy", testmodel::phy(8, &welded_parts, false, &welded_text(r#"collisionrules { "selfcollisions" "0" }"#))),
        ("models/crate.mdl", crate_mdl),
        ("models/crate.phy", crate_phy),
    ]
}

/// A server on [`player_map`] - a floor with its top at 0 - with [`ragdoll_files`], the
/// players of [`PLAYER_LUA`] and these hooks.
fn ragdoll_game(hooks: &str) -> Game {
    let files = ragdoll_files();
    let files: Vec<(&str, &[u8])> = files.iter().map(|(path, bytes)| (*path, bytes.as_slice())).collect();
    let g = game_with(Realm::Server, &files);
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime.run_string(hooks, "hooks").unwrap();
    g.runtime.spawn_map(&player_map());
    g
}

#[test]
fn prop_ragdolls_hang_together_and_fall() {
    let g = ragdoll_game("");
    // a body for each solid, where its bone is; the entity where the first is
    assert!(g.eval::<bool>(
        "R = ents.Create('prop_ragdoll') R:SetModel('models/doll.mdl') R:SetPos(Vector(0, 0, 100)) R:Spawn()
         BODY, ARM = R:GetPhysicsObjectNum(0), R:GetPhysicsObjectNum(1)
         return util.IsValidRagdoll('models/doll.mdl') and R:IsRagdoll() and R:GetPhysicsObjectCount() == 2
             and BODY == R:GetPhysicsObject() and ARM:IsValid() and ARM:GetEntity() == R and ARM ~= BODY
             and not R:GetPhysicsObjectNum(2):IsValid() and not R:GetPhysicsObjectNum(-1):IsValid()
             and math.abs(BODY:GetMass() - 10) < 0.01 and math.abs(ARM:GetMass() - 2) < 0.01 and BODY:GetMaterial() == 'flesh'
             and R:TranslatePhysBoneToBone(0) == 1 and R:TranslatePhysBoneToBone(1) == 2 and R:TranslatePhysBoneToBone(5) == -1
             and R:TranslateBoneToPhysBone(3) == 1 and R:TranslateBoneToPhysBone(1) == 0 and R:TranslateBoneToPhysBone(0) == -1
             and R:GetPos():IsEqualTol(Vector(0, 0, 120), 0.01) and BODY:GetPos():IsEqualTol(Vector(0, 0, 120), 0.01)
             and ARM:GetPos():IsEqualTol(Vector(8, 0, 120), 0.01) and R:GetBonePosition(3):IsEqualTol(Vector(24, 0, 120), 0.01)
             and R:OBBMins():IsEqualTol(Vector(-4, -4, -4), 0.1) and R:OBBMaxs():IsEqualTol(Vector(24, 4, 4), 0.1)"
    ));
    // held by its body, the arm hangs as far down as it may turn, and the hand at its end
    assert!(g.eval::<bool>("BODY:EnableMotion(false) ARM:Wake() return not BODY:IsMotionEnabled() and ARM:IsMotionEnabled()"));
    g.runtime.think(3.0);
    assert!(g.eval::<bool>(
        "local a = ARM:GetAngles()
         return ARM:GetPos():IsEqualTol(Vector(8, 0, 120), 0.5) and math.abs(a.p - 30) < 2 and math.abs(a.y) < 1 and math.abs(a.r) < 1
             and R:GetBonePosition(2):IsEqualTol(ARM:GetPos(), 0.05) and R:GetBonePosition(3):IsEqualTol(ARM:GetPos() + a:Forward() * 16, 0.05)
             and R:GetPos():IsEqualTol(Vector(0, 0, 120), 0.01)"
    ));
    // let go, it falls and lies on the floor; the entity goes with its body
    assert!(g.eval::<bool>("BODY:EnableMotion(true) R:PhysWake() return BODY:IsMotionEnabled()"));
    g.runtime.think(4.0);
    assert!(g.eval::<bool>(
        "return R:GetPos():IsEqualTol(BODY:GetPos(), 0.01) and BODY:GetPos().z > 3 and BODY:GetPos().z < 6
             and ARM:GetPos().z > 0 and ARM:LocalToWorld(Vector(16, 0, 0)).z > -0.5
             and BODY:GetVelocity():Length() < 5 and ARM:GetVelocity():Length() < 5"
    ));
    // a ray says which part it hits; a part of the world moves alone, the entity all of them
    assert!(g.eval::<bool>(
        "local function down(p) return util.TraceLine({ start = p + Vector(0, 0, 50), endpos = p - Vector(0, 0, 50) }) end
         local arm, body = down(ARM:LocalToWorld(Vector(12, 0, 0))), down(BODY:GetPos())
         return arm.Entity == R and arm.PhysicsBone == 1 and body.Entity == R and body.PhysicsBone == 0
             and util.TraceLine({ start = Vector(0, 300, 50), endpos = Vector(0, 300, -50) }).PhysicsBone == 0"
    ));
    assert!(g.eval::<bool>(
        "local offset = ARM:GetPos() - BODY:GetPos()
         R:SetPos(Vector(300, 0, 200))
         local moved = BODY:GetPos():IsEqualTol(Vector(300, 0, 200), 0.01) and (ARM:GetPos() - BODY:GetPos()):IsEqualTol(offset, 0.01)
         ARM:SetPos(ARM:GetPos() + Vector(0, 0, 10))
         return moved and BODY:GetPos():IsEqualTol(Vector(300, 0, 200), 0.01) and R:GetPos():IsEqualTol(Vector(300, 0, 200), 0.01)
             and (ARM:GetPos() - BODY:GetPos()):IsEqualTol(offset + Vector(0, 0, 10), 0.01)"
    ));
    g.runtime.run_string("R:Remove()", "remove").unwrap();
    g.runtime.think(0.02);
    assert!(g.eval::<bool>("return not BODY:IsValid() and not ARM:IsValid()"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn ragdolls_spawned_as_sandbox_spawns_them() {
    let g = ragdoll_game("");
    // GMODSpawnRagdoll and DoPlayerEntitySpawn of sandbox: laid on its back where the player
    // looks, then moved up out of the floor part by part
    assert!(g.eval::<bool>(
        "local tr = { HitPos = Vector(0, 0, 0), HitNormal = Vector(0, 0, 1) }
         if not util.IsValidRagdoll('models/doll.mdl') then return false end
         local e = ents.Create('prop_ragdoll')
         e:SetModel('models/doll.mdl') e:SetSkin(0) e:SetAngles(Angle(-90, 180, 0)) e:SetPos(tr.HitPos) e:Spawn() e:Activate()
         local flush = e:NearestPoint(tr.HitPos - tr.HitNormal * 512)
         flush = tr.HitPos + (e:GetPos() - flush)
         local offset = flush - e:GetPos()
         for i = 0, e:GetPhysicsObjectCount() - 1 do
             local phys = e:GetPhysicsObjectNum(i)
             phys:SetPos(phys:GetPos() + offset)
         end
         SPAWNED = e
         return e:GetPos():IsEqualTol(Vector(4, 0, 4), 0.1) and e:GetPhysicsObjectNum(1):GetPos():IsEqualTol(Vector(4, 0, 12), 0.1)"
    ));
    // it lies there, its arm up from its body no farther over than the joint lets it
    g.runtime.think(3.0);
    assert!(g.eval::<bool>(
        "local body, arm = SPAWNED:GetPhysicsObjectNum(0), SPAWNED:GetPhysicsObjectNum(1)
         local up = arm:GetAngles():Forward():Dot(body:GetAngles():Forward())
         return math.abs(body:GetPos().z - 4) < 1 and arm:GetPos().z > 4 and up > math.cos(math.rad(46))
             and arm:GetVelocity():Length() < 5 and body:GetVelocity():Length() < 5"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn players_leave_their_corpses() {
    let g = ragdoll_game("");
    let alice = g.runtime.join(alice()).unwrap();
    assert!(g.eval::<bool>(
        "CRATE = ents.Create('prop_physics') CRATE:SetModel('models/crate.mdl') CRATE:SetPos(Vector(0, 0, 0)) CRATE:Spawn()
         local p = Entity(1) p:SetModel('models/doll.mdl') p:SetPos(Vector(0, 0, 50)) p:CreateRagdoll()
         CORPSE = p:GetRagdollEntity()
         return CORPSE:IsValid() and CORPSE:GetClass() == 'hl2mp_ragdoll' and CORPSE:GetModel() == 'models/doll.mdl'
             and CORPSE:GetPhysicsObjectCount() == 2 and CORPSE:GetPos():IsEqualTol(Vector(0, 0, 70), 0.01)"
    ));
    let corpse = g.eval::<i64>("return CORPSE:EntIndex()");
    let drawn = g.runtime.drawn(alice);
    let shown = drawn.iter().find(|d| d.entity.index.map(i64::from) == Some(corpse)).expect("the corpse is drawn");
    assert!(shown.model == "models/doll.mdl" && shown.bones.len() == 4);
    // it lies on the world, through the crate, which feels nothing
    g.runtime.think(0.5);
    assert!(g.eval::<bool>("return CORPSE:GetPos().z < 12"));
    g.runtime.think(1.5);
    assert!(g.eval::<bool>(
        "return CORPSE:GetPos().z < 6 and CORPSE:GetPhysicsObject():GetVelocity():Length() < 5
             and CRATE:GetPos():IsEqualTol(Vector(0, 0, 0), 0.02) and CRATE:GetVelocity():Length() < 0.1"
    ));
    // another takes its place
    assert!(g.eval::<bool>("local old = CORPSE Entity(1):CreateRagdoll() return Entity(1):GetRagdollEntity() ~= old"));
    g.runtime.think(0.02);
    assert!(g.eval::<bool>("return not CORPSE:IsValid() and Entity(1):GetRagdollEntity():IsValid()"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn parts_are_taken_and_joined_alone() {
    use player::key::{ATTACK, ATTACK2};
    let g = ragdoll_game(
        "HOOKS.PlayerSpawn = function(p) p:Give('weapon_physgun') end
         HOOKS.PhysgunPickup = function(p, e) return true end
         HOOKS.OnPhysgunFreeze = function(w, phys, e, p) FROZE = phys phys:EnableMotion(false) end",
    );
    // a ragdoll frozen in the air before the player's eyes, its arm along x
    assert!(g.eval::<bool>(
        "R = ents.Create('prop_ragdoll') R:SetModel('models/doll.mdl') R:SetPos(Vector(0, 100, 44)) R:Spawn()
         BODY, ARM = R:GetPhysicsObjectNum(0), R:GetPhysicsObjectNum(1)
         BODY:EnableMotion(false) ARM:EnableMotion(false)
         local arm = util.TraceLine({ start = Vector(16, 0, 64), endpos = Vector(16, 200, 64) })
         local body = util.TraceLine({ start = Vector(0, 0, 64), endpos = Vector(0, 200, 64) })
         local hull = util.TraceHull({ start = Vector(16, 0, 64), endpos = Vector(16, 200, 64), mins = Vector(-1, -1, -1), maxs = Vector(1, 1, 1) })
         return arm.Entity == R and arm.PhysicsBone == 1 and math.abs(arm.HitPos.y - 98) < 0.1 and arm.HitNormal:IsEqualTol(Vector(0, -1, 0), 0.01)
             and body.Entity == R and body.PhysicsBone == 0 and math.abs(body.HitPos.y - 96) < 0.1 and hull.Entity == R"
    ));
    // the gun takes the arm alone, and freezes it alone
    let alice = g.runtime.join(alice()).unwrap();
    let yaw = 100.0f32.atan2(16.0).to_degrees();
    g.runtime.set_input(alice, aiming(0.0, yaw, ATTACK));
    g.runtime.think(0.02);
    assert!(g.runtime.view(alice).unwrap().holding);
    assert!(g.eval::<bool>("return ARM:IsMotionEnabled() and not BODY:IsMotionEnabled()"));
    g.runtime.set_input(alice, aiming(0.0, yaw, ATTACK | ATTACK2));
    g.runtime.think(0.02);
    assert!(g.eval::<bool>("return FROZE == ARM and not ARM:IsMotionEnabled() and not BODY:IsMotionEnabled()"));
    // welded to the world by its arm, as constraint.Weld with a bone does, the body hangs
    // from the arm as far down as the joint lets it
    assert!(g.eval::<bool>(
        "local world = game.GetWorld()
         local c = ents.Create('phys_constraint')
         c:SetPhysConstraintObjects(R:GetPhysicsObjectNum(1), world:GetPhysicsObject()) c:Spawn() c:Activate()
         local a, b = c:GetConstrainedPhysObjects()
         local first, second = c:GetConstrainedEntities()
         ARM:EnableMotion(true) BODY:EnableMotion(true) R:PhysWake()
         return a == ARM and b == world:GetPhysicsObject() and first == R and second == world"
    ));
    g.runtime.set_input(alice, aiming(0.0, yaw, 0));
    g.runtime.think(2.0);
    assert!(g.eval::<bool>(
        "local joint = Vector(8, 100, 64)
         return ARM:GetPos():IsEqualTol(joint, 0.5) and math.abs(ARM:GetAngles().p) < 2
             and math.abs(BODY:GetPos():Distance(joint) - 8) < 0.5 and BODY:GetPos().z < 64 - 3"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn ragdoll_constraints_of_lua() {
    let g = ragdoll_game("");
    // a crate hangs by its side from a frozen one, turning about y as far as 45 degrees
    assert!(g.eval::<bool>(
        "local function crate(pos) local c = ents.Create('prop_physics') c:SetModel('models/crate.mdl') c:SetPos(pos) c:Spawn() return c end
         A = crate(Vector(-300, 0, 200)) B = crate(Vector(-280, 0, 200))
         A:GetPhysicsObject():EnableMotion(false)
         local c = ents.Create('phys_ragdollconstraint') c:SetPos(Vector(-290, 0, 208))
         for k, v in pairs({ xmin = 0, xmax = 0, ymin = -45, ymax = 45, zmin = 0, zmax = 0, spawnflags = 1 }) do c:SetKeyValue(k, v) end
         c:SetPhysConstraintObjects(A:GetPhysicsObject(), B:GetPhysicsObject()) c:Spawn() c:Activate()
         C = crate(Vector(-300, 100, 200)) D = crate(Vector(-280, 100, 200))
         C:GetPhysicsObject():EnableMotion(false)
         local free = ents.Create('phys_ragdollconstraint') free:SetPos(Vector(-290, 100, 208))
         for k, v in pairs({ xmin = 0, xmax = 0, ymin = 0, ymax = 0, zmin = 0, zmax = 0, spawnflags = 3 }) do free:SetKeyValue(k, v) end
         free:SetPhysConstraintObjects(C:GetPhysicsObject(), D:GetPhysicsObject()) free:Spawn() free:Activate()
         return c:IsConstraint() and free:IsConstraint()"
    ));
    g.runtime.think(3.0);
    assert!(g.eval::<bool>("local a = B:GetAngles() return math.abs(a.p - 45) < 3 and math.abs(a.y) < 1 and math.abs(a.r) < 1"));
    // "only rotation" (2): it falls away, but does not turn
    assert!(
        g.eval::<bool>("local a = D:GetAngles() return D:GetPos().z < 1 and math.abs(a.p) < 1 and math.abs(a.y) < 1 and math.abs(a.r) < 1")
    );
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn welded_parts_are_where_their_bones_are() {
    let g = ragdoll_game("");
    // the chest welded onto the body is where its bone is, and so is its centre of mass
    assert!(g.eval::<bool>(
        "R = ents.Create('prop_ragdoll') R:SetModel('models/welded.mdl') R:SetPos(Vector(0, 0, 100)) R:Spawn()
         BODY, CHEST, ARM, LEG = R:GetPhysicsObjectNum(0), R:GetPhysicsObjectNum(1), R:GetPhysicsObjectNum(2), R:GetPhysicsObjectNum(3)
         return R:GetPhysicsObjectCount() == 4 and CHEST:IsValid() and R:TranslatePhysBoneToBone(1) == 2
             and BODY:GetPos():IsEqualTol(Vector(0, 0, 120), 0.01) and CHEST:GetPos():IsEqualTol(Vector(0, 0, 130), 0.01)
             and CHEST:GetPos():IsEqualTol(R:GetBonePosition(2), 0.01) and ARM:GetPos():IsEqualTol(Vector(8, 0, 130), 0.01)
             and LEG:GetPos():IsEqualTol(Vector(0, 0, 110), 0.01)
             and CHEST:LocalToWorld(CHEST:GetMassCenter()):IsEqualTol(BODY:LocalToWorld(BODY:GetMassCenter()), 0.01)"
    ));
    // DoPlayerEntitySpawn of sandbox moves every part by the offset: each goes by it once
    assert!(g.eval::<bool>(
        "local before, offset = {}, Vector(0, 50, 7)
         for i = 0, R:GetPhysicsObjectCount() - 1 do before[i] = R:GetPhysicsObjectNum(i):GetPos() end
         for i = 0, R:GetPhysicsObjectCount() - 1 do
             local phys = R:GetPhysicsObjectNum(i)
             phys:SetPos(phys:GetPos() + offset)
         end
         for i = 0, R:GetPhysicsObjectCount() - 1 do
             if not R:GetPhysicsObjectNum(i):GetPos():IsEqualTol(before[i] + offset, 0.01) then return false end
         end
         return R:GetPos():IsEqualTol(Vector(0, 50, 127), 0.01)"
    ));
    // put by itself, the chest stays on the body; turning the body turns it along
    assert!(g.eval::<bool>(
        "CHEST:SetPos(Vector(500, 500, 500)) CHEST:SetAngles(Angle(45, 0, 0))
         local stayed = CHEST:GetPos():IsEqualTol(Vector(0, 50, 137), 0.01) and math.abs(CHEST:GetAngles().p) < 0.01
         BODY:SetAngles(Angle(0, 90, 0))
         return stayed and BODY:GetPos():IsEqualTol(Vector(0, 50, 127), 0.01) and CHEST:GetPos():IsEqualTol(Vector(0, 50, 137), 0.01)
             and math.abs(CHEST:GetAngles().y - 90) < 0.01 and ARM:GetPos():IsEqualTol(Vector(8, 50, 137), 0.01)"
    ));
    // without rules every part hits every other but those joined to it; "selfcollisions" "0"
    // lets none
    g.eval::<()>("APART = ents.Create('prop_ragdoll') APART:SetModel('models/apart.mdl') APART:SetPos(Vector(200, 0, 100)) APART:Spawn()");
    let lua = g.runtime.lua();
    for (name, arm_hits_leg) in [("R", true), ("APART", false)] {
        let index = g.eval::<u32>(&format!("return {name}:EntIndex()"));
        let handle = crate::host(lua).entities.at(index).unwrap();
        let part = |i: usize| crate::physics::part_body(lua, handle, i).unwrap();
        let (body, chest, arm, leg) = (part(0), part(1), part(2), part(3));
        let host = crate::host(lua);
        let physics = host.physics.physics.borrow();
        assert!(body == chest && physics.apart(body, arm) && physics.apart(body, leg), "{name}");
        assert_eq!(!physics.apart(arm, leg), arm_hits_leg, "{name}");
    }
    // it falls and lies together
    g.runtime.think(3.0);
    assert!(g.eval::<bool>(
        "return CHEST:GetPos():Distance(BODY:GetPos()) < 10.01 and CHEST:GetPos():Distance(BODY:GetPos()) > 9.99
             and LEG:GetPos().z > 0 and ARM:GetPos().z > 0 and BODY:GetVelocity():Length() < 5"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn map_ragdolls_keep_their_flags() {
    let g = ragdoll_game("");
    // "motion disabled" (16384) with "debris" (4), "start asleep" (65536); a prop's 8 and 1
    // are nothing to a ragdoll
    assert!(g.eval::<bool>(
        "local function doll(pos, flags)
             local r = ents.Create('prop_ragdoll') r:SetModel('models/doll.mdl') r:SetPos(pos) r:SetKeyValue('spawnflags', flags) r:Spawn()
             return r
         end
         STILL, SLEEPY, LOOSE = doll(Vector(0, 0, 100), '16388'), doll(Vector(0, 100, 100), '65536'), doll(Vector(0, 200, 100), '9')
         return not STILL:GetPhysicsObjectNum(0):IsMotionEnabled() and not STILL:GetPhysicsObjectNum(1):IsMotionEnabled()
             and STILL:GetCollisionGroup() == COLLISION_GROUP_DEBRIS and SLEEPY:GetPhysicsObjectNum(1):IsAsleep()
             and SLEEPY:GetCollisionGroup() == COLLISION_GROUP_NONE and LOOSE:GetPhysicsObjectNum(0):IsMotionEnabled()
             and not LOOSE:GetPhysicsObjectNum(0):IsAsleep()"
    ));
    g.runtime.think(1.0);
    assert!(g.eval::<bool>(
        "return STILL:GetPos():IsEqualTol(Vector(0, 0, 120), 0.01) and SLEEPY:GetPos():IsEqualTol(Vector(0, 100, 120), 0.01)
             and LOOSE:GetPos().z < 10"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
