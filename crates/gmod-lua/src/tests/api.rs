//! Parts of the API that stand alone: user messages, fire, the data folder seen through the
//! game's search paths, metatables scripts take over, what `util` reads of models.

use super::*;
use crate::player::ToClient;
use crate::usermessage::Part;

#[test]
fn user_messages_go_to_their_players_and_are_read_in_order() {
    let g = player_game();
    let alice = g.runtime.join(PlayerInfo { name: "Alice".into(), ..PlayerInfo::default() }).unwrap();
    g.runtime.take_to_clients();
    g.runtime
        .run_string(
            "umsg.Start('hello', Entity(1)) umsg.String('hi') umsg.Short(70000) umsg.Bool(true) umsg.Vector(Vector(1, 2, 3)) umsg.Entity(Entity(1)) umsg.End()",
            "umsg",
        )
        .unwrap();
    let sent = asked_of(&g);
    let [(to, ToClient::UserMessage(name, parts))] = sent.as_slice() else { panic!("{sent:?}") };
    assert_eq!((*to, name.as_str()), (alice, "hello"));
    assert_eq!(parts[..3], [Part::Text("hi".into()), Part::Int(70000 - 65536), Part::Bool(true)], "a short wraps");

    let client = client_game(&[]);
    client
        .runtime
        .run_string(
            "usermessage = { IncomingMessage = function(name, msg)
                 READ = { name, msg:ReadString(), msg:ReadShort(), msg:ReadBool(), msg:ReadVector(), msg:ReadEntity(), msg:ReadLong() }
                 msg:Reset() FIRST = msg:ReadString()
             end }",
            "usermessage",
        )
        .unwrap();
    client.runtime.from_server(&ToClient::UserMessage(name.clone(), parts.clone()));
    assert!(client.eval::<bool>(
        "return READ[1] == 'hello' and READ[2] == 'hi' and READ[3] == 4464 and READ[4] == true
             and READ[5] == Vector(1, 2, 3) and READ[7] == 0 and FIRST == 'hi'"
    ));
}

#[test]
fn fire_burns_until_it_is_put_out() {
    let g = player_game();
    g.runtime.join(PlayerInfo { name: "Alice".into(), ..PlayerInfo::default() }).unwrap();
    g.runtime
        .run_string(
            "HOOKS.EntityTakeDamage = function(ent, info) BURNS = (BURNS or 0) + (info:IsDamageType(DMG_BURN) and 1 or 0) end
             Entity(1):Ignite(10)",
            "ignite",
        )
        .unwrap();
    assert!(g.eval::<bool>("return Entity(1):IsOnFire() and bit.band(Entity(1):GetFlags(), FL_ONFIRE) ~= 0"));
    g.runtime.think(1.0);
    assert!(g.eval::<i64>("return BURNS") >= 4, "five times a second");
    g.runtime.run_string("Entity(1):Extinguish()", "out").unwrap();
    assert!(g.eval::<bool>("return not Entity(1):IsOnFire()"));
    // a short fire goes out by itself
    g.runtime.run_string("Entity(1):Ignite(0.1)", "short").unwrap();
    g.runtime.think(0.5);
    assert!(g.eval::<bool>("return not Entity(1):IsOnFire()"));
}

#[test]
fn the_data_folder_is_in_the_games_search_paths() {
    let g = game(Realm::Server, &[("data/stale.txt", "the mounted game's")]);
    let home = TempDir::new();
    g.runtime.set_data_dir(gmod_fs::UserDir::open(&home.0, None).unwrap());
    g.runtime
        .run_string("file.CreateDir('mine') local f = file.Open('mine/a.txt', 'wb', 'DATA') f:Write('ours') f:Close()", "write")
        .unwrap();
    assert!(g.eval::<bool>(
        "return file.Open('data/mine/a.txt', 'rb', 'GAME'):Read(4) == 'ours' and file.Exists('data/mine/a.txt', 'MOD')
             and file.Find('data/mine/*', 'GAME')[1] == 'a.txt' and not file.Exists('data/stale.txt', 'GAME')"
    ));
}

