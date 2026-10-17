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
    npc_game_on(hooks, &player_map())
}

/// [`npc_game`] on another map.
fn npc_game_on(hooks: &str, map: &Map) -> Game {
    let files = npc_files();
    let files: Vec<(&str, &[u8])> = files.iter().map(|(path, bytes)| (path.as_str(), bytes.as_slice())).collect();
    let g = game_with(Realm::Server, &files);
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime
        .run_string(
            "HOOKS.AcceptInput = function(ent, input, activator, caller, param)
                 if ent:GetName() == 'rec' then print('rec', input, (activator:IsValid() or activator:IsWorld()) and activator:GetClass() or '-') end
             end",
            "rec",
        )
        .unwrap();
    g.runtime.run_string(hooks, "hooks").unwrap();
    g.runtime.spawn_map(map);
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
    // a side given before it spawned is kept; one without a box is a point nothing runs into,
    // where it was put; a grenade is no NPC
    assert!(g.eval::<bool>(
        "R = ents.Create('npc_citizen') R:SetNPCClass(CLASS_COMBINE) R:SetPos(Vector(0, -300, 0)) R:Spawn()
         F = ents.Create('npc_enemyfinder') F:SetPos(Vector(0, -500, 50)) F:Spawn()
         G = ents.Create('npc_grenade_frag')
         return R:Classify() == CLASS_COMBINE and R:Disposition(N) == D_HT
             and F:IsNPC() and F:GetSolid() == SOLID_NONE and F:GetPos() == Vector(0, -500, 50)
             and G:IsValid() and not G:IsNPC()"
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
    // picked up again; not one that is held, nor what is no weapon
    assert!(g.eval::<bool>(
        "local took = N:PickupWeapon(W)
         return took and N:GetActiveWeapon() == W and W:GetOwner() == N and W:GetParent() == N
             and W:IsEffectActive(EF_BONEMERGE) and not N:PickupWeapon(W) and not N:PickupWeapon(REC)"
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
    // damage given directly is taken as it is, a bullet's too: ScaleNPCDamage is for where
    // a flying bullet hits
    assert!(g.eval::<bool>(
        "local d = DamageInfo() d:SetDamage(10) d:SetDamageType(DMG_BULLET) d:SetAttacker(Entity(1)) d:SetInflictor(Entity(1))
         N:TakeDamageInfo(d)
         return N:Health() == 30 and SCALED == nil"
    ));
    g.runtime.think(0.1);
    assert_eq!(taken(), ["rec\tHurt\tplayer", "rec\tByPlayer\tplayer"]);
    assert!(g.eval::<bool>(
        "N:TakeDamage(12, game.GetWorld(), game.GetWorld())
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
         N:SetKeyValue('spawnflags', bit.bor(SF_NPC_FADE_CORPSE, SF_NPC_DROP_HEALTHKIT)) N:SetKeyValue('additionalequipment', 'weapon_pistol')
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
             and CORPSE:GetPos():IsEqualTol(Vector(0, 200, 40), 0.1) and MADE == nil
             and #ents.FindByClass('item_healthvial') == 1
             and ents.FindByClass('item_healthvial')[1]:GetPos():IsEqualTol(Vector(0, 200, 16), 0.1)"
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
    // of the client's corpses that do not fade, the oldest goes past 32
    g.eval::<()>(
        "for i = 1, 33 do
             local z = ents.Create('npc_zombie') z:SetPos(Vector(-400, -600 + i * 30, 0)) z:Spawn()
             z:TakeDamage(500, Entity(1), Entity(1))
             local made = ents.FindByClass('class C_ClientRagdoll')
             if i == 1 then FIRST = made[1] end
         end
         LAST = ents.FindByClass('class C_ClientRagdoll')",
    );
    g.runtime.think(0.1);
    assert!(g.eval::<bool>(
        "return #ents.FindByClass('class C_ClientRagdoll') == 32 and not FIRST:IsValid() and #LAST >= 32
             and LAST[#LAST]:IsValid()"
    ));

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
             and N:NavSetGoal(Vector(0, 0, 0)) and N:IsMoving() and N:IsGoalActive()
             and N:GetCurrentWaypointPos() == Vector(0, 0, 0) and N:GetPathDistanceToGoal() == 200"
    ));
    // an activity plays its sequence
    assert!(g.eval::<bool>(
        "N:SetActivity(ACT_WALK) N:StopMoving() N:AlertSound() N:ClearEnemyMemory()
         return N:GetActivity() == ACT_WALK and N:GetSequence() == 1 and not N:GetEnemy():IsValid() and not N:HasEnemyMemory(C)
             and not N:IsMoving() and not N:IsGoalActive()
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
         BEHIND = ents.Create('npc_zombie') BEHIND:SetPos(Vector(300, 0, 0)) BEHIND:Spawn()
         N:AddRelationship('npc_zombie D_NU 0') Z:AddRelationship('npc_citizen D_NU 0')",
    );
    g.runtime.think(1.0);
    // the zombie found the player and turned to face them (going for them); the citizen likes
    // them, and was told to leave zombies be; the zombie behind the wall did not see them
    assert!(g.eval::<bool>(
        "return Z:GetEnemy() == Entity(1) and Z:GetNPCState() == NPC_STATE_COMBAT and math.abs(Z:GetAngles().y % 360 - 270) < 1
             and not N:GetEnemy():IsValid() and N:GetAngles().y == 0
             and not BEHIND:GetEnemy():IsValid()"
    ));
    // with ai_ignoreplayers it lets go of them, still alert, and is not turned on them by a shot
    g.runtime.run_command("ai_ignoreplayers 1");
    g.runtime.think(0.5);
    assert!(g.eval::<bool>(
        "local let_go = not Z:GetEnemy():IsValid() and Z:GetNPCState() == NPC_STATE_ALERT
         Z:TakeDamage(1, Entity(1), Entity(1))
         return let_go and not Z:GetEnemy():IsValid()"
    ));
    g.runtime.think(0.5);
    assert!(g.eval::<bool>("return not Z:GetEnemy():IsValid()"));
    // a player with no target is neither seen nor taken when shooting, and let go of
    g.runtime.run_string("Entity(1):SetNoTarget(true)", "notarget").unwrap();
    g.runtime.run_command("ai_ignoreplayers 0");
    g.runtime.think(0.5);
    assert!(g.eval::<bool>(
        "local unseen = not Z:GetEnemy():IsValid()
         Z:TakeDamage(1, Entity(1), Entity(1))
         return unseen and not Z:GetEnemy():IsValid()"
    ));
    g.runtime.run_string("Entity(1):SetNoTarget(false)", "target").unwrap();
    g.runtime.think(0.5);
    assert!(g.eval::<bool>("return Z:GetEnemy() == Entity(1)"));
    g.runtime.run_string("Entity(1):SetNoTarget(true)", "notarget").unwrap();
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return not Z:GetEnemy():IsValid()"));
    // with ai_disabled it does not turn
    g.runtime.run_string("Entity(1):SetNoTarget(false)", "target").unwrap();
    g.runtime.think(0.5);
    g.runtime.run_command("ai_disabled 1");
    g.runtime.run_string("Z:SetAngles(Angle(0, 0, 0))", "turn").unwrap();
    g.runtime.think(0.5);
    assert!(g.eval::<bool>("return Z:GetEnemy() == Entity(1) and Z:GetAngles().y == 0"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn template_npcs_go_as_the_map_loads() {
    let pairs = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
    let mut map = player_map();
    map.entities.push(pairs(&[
        ("classname", "npc_combine_s"),
        ("targetname", "template"),
        ("origin", "0 200 0"),
        ("spawnflags", "2048"),
        ("additionalequipment", "weapon_smg1"),
        ("OnFoundEnemy", "rec,Found,,0,-1"),
    ]));
    map.entities.push(pairs(&[("classname", "npc_combine_s"), ("targetname", "live"), ("origin", "0 -200 0")]));
    let g = npc_game_on("", &map);
    g.runtime.think(0.1);
    assert!(g.eval::<bool>(
        "return #ents.FindByName('template') == 0 and #ents.FindByClass('weapon_smg1') == 0
             and ents.FindByName('live')[1]:IsNPC() and #ents.FindByClass('npc_combine_s') == 1"
    ));
    // nothing of it fires when the player comes
    g.runtime.join(alice()).unwrap();
    g.runtime.think(1.0);
    assert!(g.eval::<bool>("return ents.FindByName('live')[1]:GetEnemy() == Entity(1)"));
    assert!(!g.printed().contains("rec\t"), "{}", g.printed());
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn the_client_keeps_what_a_dead_npc_dropped() {
    let server = npc_game("");
    server.eval::<()>(
        "N = ents.Create('npc_citizen') N:SetPos(Vector(0, 200, 0)) N:SetKeyValue('additionalequipment', 'weapon_pistol') N:Spawn()",
    );
    server.runtime.think(0.02);
    let client = client_game(&[]);
    client.runtime.spawn_map(&player_map());
    client.runtime.apply_snapshot(&server.runtime.snapshot());
    assert!(client.eval::<bool>(
        "N, W = ents.FindByClass('npc_citizen')[1], ents.FindByClass('weapon_pistol')[1]
         SEEN = {}
         HOOKS.EntityRemoved = function(ent) SEEN[#SEEN + 1] = 'removed ' .. ent:GetClass() end
         HOOKS.OnEntityCreated = function(ent) SEEN[#SEEN + 1] = 'created ' .. ent:GetClass() end
         return N:IsNPC() and W:GetOwner() == N and W:GetParent() == N"
    ));
    // the server drops the weapon and removes the NPC; the client lets the NPC go, not the
    // weapon the server still has
    server.eval::<()>("N:TakeDamage(100, game.GetWorld(), game.GetWorld())");
    server.runtime.think(0.02);
    client.runtime.apply_snapshot(&server.runtime.snapshot());
    assert!(client.eval::<bool>(
        "local weapon = false
         for _, what in ipairs(SEEN) do weapon = weapon or string.find(what, 'weapon_pistol', 1, true) ~= nil end
         return not N:IsValid() and W:IsValid() and ents.FindByClass('weapon_pistol')[1] == W and not W:GetOwner():IsValid()
             and not weapon and SEEN[1] == 'removed npc_citizen'"
    ));
    assert!(server.messages().is_empty(), "{:?}", server.messages());
    assert!(client.messages().is_empty(), "{:?}", client.messages());
}

/// A map for walking: a floor with its top at 0, a wall along y from x -16 to 16 with a way
/// round past y 200, a step 16 high from x 300 to 600 and y -400 to -200, a block 64 high from
/// x 300 to 600 and y 400 to 600; ground nodes either side of the way round and on the right,
/// an air node, and a spawn point far off.
fn walking_map() -> Map {
    use gmod_bsp::testmap::{TestBrush, TestMap};
    let brushes = vec![
        TestBrush::cuboid([-1024.0, -1024.0, -64.0], [1024.0, 1024.0, 0.0]),
        TestBrush::cuboid([-16.0, -1024.0, 0.0], [16.0, 200.0, 256.0]),
        TestBrush::cuboid([300.0, -400.0, 0.0], [600.0, -200.0, 16.0]),
        TestBrush::cuboid([300.0, 400.0, 0.0], [600.0, 600.0, 64.0]),
    ];
    let test = TestMap { models: vec![brushes], ..TestMap::default() };
    let bsp = gmod_bsp::Bsp::parse("walk.bsp".into(), Arc::from(test.bytes())).unwrap();
    let texture =
        |info: &gmod_bsp::TexInfo| usize::try_from(info.texdata).ok().and_then(|i| bsp.materials.get(i)).cloned().unwrap_or_default();
    let pairs = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
    Map {
        name: "gm_walking".into(),
        entities: vec![
            pairs(&[("classname", "worldspawn")]),
            pairs(&[("classname", "info_player_start"), ("origin", "-800 -800 0"), ("angles", "0 90 0")]),
            pairs(&[("classname", "info_node"), ("origin", "-150 350 8")]),
            pairs(&[("classname", "info_node"), ("origin", "150 350 8")]),
            pairs(&[("classname", "info_node_hint"), ("origin", "500 0 8"), ("hinttype", "0")]),
            pairs(&[("classname", "info_node_air"), ("origin", "0 0 400")]),
        ],
        brush_models: bsp.models.iter().map(|m| (m.mins, m.maxs)).collect(),
        collision: Some(Arc::new(gmod_bsp::Collision::new(&bsp))),
        textures: bsp.texinfo.iter().map(texture).collect(),
        visibility: gmod_bsp::Visibility::new(&bsp).map(Arc::new),
        surfaces: Default::default(),
    }
}

/// Thinks half a second at a time until `done` (Lua) is true, for at most `seconds`.
fn think_until(g: &Game, done: &str, seconds: f64) -> bool {
    let mut left = seconds;
    while left > 0.0 {
        if g.eval::<bool>(done) {
            return true;
        }
        g.runtime.think(0.5);
        left -= 0.5;
    }
    g.eval::<bool>(done)
}

#[test]
fn npcs_find_their_way_by_the_nodes() {
    // where it went: never through the wall, and how far round
    let g = npc_game_on(
        "HOOKS.Tick = function()
             if N and N:IsValid() then
                 local p = N:GetPos()
                 if p.y < 200 and math.abs(p.x) < 29 then THROUGH = true end
                 FARTHEST = math.max(FARTHEST or -1e9, p.y)
             end
         end",
        &walking_map(),
    );
    // the nodes are no entities
    assert!(g.eval::<bool>(
        "return #ents.FindByClass('info_node') == 0 and #ents.FindByClass('info_node_hint') == 0
             and #ents.FindByClass('info_node_air') == 0"
    ));
    // told to go to the other side of the wall, it goes round by the nodes
    assert!(g.eval::<bool>(
        "N = ents.Create('npc_citizen') N:SetPos(Vector(-150, -100, 0)) N:Spawn()
         N:SetLastPosition(Vector(150, -100, 0)) N:SetSchedule(SCHED_FORCED_GO_RUN)
         return N:GetCurrentSchedule() == SCHED_FORCED_GO_RUN"
    ));
    g.runtime.think(1.0);
    // the model has no run: it walks, at the speed of its own
    assert!(g.eval::<bool>(
        "return N:IsMoving() and N:IsGoalActive() and N:GetCurrentSchedule() == SCHED_FORCED_GO_RUN
             and N:GetActivity() == ACT_WALK and N:GetMovementActivity() == ACT_RUN and N:GetSequence() == 1
             and N:GetCurrentWaypointPos():IsEqualTol(Vector(-150, 350, 0), 1)
             and N:GetNextWaypointPos():IsEqualTol(Vector(150, 350, 0), 1)
             and N:GetPos().x < -100 and N:GetPos().y > -40 and N:GetPos().y < -20
             and N:GetMoveVelocity():IsEqualTol(Vector(0, 75, 0), 1) and N:GetIdealMoveSpeed() == 75
             and math.abs(N:GetPathDistanceToGoal() - ((350 - N:GetPos().y) + 300 + 450)) < 1 and math.abs(N:GetAngles().y - 90) < 1"
    ));
    assert!(think_until(&g, "return not N:IsMoving()", 20.0));
    assert!(g.eval::<bool>(
        "return N:GetPos():IsEqualTol(Vector(150, -100, 0), 13) and not N:IsGoalActive()
             and N:GetCurrentSchedule() == SCHED_IDLE_STAND and N:GetActivity() == ACT_IDLE
             and not THROUGH and FARTHEST > 300 and N:GetMoveVelocity() == Vector(0, 0, 0)"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn npcs_walk_up_steps_and_not_up_walls() {
    let g = npc_game_on("", &walking_map());
    // up a step, by a goal of Lua's
    assert!(g.eval::<bool>(
        "S = ents.Create('npc_citizen') S:SetPos(Vector(250, -300, 0)) S:Spawn()
         return S:NavSetGoal(Vector(450, -300, 16))"
    ));
    assert!(think_until(&g, "return not S:IsMoving()", 6.0));
    assert!(g.eval::<bool>("return S:GetPos():IsEqualTol(Vector(450, -300, 16), 13) and math.abs(S:GetPos().z - 16) < 0.1"));
    // not onto a block a man's height, nor into a wall; what it could not get to it remembers
    assert!(g.eval::<bool>(
        "return not S:NavSetGoal(Vector(450, 500, 64)) and not S:NavSetGoal(Vector(0, -500, 50)) and not S:IsGoalActive()
             and not S:IsUnreachable(game.GetWorld())"
    ));
    // wandering: to the hint node, the only one in reach, then it stands
    assert!(g.eval::<bool>(
        "S:SetSchedule(SCHED_IDLE_WANDER)
         return S:GetCurrentSchedule() == SCHED_IDLE_WANDER"
    ));
    g.runtime.think(0.1);
    assert!(g.eval::<bool>("return S:IsMoving() and S:GetCurrentWaypointPos():IsEqualTol(Vector(500, 0, 0), 1)"));
    assert!(think_until(&g, "return S:GetCurrentSchedule() == SCHED_IDLE_STAND", 8.0));
    assert!(g.eval::<bool>("return S:GetPos():IsEqualTol(Vector(500, 0, 0), 13) and not S:IsMoving()"));
    // one that does not move goes nowhere; StopMoving stops one that does
    assert!(g.eval::<bool>(
        "T = ents.Create('npc_turret_floor') T:SetPos(Vector(-500, -500, 0)) T:Spawn()
         local fixed = not T:NavSetGoal(Vector(-400, -500, 0)) and T:GetNavType() == NAV_NONE
         S:NavSetGoalPos(Vector(500, 300, 0)) local going = S:IsMoving()
         S:StopMoving()
         return fixed and going and not S:IsMoving() and S:GetNavType() == NAV_GROUND"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn zombies_go_for_players_and_strike() {
    let g = npc_game(
        "HOOKS.EntityTakeDamage = function(target, info)
             if target:IsPlayer() then
                 HURT = (HURT or 0) + info:GetDamage() BY = info:GetAttacker() KIND = info:GetDamageType()
             end
         end",
    );
    g.runtime.join(alice()).unwrap();
    g.eval::<()>("Z = ents.Create('npc_zombie') Z:SetPos(Vector(0, 400, 0)) Z:Spawn()");
    g.runtime.think(1.0);
    assert!(g.eval::<bool>(
        "return Z:GetEnemy() == Entity(1) and Z:GetCurrentSchedule() == SCHED_CHASE_ENEMY and Z:IsMoving()
             and Z:GetPos().y < 350 and Z:GetActivity() == ACT_WALK and HURT == nil"
    ));
    assert!(think_until(&g, "return HURT ~= nil", 8.0));
    assert!(g.eval::<bool>(
        "return HURT == 10 and BY == Z and KIND == DMG_SLASH and Entity(1):Health() == 90
             and Z:GetCurrentSchedule() == SCHED_MELEE_ATTACK1 and not Z:IsMoving()
             and Z:GetPos():Distance(Entity(1):GetPos()) < 72"
    ));
    // a skill ConVar sets how hard, and it strikes again a while later
    g.runtime.run_string("CreateConVar('sk_zombie_dmg_one_slash', '25')", "skill").unwrap();
    assert!(think_until(&g, "return HURT > 10", 3.0));
    assert!(g.eval::<bool>("return HURT == 35 and Entity(1):Health() == 65"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn soldiers_shoot_in_bursts_and_reload() {
    let g = npc_game(
        "HOOKS.EntityTakeDamage = function(target, info)
             if target:IsPlayer() then HURT = (HURT or 0) + info:GetDamage() BY = info:GetAttacker() KIND = info:GetDamageType() end
         end
         HOOKS.EntityFireBullets = function(shooter, bullets)
             if shooter == C then SHOTS = (SHOTS or 0) + 1 FROM = bullets.Src end
         end",
    );
    g.runtime.join(alice()).unwrap();
    g.eval::<()>(
        "C = ents.Create('npc_combine_s') C:SetPos(Vector(0, 600, 0)) C:SetAngles(Angle(0, -90, 0)) C:Spawn()
         W = C:Give('weapon_pistol') C:SetCurrentWeaponProficiency(WEAPON_PROFICIENCY_PERFECT) W:SetClip1(1)",
    );
    // it sees the player, shoots its round and reloads, standing where it is
    g.runtime.think(0.5);
    assert!(g.eval::<bool>(
        "return C:GetEnemy() == Entity(1) and SHOTS == 1 and W:Clip1() == 0 and C:GetCurrentSchedule() == SCHED_RELOAD
             and FROM == C:GetShootPos() and C:GetPos() == Vector(0, 600, 0)"
    ));
    g.runtime.think(1.5);
    assert!(g.eval::<bool>("return W:Clip1() > 0 and W:Clip1() <= 18"));
    // and shoots on in bursts, the player hurt by its bullets
    g.runtime.think(4.0);
    assert!(g.eval::<bool>(
        "return SHOTS >= 6 and SHOTS <= 17 and HURT ~= nil and HURT % 3 == 0 and BY == C and KIND == DMG_BULLET
             and C:GetCurrentSchedule() == SCHED_RANGE_ATTACK1 and C:GetPos() == Vector(0, 600, 0)"
    ));
    // not through a friend: it goes to shoot from elsewhere
    g.eval::<()>("F = ents.Create('npc_combine_s') F:SetPos(Vector(0, 300, 0)) F:Spawn() SHOTS = 0");
    g.runtime.think(0.3);
    assert!(g.eval::<bool>("return SHOTS == 0 and C:GetCurrentSchedule() == SCHED_CHASE_ENEMY and C:IsMoving()"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn npcs_fight_each_other() {
    let g = npc_game("");
    g.eval::<()>(
        "R = ents.Create('npc_citizen') R:SetPos(Vector(-500, -500, 0)) R:Spawn() R:Give('weapon_pistol')
         R:SetCurrentWeaponProficiency(WEAPON_PROFICIENCY_PERFECT)
         Z = ents.Create('npc_zombie') Z:SetPos(Vector(-500, -100, 0)) Z:Spawn()",
    );
    g.runtime.think(3.0);
    assert!(g.eval::<bool>(
        "return R:GetEnemy() == Z and Z:GetEnemy() == R and Z:Health() < 50 and Z:Health() % 3 == 2
             and R:GetCurrentSchedule() == SCHED_RANGE_ATTACK1 and Z:GetPos().y < -200"
    ));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
