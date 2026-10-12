//! NPCs: what the engine's own are when spawned, hurt and killed, and the `NPC` methods.

use super::*;

/// A body of a root, a body 40 above it and an arm 8 along x from the body; it idles turning
/// the body (`ACT_IDLE`, looping) and walks (`ACT_WALK`). Its physics model is a body of 10 kg
/// and an arm of 2 kg, as the doll of the ragdoll tests. Written where HL2 keeps the models of
/// citizens (every head of `group01`), zombies and Combine soldiers.
fn npc_files() -> Vec<(String, Vec<u8>)> {
    use gmod_model::testmodel::{self, TestAnimation, TestBone, TestConvex, TestRig, TestSequence};
    let bone = |name: &str, parent: i32, position: [f32; 3]| TestBone { name: name.into(), parent, position, euler: [0.0; 3] };
    let mut idle = TestSequence::new("idle", "ACT_IDLE", 0);
    idle.flags = gmod_model::anim::flags::LOOPING;
    let mut walk = TestSequence::new("walk", "ACT_WALK", 1);
    walk.flags = gmod_model::anim::flags::LOOPING;
    let rig = TestRig {
        name: "npc.mdl".into(),
        checksum: 9,
        bones: vec![bone("root", -1, [0.0; 3]), bone("body", 0, [0.0, 0.0, 40.0]), bone("arm", 1, [8.0, 0.0, 0.0])],
        animations: vec![TestAnimation::turning("@idle", 1, 31, 0.0, 0.5), TestAnimation::turning("@walk", 1, 11, 0.0, -0.5)],
        sequences: vec![idle, walk],
        ..TestRig::default()
    };
    let text = r#"solid { "index" "0" "name" "body" "mass" "10" "surfaceprop" "flesh" }
        solid { "index" "1" "name" "arm" "mass" "2" }
        ragdollconstraint { "parent" "0" "child" "1" "xmin" "0" "xmax" "0" "ymin" "-30" "ymax" "30" "zmin" "-45" "zmax" "45" }"#;
    let parts = [vec![TestConvex::cuboid([-4.0; 3], [4.0; 3])], vec![TestConvex::cuboid([0.0, -2.0, -2.0], [16.0, 2.0, 2.0])]];
    let (mdl, phy) = (rig.files().0, testmodel::phy(9, &parts, false, text));
    let mut models: Vec<String> = ["zombie/classic", "combine_soldier"].iter().map(|m| m.to_string()).collect();
    models.extend((1..=9).map(|n| format!("humans/group01/male_0{n}")));
    models.extend([1, 2, 3, 4, 6, 7].iter().map(|n| format!("humans/group01/female_0{n}")));
    let mut files = vec![
        ("lua/includes/init.lua".to_string(), ENTITY_LUA.as_bytes().to_vec()),
        ("scripts/weapons/weapon_pistol.txt".to_string(), PISTOL.as_bytes().to_vec()),
    ];
    for model in models {
        files.push((format!("models/{model}.mdl"), mdl.clone()));
        files.push((format!("models/{model}.phy"), phy.clone()));
    }
    files
}

/// A server on [`player_map`] - a floor with its top at 0 - with [`npc_files`], the players of
/// [`PLAYER_LUA`], an `info_target` named `rec` that prints the inputs it is given, and these
/// hooks.
fn npc_game(hooks: &str) -> Game {
    let files = npc_files();
    let files: Vec<(&str, &[u8])> = files.iter().map(|(path, bytes)| (path.as_str(), bytes.as_slice())).collect();
    let g = game_with(Realm::Server, &files);
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime
        .run_string(
            "HOOKS.AcceptInput = function(ent, input, activator, caller, param)
                 if ent:GetName() == 'rec' then print('rec', input, activator:IsValid() and activator:GetClass() or '-') end
             end",
            "rec",
        )
        .unwrap();
    g.runtime.run_string(hooks, "hooks").unwrap();
    g.runtime.spawn_map(&player_map());
    g.runtime.run_string("REC = ents.Create('info_target') REC:SetKeyValue('targetname', 'rec') REC:Spawn()", "rec").unwrap();
    g
}