#[test]
fn metatables_scripts_take_over_are_the_values_own() {
    let g = game(Realm::Server, &[]);
    g.runtime
        .run_string(
            "local meta = FindMetaTable('CTakeDamageInfo')
             local extra = {}
             meta.__index = function(self, key) return meta[key] or (extra[self] or {})[key] end
             meta.__newindex = function(self, key, value) extra[self] = extra[self] or {} extra[self][key] = value end
             D = DamageInfo() D.Note = 'kept' D:SetDamage(5)",
            "meta",
        )
        .unwrap();
    assert!(g.eval::<bool>("return D.Note == 'kept' and D:GetDamage() == 5"));
}

#[test]
fn util_reads_what_models_say() {
    let mdl =
        gmod_model::testmodel::mdl_with_key_values(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]), "\"prop_data\" { \"base\" \"Wooden.Small\" }");
    let (vvd, vtx) = (gmod_model::testmodel::empty_vvd(1), gmod_model::testmodel::empty_vtx(1));
    let g = game_with(Realm::Server, &[("models/box.mdl", &mdl), ("models/box.vvd", &vvd), ("models/box.dx90.vtx", &vtx)]);
    assert!(g.eval::<bool>(
        "local meshes, info = util.GetModelMeshes('models/box.mdl'), util.GetModelInfo('models/box.mdl')
         return type(meshes) == 'table' and info.KeyValues:find('Wooden.Small') ~= nil and util.IsModelLoaded('models/box.mdl')
             and util.GetModelMeshes('models/none.mdl') == nil and util.GetModelInfo('models/none.mdl') == nil"
    ));
}

#[test]
fn util_hashes_and_geometry() {
    let g = game(Realm::Server, &[]);
    assert!(g.eval::<bool>(
        "return util.MD5('abc') == '900150983cd24fb0d6963f7d28e17f72'
             and util.SHA1('abc') == 'a9993e364706816aba3e25717850c26c9cd0d89d'
             and util.SHA256('abc') == 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'"
    ));
    assert!(g.eval::<bool>(
        "local pos, normal, t = util.IntersectRayWithOBB(Vector(-100, 0, 0), Vector(200, 0, 0), Vector(0, 0, 0), Angle(0, 0, 0), Vector(-10, -10, -10), Vector(10, 10, 10))
         return pos:Distance(Vector(-10, 0, 0)) < 1e-3 and normal:Distance(Vector(-1, 0, 0)) < 1e-4 and math.abs(t - 0.45) < 1e-4
             and util.IntersectRayWithOBB(Vector(-100, 50, 0), Vector(200, 0, 0), Vector(0, 0, 0), Angle(0, 0, 0), Vector(-10, -10, -10), Vector(10, 10, 10)) == nil
             and util.IsBoxIntersectingBox(Vector(0, 0, 0), Vector(2, 2, 2), Vector(1, 1, 1), Vector(3, 3, 3))
             and not util.IsSphereIntersectingSphere(Vector(0, 0, 0), 1, Vector(5, 0, 0), 1)
             and util.IsOBBIntersectingOBB(Vector(0, 0, 0), Angle(0, 45, 0), Vector(-1, -1, -1), Vector(1, 1, 1), Vector(2.3, 0, 0), Angle(0, 0, 0), Vector(-1, -1, -1), Vector(1, 1, 1))
             and not util.IsOBBIntersectingOBB(Vector(0, 0, 0), Angle(0, 0, 0), Vector(-1, -1, -1), Vector(1, 1, 1), Vector(2.5, 0, 0), Angle(0, 0, 0), Vector(-1, -1, -1), Vector(1, 1, 1))"
    ));
}

