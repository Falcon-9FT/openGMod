//! A server of several players: their slots, who the server's messages go to.

use super::*;

/// A server for `max` players with the players' map, before anyone joins.
fn server_for(max: u32) -> Game {
    let mdl = gmod_model::testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let g = game_with(
        Realm::Server,
        &[
            ("lua/includes/init.lua", ENTITY_LUA.as_bytes()),
            ("models/box.mdl", &mdl),
            ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes()),
        ],
    );
    g.runtime.set_max_players(max);
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime.spawn_map(&player_map());
    g
}

fn named(name: &str) -> PlayerInfo {
    PlayerInfo { name: name.into(), ..PlayerInfo::default() }
}

#[test]
fn players_take_the_slots_the_server_has() {
    let g = server_for(3);
    assert!(g.eval::<bool>("return game.MaxPlayers() == 3 and not game.SinglePlayer() and not game.IsDedicated()"));
    // the map's entities come after the slots
    assert!(g.eval::<bool>("return ents.FindByClass('info_player_start')[1]:EntIndex() == 4"));
    let players: Vec<EntityHandle> = ["Alice", "Bob", "Carol"].iter().map(|n| g.runtime.join(named(n)).unwrap()).collect();
    assert_eq!(players.iter().map(|p| p.index.unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(g.runtime.join(named("Dave")).is_none(), "the server is full");
    assert_eq!(g.runtime.players(), players);
    assert!(g.eval::<bool>(
        "local all = player.GetAll()
         return #all == 3 and all[2]:Nick() == 'Bob' and Entity(1):IsListenServerHost() and not Entity(2):IsListenServerHost()
             and Entity(1):UniqueID() ~= 1"
    ));
    // one leaves: the next to come takes the slot
    g.runtime.leave(players[1]);
    g.runtime.think(0.02);
    assert_eq!(g.runtime.join(named("Dave")).and_then(|p| p.index), Some(2));
    // only the host runs Lua from the console
    g.runtime.run_player_command(players[2], "lua_run CAROL_RAN = true");
    g.runtime.run_player_command(players[0], "lua_run ALICE_RAN = true");
    assert!(g.eval::<bool>("return CAROL_RAN == nil and ALICE_RAN == true"));
}

#[test]
fn a_dedicated_server_has_no_host() {
    let g = server_for(2);
    g.runtime.set_dedicated(true);
    let alice = g.runtime.join(named("Alice")).unwrap();
    assert!(g.eval::<bool>("return game.IsDedicated() and not Entity(1):IsListenServerHost()"));
    g.runtime.run_player_command(alice, "lua_run RAN = true");
    assert!(g.eval::<bool>("return RAN == nil"));
}

#[test]
fn net_messages_go_to_whom_they_are_sent() {
    let g = server_for(4);
    for name in ["Alice", "Bob", "Carol"] {
        g.runtime.join(named(name)).unwrap();
    }
    g.runtime
        .run_string(
            "util.AddNetworkString('m')
             local function send(f, ...) net.Start('m') f(...) end
             send(net.Send, Entity(2))
             send(net.Send, { Entity(3), Entity(1), Entity(3), 'not a player' })
             local filter = RecipientFilter()
             filter:AddAllPlayers() filter:RemovePlayer(Entity(1))
             FILTER = { filter:GetCount(), filter:GetPlayers()[1] == Entity(2) }
             send(net.Send, filter)
             send(net.SendOmit, Entity(2))
             send(net.Broadcast)
             send(net.SendPVS, Vector(0, 0, 0))
             send(net.Send, game.GetWorld())",
            "send",
        )
        .unwrap();
    let to: Vec<Vec<u32>> = g.runtime.take_net_messages().into_iter().map(|m| m.to).collect();
    assert_eq!(to, [vec![2], vec![3, 1], vec![2, 3], vec![1, 3], vec![1, 2, 3], vec![1, 2, 3], vec![]]);
    assert!(g.eval::<bool>("return FILTER[1] == 2 and FILTER[2]"));
    // by team
    g.runtime
        .run_string(
            "Entity(2):SetTeam(5)
             local filter = RecipientFilter() filter:AddRecipientsByTeam(5)
             TEAM = filter:GetCount()",
            "team",
        )
        .unwrap();
    assert_eq!(g.eval::<i64>("return TEAM"), 1);
}

/// What a snapshot is after the network: encoded and decoded.
fn over_the_wire(snapshot: &Snapshot) -> Snapshot {
    let bytes = postcard::to_allocvec(snapshot).unwrap();
    postcard::from_bytes(&bytes).unwrap()
}

#[test]
fn snapshots_cross_the_network_with_ragdolls_posed() {
    let files = super::ragdoll::ragdoll_files();
    let files: Vec<(&str, &[u8])> = files.iter().map(|(path, bytes)| (*path, bytes.as_slice())).collect();
    let server = game_with(Realm::Server, &files);
    server.runtime.init();
    server.runtime.run_string(PLAYER_LUA, "players").unwrap();
    server.runtime.spawn_map(&player_map());
    // a doll held by its body, its arm hanging
    server
        .runtime
        .run_string(
            "R = ents.Create('prop_ragdoll') R:SetModel('models/doll.mdl') R:SetPos(Vector(0, 0, 100)) R:Spawn()
             R:GetPhysicsObjectNum(0):EnableMotion(false) R:GetPhysicsObjectNum(1):Wake()
             R:SetNWString('owner', 'Alice') R:SetNW2Int('parts', 2)",
            "doll",
        )
        .unwrap();
    server.runtime.think(3.0);
    let snapshot = server.runtime.snapshot();
    let doll = snapshot.entities.iter().find(|e| e.ent.class == "prop_ragdoll").unwrap();
    assert_eq!(doll.ent.ragdoll_pose.len(), 2, "the body's bone and the arm's");
    assert!(doll.ent.physics.is_none());

    let client = game_with(Realm::Client, &files);
    client.runtime.init();
    client.runtime.apply_snapshot(&over_the_wire(&snapshot));
    assert!(client.eval::<bool>(
        "local r = ents.FindByClass('prop_ragdoll')[1]
         return r:GetNWString('owner') == 'Alice' and r:GetNW2Int('parts') == 2 and r:GetModel() == 'models/doll.mdl'"
    ));
    // drawn on the client as on the server: the arm where the server's body has it
    let on_server = server.runtime.drawn(EntityHandle::NULL);
    let on_client = client.runtime.drawn(EntityHandle::NULL);
    let bones = |drawn: &[Drawn]| drawn.iter().find(|d| d.model == "models/doll.mdl").map(|d| d.bones.clone()).unwrap();
    let (a, b) = (bones(&on_server), bones(&on_client));
    assert_eq!(a.len(), 4);
    for (a, b) in a.iter().flatten().flatten().zip(b.iter().flatten().flatten()) {
        assert!((a - b).abs() < 0.01, "{a} {b}");
    }
}

#[test]
fn between_two_snapshots_things_move_smoothly() {
    let g = server_for(1);
    g.runtime
        .run_string(
            "P = ents.Create('prop_physics') P:SetModel('models/box.mdl') P:SetPos(Vector(0, 0, 0)) P:SetAngles(Angle(0, 170, 0)) P:Spawn()",
            "prop",
        )
        .unwrap();
    let a = g.runtime.snapshot();
    g.runtime.run_string("P:SetPos(Vector(100, 0, 0)) P:SetAngles(Angle(0, -170, 0))", "move").unwrap();
    let b = Snapshot { time: a.time + 0.1, ..g.runtime.snapshot() };
    let mid = Snapshot::between(&a, &b, 0.25);
    let prop = mid.entities.iter().find(|e| e.ent.class == "prop_physics").unwrap();
    assert!((prop.ent.local_pos.x - 25.0).abs() < 0.01, "{:?}", prop.ent.local_pos);
    // the short way round: from 170 through 180
    assert!((prop.ent.local_angles.y - 175.0).abs() < 0.01, "{:?}", prop.ent.local_angles);
    assert!((mid.time - (a.time + 0.025)).abs() < 1e-9);
    // a jump is not drawn out
    g.runtime.run_string("P:SetPos(Vector(5000, 0, 0))", "teleport").unwrap();
    let c = g.runtime.snapshot();
    let mid = Snapshot::between(&b, &c, 0.5);
    assert_eq!(mid.entities.iter().find(|e| e.ent.class == "prop_physics").unwrap().ent.local_pos.x, 5000.0);
}

#[test]
fn the_server_asks_things_of_clients() {
    let g = server_for(3);
    let alice = g.runtime.join(named("Alice")).unwrap();
    let bob = g.runtime.join(named("Bob")).unwrap();
    let bot = g.runtime.join(PlayerInfo { name: "Bot01".into(), bot: true, ..PlayerInfo::default() }).unwrap();
    g.runtime
        .run_string(
            "Entity(1):SendLua('X = 1') BroadcastLua('Y = 2') Entity(2):ConCommand('cl_playerskin 4')
             Entity(3):ConCommand('kill') Entity(3):SendLua('nothing')",
            "ask",
        )
        .unwrap();
    assert_eq!(
        g.runtime.take_to_clients(),
        [
            (alice, ToClient::Lua("X = 1".into())),
            (alice, ToClient::Lua("Y = 2".into())),
            (bob, ToClient::Lua("Y = 2".into())),
            (bob, ToClient::Command("cl_playerskin 4".into())),
        ]
    );
    // a bot has no client: the server runs its console itself
    assert!(g.eval::<bool>("return not Entity(3):Alive()"), "{bot:?}");

    // a client does as asked
    let client = game(Realm::Client, &[]);
    client.runtime.from_server(&ToClient::Lua("X = 41 + 1".into()));
    client.runtime.from_server(&ToClient::Print("hello".into()));
    client.runtime.from_server(&ToClient::Command("say hi".into()));
    assert_eq!(client.eval::<i64>("return X"), 42);
    assert_eq!(client.printed(), "hello\n");
    assert_eq!(client.runtime.take_server_commands(), ["say hi"]);
}
