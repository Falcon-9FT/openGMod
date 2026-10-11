//! Ragdolls: `prop_ragdoll`, the corpses of players, and what Lua does with their parts.

use super::*;

/// `models/doll.mdl`: a ragdoll of a body and an arm. Its bones are a root at the origin, the
/// body 20 above it, the arm 8 along x from the body and a hand at the arm's end, 16 along.
/// The body is a box 8 wide about its bone, of 10 kg of flesh; the arm a bar of 2 kg along its
/// x, 16 long and 4 thick, that does not twist, swings 30 degrees about y and 45 about z.
/// `models/crate.mdl` is a box 16 wide and high on its origin, of 25 kg.
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
    let crate_mdl = testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let crate_phy =
        testmodel::phy(1, &[vec![TestConvex::cuboid([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0])]], false, r#"solid { "index" "0" "mass" "25" }"#);
    vec![
        ("lua/includes/init.lua", ENTITY_LUA.as_bytes().to_vec()),
        ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes().to_vec()),
        ("models/doll.mdl", rig.files().0),
        ("models/doll.phy", testmodel::phy(7, &parts, false, text)),
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
        "local p = Entity(1) p:SetModel('models/doll.mdl') p:SetPos(Vector(0, 0, 50)) p:CreateRagdoll()
         CORPSE = p:GetRagdollEntity()
         return CORPSE:IsValid() and CORPSE:GetClass() == 'hl2mp_ragdoll' and CORPSE:GetModel() == 'models/doll.mdl'
             and CORPSE:GetPhysicsObjectCount() == 2 and CORPSE:GetPos():IsEqualTol(Vector(0, 0, 70), 0.01)"
    ));
    let corpse = g.eval::<i64>("return CORPSE:EntIndex()");
    let drawn = g.runtime.drawn(alice);
    let shown = drawn.iter().find(|d| d.entity.index.map(i64::from) == Some(corpse)).expect("the corpse is drawn");
    assert!(shown.model == "models/doll.mdl" && shown.bones.len() == 4);
    g.runtime.think(2.0);
    assert!(g.eval::<bool>("return CORPSE:GetPos().z < 6 and CORPSE:GetPhysicsObject():GetVelocity():Length() < 5"));
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
         return c:IsConstraint()"
    ));
    g.runtime.think(3.0);
    assert!(g.eval::<bool>("local a = B:GetAngles() return math.abs(a.p - 45) < 3 and math.abs(a.y) < 1 and math.abs(a.r) < 1"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