#[test]
fn blast_damage_falls_off_and_shakes_are_sent() {
    let g = player_game();
    let alice = g.runtime.join(PlayerInfo { name: "Alice".into(), ..PlayerInfo::default() }).unwrap();
    g.runtime.take_to_clients();
    let (health, gone): (i64, bool) = g.eval(
        "local p = Entity(1) local before = p:Health()
         util.BlastDamage(game.GetWorld(), game.GetWorld(), p:GetPos() + Vector(0, 0, 36) + Vector(100, 0, 0), 200, 50)
         local hurt = before - p:Health()
         util.BlastDamage(game.GetWorld(), game.GetWorld(), p:GetPos() + Vector(1000, 0, 0), 200, 50)
         return hurt, before - p:Health() == hurt",
    );
    assert!(health > 0 && health < 50 && gone, "{health}");
    g.runtime
        .run_string(
            "util.ScreenShake(Entity(1):GetPos(), 5, 5, 1, 500) util.ScreenShake(Entity(1):GetPos() + Vector(5000, 0, 0), 5, 5, 1, 500)",
            "shake",
        )
        .unwrap();
    let sent = asked_of(&g);
    assert!(matches!(sent.as_slice(), [(to, ToClient::Shake(a, ..))] if *to == alice && *a > 4.0), "{sent:?}");
}

#[test]
fn a_panel_faded_out_is_still_painted() {
    let g = client_game(&[]);
    g.runtime
        .run_string("P = vgui.Create('Panel') P:SetSize(100, 100) P:SetAlpha(0) function P:Paint() PAINTED = true end", "panel")
        .unwrap();
    g.runtime.paint(0.01);
    assert!(g.eval::<bool>("return PAINTED == true"));
}

#[test]
fn particle_systems_go_to_players_and_language_takes_a_name_alone() {
    let g = player_game();
    let alice = g.runtime.join(PlayerInfo { name: "Alice".into(), ..PlayerInfo::default() }).unwrap();
    g.runtime.take_to_clients();
    g.runtime.run_string("ParticleEffect('nitro_main_m9k', Vector(1, 2, 3), Angle(0, 0, 0), nil)", "pcf").unwrap();
    let sent = asked_of(&g);
    assert!(
        matches!(sent.as_slice(), [(to, ToClient::ParticleSystem(name, data))] if *to == alice && name == "nitro_main_m9k" && data.origin == crate::vector::Vector::new(1.0, 2.0, 3.0)),
        "{sent:?}"
    );

    let client = client_game(&[]);
    client.runtime.from_server(&ToClient::ParticleSystem("nitro_main_m9k".into(), crate::effect::EffectData::default()));
    client.runtime.drawn(EntityHandle::NULL);
    assert!(!client.runtime.take_primitives().is_empty(), "an explosion shows");
    client.runtime.run_string("language.Add('only_a_name') language.Add('named', 'Named')", "language").unwrap();
    assert!(client.eval::<bool>("return language.GetPhrase('named') == 'Named' and language.GetPhrase('only_a_name') == 'only_a_name'"));
}

#[test]
fn a_fall_hurts_and_the_corpse_goes_with_the_next_life() {
    let g = player_game();
    g.runtime.join(PlayerInfo { name: "Alice".into(), ..PlayerInfo::default() }).unwrap();
    g.runtime
        .run_string(
            "HOOKS.OnPlayerHitGround = function(ply, water, floater, speed) LANDED = speed end
             Entity(1):SetPos(Vector(0, 0, 1000))",
            "fall",
        )
        .unwrap();
    for _ in 0..200 {
        g.runtime.think(crate::timer::TICK_INTERVAL);
    }
    let (landed, health): (f64, i64) = g.eval("return LANDED, Entity(1):Health()");
    assert!(landed > 1000.0, "{landed}");
    assert_eq!(health, 90, "the gamemode said nothing: 10");
    // the corpse is there while dead, and goes as the player comes back
    g.runtime.run_string("Entity(1):Kill() Entity(1):CreateRagdoll()", "kill").unwrap();
    assert!(g.eval::<bool>("return #ents.FindByClass('hl2mp_ragdoll') == 1"));
    g.runtime.run_string("Entity(1):Spawn()", "spawn").unwrap();
    g.runtime.think(crate::timer::TICK_INTERVAL);
    assert!(g.eval::<bool>("return #ents.FindByClass('hl2mp_ragdoll') == 0 and not Entity(1):GetRagdollEntity():IsValid()"));
}