#[test]
fn npcs_stand_and_idle_as_their_class_has_them() {
    let g = npc_game("");
    assert!(g.eval::<bool>(
        "N = ents.Create('npc_citizen') N:SetPos(Vector(0, 200, 50)) N:SetAngles(Angle(0, 45, 0))
         N:SetKeyValue('squadname', 'resistance') N:Spawn() N:Activate()
         return N:IsNPC() and N:GetClass() == 'npc_citizen' and string.find(N:GetModel(), 'models/humans/group01/', 1, true) == 1
             and N:Health() == 40 and N:GetMaxHealth() == 40
             and N:GetSequence() == 0 and N:GetActivity() == ACT_IDLE and N:GetSequenceActivity(N:GetSequence()) == ACT_IDLE
             and N:GetPos():IsEqualTol(Vector(0, 200, 0), 0.1) and N:GetAngles().y == 45
             and N:OBBMins() == Vector(-13, -13, 0) and N:OBBMaxs() == Vector(13, 13, 72)
             and N:GetSolid() == SOLID_BBOX and N:GetMoveType() == MOVETYPE_STEP and N:IsFlagSet(FL_NPC)
             and N:GetCollisionGroup() == COLLISION_GROUP_NPC
             and N:Classify() == CLASS_PLAYER_ALLY and N:GetSquad() == 'resistance'
             and ai.GetSquadMemberCount('resistance') == 1 and ai.GetSquadMembers('resistance')[1] == N
             and ai.GetSquadLeader('resistance') == N and ai.GetSquadMemberCount('combine') == 0
             and N:GetCurrentSchedule() == SCHED_IDLE_STAND and N:GetNPCState() == NPC_STATE_IDLE
             and bit.band(N:CapabilitiesGet(), CAP_USE_WEAPONS) ~= 0 and not N:GetActiveWeapon():IsValid()"
    ));
    // a class with a model of its own, and its health from the skill ConVar when one is set
    g.runtime.run_string("CreateConVar('sk_zombie_health', '75')", "skill").unwrap();
    assert!(g.eval::<bool>(
        "Z = ents.Create('npc_zombie') Z:SetPos(Vector(300, 0, 0)) Z:Spawn()
         C = ents.Create('npc_combine_s') C:SetModel('models/zombie/classic.mdl') C:SetHealth(20) C:SetPos(Vector(-300, 0, 0)) C:Spawn()
         return Z:GetModel() == 'models/zombie/classic.mdl' and Z:Health() == 75 and Z:Classify() == CLASS_ZOMBIE
             and C:GetModel() == 'models/zombie/classic.mdl' and C:Health() == 20 and C:GetMaxHealth() == 20
             and C:Classify() == CLASS_COMBINE"
    ));
    // the engine moves its idle on, round and round
    g.runtime.think(0.5);
    assert!(g.eval::<bool>("return N:GetCycle() > 0.4 and N:GetSequence() == 0"));
    g.runtime.think(0.6);
    assert!(g.eval::<bool>("return N:GetCycle() < 0.2 and N:GetSequence() == 0"));
    // removed, it goes with its weapon
    assert!(g.eval::<bool>("W = N:Give('weapon_pistol') N:Remove() return W:IsValid() and W:IsMarkedForDeletion()"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn npcs_hold_their_weapons() {
    let g = npc_game("");
    let alice = g.runtime.join(alice()).unwrap();
    assert!(g.eval::<bool>(
        "N = ents.Create('npc_citizen') N:SetPos(Vector(0, 200, 0)) N:SetKeyValue('additionalequipment', 'weapon_pistol') N:Spawn()
         W = N:GetActiveWeapon()
         return W:IsValid() and W:GetClass() == 'weapon_pistol' and W:GetOwner() == N and W:GetParent() == N
             and W:IsEffectActive(EF_BONEMERGE) and not W:IsEffectActive(EF_NODRAW) and W:GetPos() == N:GetPos()
             and N:GetWeapons()[1] == W and N:GetWeapon('weapon_pistol') == W and not N:GetWeapon('weapon_smg1'):IsValid()"
    ));
    // another in its place; the first goes
    assert!(g.eval::<bool>(
        "OLD = W W = N:Give('weapon_pistol')
         return W:IsValid() and W ~= OLD and OLD:IsMarkedForDeletion() and N:GetActiveWeapon() == W
             and not N:Give('info_target'):IsValid() and N:GetActiveWeapon() == W"
    ));
    // dropped, it is the world's
    assert!(g.eval::<bool>(
        "N:DropWeapon() return not N:GetActiveWeapon():IsValid() and W:IsValid() and not W:GetParent():IsValid()
             and not W:GetOwner():IsValid() and not W:IsEffectActive(EF_BONEMERGE)"
    ));
    let _ = alice;
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn hurt_npcs_fire_their_outputs() {
    let g = npc_game(
        "HOOKS.ScaleNPCDamage = function(npc, group, dmg) SCALED = { npc, group } dmg:ScaleDamage(2) end
         HOOKS.EntityTakeDamage = function(ent, dmg) if ent.Unhurt then return true end end",
    );
    g.runtime.join(alice()).unwrap();
    g.eval::<()>(
        "N = ents.Create('npc_citizen') N:SetPos(Vector(0, 200, 0))
         N:SetKeyValue('OnDamaged', 'rec,Hurt,,0,-1') N:SetKeyValue('OnHalfHealth', 'rec,Half,,0,-1')
         N:SetKeyValue('OnDamagedByPlayer', 'rec,ByPlayer,,0,-1') N:Spawn()",
    );
    let mut seen = g.printed().len();
    let mut taken = || {
        let printed = g.printed();
        let new: Vec<String> = printed[seen..].lines().map(str::to_string).collect();
        seen = printed.len();
        new
    };
    // a bullet is scaled by the hook, then taken; others are not scaled
    assert!(g.eval::<bool>(
        "local d = DamageInfo() d:SetDamage(5) d:SetDamageType(DMG_BULLET) d:SetAttacker(Entity(1)) d:SetInflictor(Entity(1))
         N:TakeDamageInfo(d)
         return N:Health() == 30 and SCALED[1] == N and SCALED[2] == HITGROUP_GENERIC"
    ));
    g.runtime.think(0.1);
    assert_eq!(taken(), ["rec\tHurt\tplayer", "rec\tByPlayer\tplayer"]);
    assert!(g.eval::<bool>(
        "SCALED = nil N:TakeDamage(12, game.GetWorld(), game.GetWorld())
         return N:Health() == 18 and SCALED == nil"
    ));
    g.runtime.think(0.1);
    assert_eq!(taken(), ["rec\tHurt\tworldspawn", "rec\tHalf\tworldspawn"]);
    // half health fires once; what the hook stops does nothing
    assert!(g.eval::<bool>(
        "N:TakeDamage(2, game.GetWorld(), game.GetWorld()) N.Unhurt = true N:TakeDamage(50, game.GetWorld(), game.GetWorld())
         return N:Health() == 16 and N:IsValid()"
    ));
    g.runtime.think(0.1);
    assert_eq!(taken(), ["rec\tHurt\tworldspawn"]);
    // a zombie hurt by a player takes them as its enemy
    assert!(g.eval::<bool>(
        "Z = ents.Create('npc_zombie') Z:SetPos(Vector(500, 0, 0)) Z:Spawn() Z:SetMaxLookDistance(10)
         Z:TakeDamage(1, Entity(1), Entity(1))
         return Z:GetEnemy() == Entity(1) and Z:HasEnemyMemory(Entity(1)) and Z:Health() == 49"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn dead_npcs_leave_corpses() {
    let g = npc_game(
        "HOOKS.OnNPCKilled = function(npc, attacker, inflictor) KILLED = { npc, attacker, inflictor, npc:Health() } end
         HOOKS.CreateEntityRagdoll = function(npc, ragdoll) MADE = { npc, ragdoll } end",
    );
    let alice = g.runtime.join(alice()).unwrap();
    assert!(g.eval::<bool>(
        "N = ents.Create('npc_citizen') N:SetPos(Vector(0, 200, 0)) N:SetAngles(Angle(0, 90, 0))
         N:SetKeyValue('spawnflags', SF_NPC_FADE_CORPSE) N:SetKeyValue('additionalequipment', 'weapon_pistol')
         N:SetKeyValue('OnDeath', 'rec,Died,,0,-1') N:Spawn()
         W = N:GetActiveWeapon() MODEL = N:GetModel()
         local d = DamageInfo() d:SetDamage(100) d:SetDamageType(DMG_BULLET) d:SetAttacker(Entity(1)) d:SetInflictor(W)
         d:SetDamageForce(Vector(0, 3000, 0)) d:SetDamagePosition(Vector(0, 200, 40))
         N:TakeDamageInfo(d)
         CORPSE = ents.FindByClass('class C_ClientRagdoll')[1]
         return KILLED[1] == N and KILLED[2] == Entity(1) and KILLED[3] == W and KILLED[4] <= 0
             and N:IsMarkedForDeletion() and N:GetNPCState() == NPC_STATE_DEAD
             and W:IsValid() and not W:IsMarkedForDeletion() and not W:GetParent():IsValid() and not W:GetOwner():IsValid()
             and CORPSE and CORPSE:IsValid() and CORPSE:GetModel() == MODEL and CORPSE:GetPhysicsObjectCount() == 2
             and CORPSE:GetPos():IsEqualTol(Vector(0, 200, 40), 0.1) and MADE == nil"
    ));
    // posed as it stood, and pushed by the shot
    g.runtime.think(0.1);
    assert!(g.eval::<bool>(
        "return not N:IsValid() and CORPSE:GetPhysicsObjectNum(0):GetVelocity().y > 20
             and #ents.FindByClass('npc_citizen') == 0"
    ));
    assert!(g.printed().contains("rec\tDied\tplayer"));
    let corpse = g.eval::<i64>("return CORPSE:EntIndex()");
    assert!(g.runtime.drawn(alice).iter().any(|d| d.entity.index.map(i64::from) == Some(corpse)));
    // it fades a while later
    g.runtime.think(14.0);
    assert!(g.eval::<bool>("return CORPSE:IsValid()"));
    g.runtime.think(1.5);
    assert!(g.eval::<bool>("return not CORPSE:IsValid()"));

    // the server's ragdolls, when asked for: a prop_ragdoll, told of; no weapon left with
    // SF_NPC_NO_WEAPON_DROP; a model without physics leaves none
    g.runtime.run_command("ai_serverragdolls 1");
    g.runtime.think(0.1);
    assert!(g.eval::<bool>(
        "N = ents.Create('npc_zombie') N:SetPos(Vector(0, -200, 0)) N:SetKeyValue('spawnflags', SF_NPC_NO_WEAPON_DROP) N:Spawn()
         W = N:Give('weapon_pistol')
         N:TakeDamage(500, Entity(1), Entity(1))
         return MADE[1] == N and MADE[2]:GetClass() == 'prop_ragdoll' and MADE[2]:GetPhysicsObjectCount() == 2
             and MADE[2]:GetModel() == 'models/zombie/classic.mdl' and W:IsMarkedForDeletion()"
    ));
    assert!(g.eval::<bool>(
        "MADE = nil N = ents.Create('npc_crow') N:Spawn() N:TakeDamage(5, Entity(1), Entity(1))
         return MADE == nil and N:IsMarkedForDeletion() and N:GetModel() == 'models/crow.mdl'"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn npcs_keep_relationships_and_schedules() {
    let g = npc_game("");
    g.runtime.join(alice()).unwrap();
    g.eval::<()>(
        "N = ents.Create('npc_citizen') N:SetPos(Vector(0, 200, 0)) N:Spawn()
         Z = ents.Create('npc_zombie') Z:SetPos(Vector(800, 0, 0)) Z:Spawn()
         C = ents.Create('npc_combine_s') C:SetPos(Vector(-800, 0, 0)) C:Spawn()
         P = Entity(1)",
    );
    // the sides of HL2
    assert!(g.eval::<bool>(
        "return N:Disposition(P) == D_LI and Z:Disposition(P) == D_HT and C:Disposition(P) == D_HT
             and Z:Disposition(N) == D_HT and C:Disposition(Z) == D_HT and N:Disposition(C) == D_HT
             and N:Disposition(N) == D_LI and N:Disposition(game.GetWorld()) == D_NU"
    ));
    // told otherwise: by class or name, then by entity, which comes first
    assert!(g.eval::<bool>(
        "N:AddRelationship('player D_HT 99')
         local a, b = N:Disposition(P)
         N:AddEntityRelationship(P, D_FR, 5)
         local c, d = N:Disposition(P)
         Z:SetName('bob') C:AddRelationship('bob D_LI 1 npc_citizen D_NU 0')
         return a == D_HT and b == 99 and c == D_FR and d == 5 and C:Disposition(Z) == D_LI and C:Disposition(N) == D_NU"
    ));
    // schedules, state, capabilities, and what it was told to remember
    assert!(g.eval::<bool>(
        "N:SetSchedule(SCHED_FORCED_GO)
         local forced = N:IsCurrentSchedule(SCHED_FORCED_GO) and N:GetCurrentSchedule() == SCHED_FORCED_GO
         N:ClearSchedule() N:SetNPCState(NPC_STATE_ALERT)
         N:CapabilitiesClear() N:CapabilitiesAdd(bit.bor(CAP_MOVE_GROUND, CAP_OPEN_DOORS)) N:CapabilitiesRemove(CAP_OPEN_DOORS)
         N:SetMaxLookDistance(500) N:SetMovementActivity(ACT_WALK) N:SetCurrentWeaponProficiency(WEAPON_PROFICIENCY_GOOD)
         N:SetSquad('alpha') N:SetLastPosition(Vector(1, 2, 3)) N:SetTarget(Z)
         N:SetEnemy(C) N:UpdateEnemyMemory(C, Vector(5, 6, 7))
         N:SetCondition(10)
         return forced and N:GetCurrentSchedule() == SCHED_NONE and N:GetNPCState() == NPC_STATE_ALERT
             and N:CapabilitiesGet() == CAP_MOVE_GROUND and N:GetMaxLookDistance() == 500
             and N:GetMovementActivity() == ACT_WALK and N:GetCurrentWeaponProficiency() == WEAPON_PROFICIENCY_GOOD
             and N:GetSquad() == 'alpha' and N:GetLastPosition() == Vector(1, 2, 3) and N:GetTarget() == Z
             and N:GetEnemy() == C and N:HasEnemyMemory(C) and N:GetEnemyLastKnownPos(C) == Vector(5, 6, 7)
             and N:GetKnownEnemies()[1] == C and N:HasCondition(10) and not N:HasCondition(11)
             and N:NavSetGoal(Vector(0, 0, 0)) == false and N:NavSetRandomGoal(100, Vector(1, 0, 0)) == false
             and not N:IsMoving()"
    ));
    // an activity plays its sequence
    assert!(g.eval::<bool>(
        "N:SetActivity(ACT_WALK) N:StopMoving() N:AlertSound() N:ClearEnemyMemory()
         return N:GetActivity() == ACT_WALK and N:GetSequence() == 1 and not N:GetEnemy():IsValid() and not N:HasEnemyMemory(C)
             and N:GetSquad() == 'alpha' and FindMetaTable('NPC').Give ~= nil"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn npcs_face_the_players_they_hate() {
    let g = npc_game("");
    g.runtime.join(alice()).unwrap();
    g.eval::<()>(
        "Z = ents.Create('npc_zombie') Z:SetPos(Vector(0, 300, 0)) Z:SetAngles(Angle(0, 0, 0)) Z:Spawn()
         N = ents.Create('npc_citizen') N:SetPos(Vector(-60, 300, 0)) N:SetAngles(Angle(0, 0, 0)) N:Spawn()
         BEHIND = ents.Create('npc_zombie') BEHIND:SetPos(Vector(300, 0, 0)) BEHIND:Spawn()",
    );
    g.runtime.think(1.0);
    // the zombie found the player and turned to face them; the citizen likes them; the
    // zombie behind the wall did not see them
    assert!(g.eval::<bool>(
        "return Z:GetEnemy() == Entity(1) and Z:GetNPCState() == NPC_STATE_COMBAT and math.abs(Z:GetAngles().y % 360 - 270) < 1
             and not N:GetEnemy():IsValid() and N:GetAngles().y == 0
             and not BEHIND:GetEnemy():IsValid()"
    ));
    // a player it cannot target is not found
    g.runtime.run_command("ai_ignoreplayers 1");
    assert!(g.eval::<bool>("Z:SetEnemy(NULL) Z:ClearEnemyMemory() return not Z:GetEnemy():IsValid()"));
    g.runtime.think(0.5);
    assert!(g.eval::<bool>("return not Z:GetEnemy():IsValid()"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
