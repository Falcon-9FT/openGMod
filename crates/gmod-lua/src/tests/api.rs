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
