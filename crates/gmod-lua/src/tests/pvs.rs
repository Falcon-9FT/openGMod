//! What each player is sent of the entities, and who sees and hears what: the map's PVS and PAS.

use super::*;

/// The width of a room, and how far one room is from the next.
const ROOM: f32 = 256.0;
const STEP: f32 = 272.0;

/// The middle of room `i`, on its floor.
fn room(i: usize) -> String {
    format!("Vector({}, 128, 0)", STEP * i as f32 + ROOM / 2.0)
}

/// Four rooms along x, 256 wide and deep and 192 high, walls 16 thick between them; each room is
/// a cluster and sees its neighbours: the first sees the second, hears the third and not the
/// fourth. The spawn point is in the first.
fn rooms_map() -> Map {
    use gmod_bsp::testmap::{TestBrush, TestMap};
    let length = STEP * 4.0;
    let mut world = vec![
        TestBrush::cuboid([-16.0, -16.0, -16.0], [length, 272.0, 0.0]),
        TestBrush::cuboid([-16.0, -16.0, 192.0], [length, 272.0, 208.0]),
        TestBrush::cuboid([-16.0, -16.0, 0.0], [length, 0.0, 192.0]),
        TestBrush::cuboid([-16.0, 256.0, 0.0], [length, 272.0, 192.0]),
        TestBrush::cuboid([-16.0, 0.0, 0.0], [0.0, 256.0, 192.0]),
    ];
    let mut clusters = Vec::new();
    for i in 0..4 {
        let lo = STEP * i as f32;
        clusters.push(([lo, 0.0, 0.0], [lo + ROOM, 256.0, 192.0]));
        world.push(TestBrush::cuboid([lo + ROOM, 0.0, 0.0], [lo + STEP, 256.0, 192.0]));
    }
    let test = TestMap { models: vec![world], clusters, sees: vec![(0, 1), (1, 2), (2, 3)], ..TestMap::default() };
    let bsp = gmod_bsp::Bsp::parse("rooms.bsp".into(), Arc::from(test.bytes())).unwrap();
    let mut map = Map::from_bsp("gm_rooms", &bsp, Arc::new(gmod_bsp::Collision::new(&bsp)));
    let pairs = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
    map.entities = vec![
        pairs(&[("classname", "worldspawn")]),
        pairs(&[("classname", "info_player_start"), ("origin", "128 128 0"), ("angles", "0 0 0")]),
    ];
    map
}

/// A server for two on [`rooms_map`] with Alice in the first room and Bob in the fourth, and a
/// box in each room: `BOX[1]` to `BOX[4]`.
fn two_rooms_apart() -> (Game, EntityHandle, EntityHandle) {
    let mdl = gmod_model::testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let g = game_with(
        Realm::Server,
        &[
            ("lua/includes/init.lua", ENTITY_LUA.as_bytes()),
            ("models/box.mdl", &mdl),
            ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes()),
        ],
    );
    g.runtime.set_max_players(2);
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime.spawn_map(&rooms_map());
    let alice = g.runtime.join(alice()).unwrap();
    let bob = g.runtime.join(PlayerInfo { name: "Bob".into(), ..PlayerInfo::default() }).unwrap();
    g.eval::<()>(&format!(
        "Entity(2):SetPos({})
         BOX = {{}}
         for i, at in ipairs({{ {}, {}, {}, {} }}) do
             local b = ents.Create('prop_physics') b:SetModel('models/box.mdl') b:SetPos(at + Vector(0, 40, 0)) b:Spawn()
             BOX[i] = b
         end",
        room(3),
        room(0),
        room(1),
        room(2),
        room(3)
    ));
    (g, alice, bob)
}

/// The indices of what `player` is sent now.
fn sent(g: &Game, player: EntityHandle) -> Vec<u32> {
    let transmission = g.runtime.transmission();
    let mut indices: Vec<u32> = g.runtime.sent_to(&transmission, player).indices.into_iter().collect();
    indices.sort();
    indices
}

fn index(g: &Game, lua: &str) -> u32 {
    g.eval::<u32>(&format!("return ({lua}):EntIndex()"))
}

#[test]
fn players_are_sent_what_they_may_see() {
    let (g, alice, bob) = two_rooms_apart();
    let boxes: Vec<u32> = (1..=4).map(|i| index(&g, &format!("BOX[{i}]"))).collect();
    let pistols = [index(&g, "Entity(1):GetActiveWeapon()"), index(&g, "Entity(2):GetActiveWeapon()")];
    let view_models = [index(&g, "Entity(1):GetViewModel()"), index(&g, "Entity(2):GetViewModel()")];
    // the world and the players always, the weapons in their hands with them; the boxes in
    // the rooms each sees; a view model to its owner alone
    let to_alice = sent(&g, alice);
    for i in [0, 1, 2, boxes[0], boxes[1], pistols[0], pistols[1], view_models[0]] {
        assert!(to_alice.contains(&i), "{i} not in {to_alice:?}");
    }
    for i in [boxes[2], boxes[3], view_models[1]] {
        assert!(!to_alice.contains(&i), "{i} in {to_alice:?}");
    }
    let to_bob = sent(&g, bob);
    assert!(to_bob.contains(&boxes[2]) && to_bob.contains(&boxes[3]) && !to_bob.contains(&boxes[0]) && !to_bob.contains(&boxes[1]));
    // what is held back is what the server has: Alice's client keeps those dormant
    let transmission = g.runtime.transmission();
    let held: Vec<u32> = g.runtime.sent_to(&transmission, alice).held.iter().map(|(i, _)| *i).collect();
    assert!(held.contains(&boxes[3]) && !held.contains(&boxes[0]));

    // a SENT says for itself; SetPreventTransmit keeps one from a player
    g.eval::<()>(&format!(
        "scripted_ents.Register({{ Type = 'anim', UpdateTransmitState = function() return TRANSMIT_ALWAYS end }}, 'always_sent')
         scripted_ents.Register({{ Type = 'anim', UpdateTransmitState = function(self) return self.How or TRANSMIT_NEVER end }}, 'never_sent')
         ALWAYS = ents.Create('always_sent') ALWAYS:SetModel('models/box.mdl') ALWAYS:SetPos({}) ALWAYS:Spawn()
         NEVER = ents.Create('never_sent') NEVER:SetModel('models/box.mdl') NEVER:SetPos({}) NEVER:Spawn()
         BOX[1]:SetPreventTransmit(Entity(1), true)",
        room(3),
        room(0)
    ));
    let (always, never) = (index(&g, "ALWAYS"), index(&g, "NEVER"));
    let to_alice = sent(&g, alice);
    assert!(to_alice.contains(&always) && !to_alice.contains(&never) && !to_alice.contains(&boxes[0]));
    assert!(sent(&g, bob).contains(&always));
    // asked again once flagged: now it goes where it is seen
    g.eval::<()>("NEVER.How = TRANSMIT_PVS NEVER:AddEFlags(EFL_FORCE_CHECK_TRANSMIT) BOX[1]:SetPreventTransmit(Entity(1), false)");
    let to_alice = sent(&g, alice);
    assert!(to_alice.contains(&never) && to_alice.contains(&boxes[0]));
    assert!(g.eval::<bool>("return not NEVER:IsEFlagSet(EFL_FORCE_CHECK_TRANSMIT) and not BOX[1]:IsDormant()"));

    // SetupPlayerVisibility adds what is seen from elsewhere, as a camera
    g.runtime
        .run_string(
            &format!(
                "HOOKS.SetupPlayerVisibility = function(ply, view)
                     ASKED = (ASKED or 0) + 1
                     if ply == Entity(1) then AddOriginToPVS({} + Vector(0, 0, 64)) end
                 end",
                room(3)
            ),
            "camera",
        )
        .unwrap();
    let to_alice = sent(&g, alice);
    assert!(to_alice.contains(&boxes[3]) && to_alice.contains(&boxes[2]) && !sent(&g, bob).contains(&boxes[0]));
    assert!(g.eval::<bool>("return ASKED == 2"));
    // so does looking through another entity
    g.eval::<()>("HOOKS.SetupPlayerVisibility = nil Entity(2):SetViewEntity(BOX[1])");
    assert!(sent(&g, bob).contains(&boxes[0]));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn without_visibility_everything_is_sent() {
    let g = player_game();
    let alice = g.runtime.join(alice()).unwrap();
    g.eval::<()>("B = ents.Create('prop_physics') B:SetModel('models/box.mdl') B:SetPos(Vector(900, 900, 0)) B:Spawn()");
    let b = index(&g, "B");
    assert!(sent(&g, alice).contains(&b));
    let transmission = g.runtime.transmission();
    assert!(g.runtime.sent_to(&transmission, alice).held.is_empty());
}

#[test]
fn filters_net_and_sounds_go_by_what_is_seen_and_heard() {
    let (g, _, _) = two_rooms_apart();
    g.runtime
        .run_string(
            &format!(
                "util.AddNetworkString('m')
                 local function count(f) local r = RecipientFilter() f(r) local p = r:GetPlayers() return #p == 1 and p[1]:EntIndex() or #p end
                 PVS0 = count(function(r) r:AddPVS({0} + Vector(0, 0, 64)) end)
                 PAS0 = count(function(r) r:AddPAS({0} + Vector(0, 0, 64)) end)
                 PAS3 = count(function(r) r:AddPAS({2} + Vector(0, 0, 64)) end)
                 LEFT = count(function(r) r:AddAllPlayers() r:RemovePVS({2} + Vector(0, 0, 64)) end)
                 net.Start('m') net.SendPVS({2} + Vector(0, 0, 64))
                 net.Start('m') net.SendPAS({1} + Vector(0, 0, 64))
                 sound.Play('ambient/hum.wav', {2} + Vector(0, 0, 64))
                 sound.Play('ambient/everywhere.wav', {2} + Vector(0, 0, 64), 0)
                 BOX[1]:EmitSound('ambient/box.wav')",
                room(0),
                room(1),
                room(3)
            ),
            "filters",
        )
        .unwrap();
    // the first room is seen from the first two and heard from the first three: by Alice
    // alone; the fourth is heard by Bob alone
    assert!(g.eval::<bool>("return PVS0 == 1 and PAS0 == 1 and PAS3 == 2 and LEFT == 1"));
    let to: Vec<Vec<u32>> = g.runtime.take_net_messages().into_iter().map(|m| m.to).collect();
    assert_eq!(to, [vec![2], vec![1, 2]]);
    let heard: Vec<Option<Vec<u32>>> = g
        .runtime
        .take_sounds()
        .iter()
        .map(|s| g.runtime.hearing(s).map(|players| players.iter().filter_map(|p| p.index).collect()))
        .collect();
    assert_eq!(heard, [Some(vec![2]), None, Some(vec![1])]);
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

/// What a snapshot is after the network: encoded and decoded.
fn over_the_wire(snapshot: &Snapshot) -> Snapshot {
    postcard::from_bytes(&postcard::to_allocvec(snapshot).unwrap()).unwrap()
}

/// The server's snapshot as `player` gets it: what it is sent, and the rest held back dormant.
fn for_player(g: &Game, player: EntityHandle) -> Snapshot {
    let transmission = g.runtime.transmission();
    let sent = g.runtime.sent_to(&transmission, player);
    let mut snapshot = g.runtime.snapshot();
    snapshot.entities.retain(|e| sent.indices.contains(&e.index));
    snapshot.dormant = sent.held;
    over_the_wire(&snapshot)
}

#[test]
fn clients_keep_what_is_out_of_sight_dormant() {
    let (server, alice, _) = two_rooms_apart();
    let mdl = gmod_model::testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let client = game_with(Realm::Client, &[("lua/includes/init.lua", ENTITY_LUA.as_bytes()), ("models/box.mdl", &mdl)]);
    client.runtime.init();
    client
        .runtime
        .run_string(
            "NOTIFIED = ' ' HOOKS.NotifyShouldTransmit = function(ent, sent) NOTIFIED = NOTIFIED .. ent:EntIndex() .. tostring(sent) .. ' ' end",
            "notify",
        )
        .unwrap();
    let far = index(&server, "BOX[4]");
    let near = index(&server, "BOX[1]");
    // first it has everything; then the far box goes out of sight: kept, dormant, not drawn
    client.runtime.apply_snapshot(&over_the_wire(&server.runtime.snapshot()));
    let drawn = |g: &Game| g.runtime.drawn(EntityHandle::NULL).len();
    let all_drawn = drawn(&client);
    client.runtime.apply_snapshot(&for_player(&server, alice));
    // (Bob's view models and hands go to Bob alone: they sleep too)
    assert!(client.eval::<bool>(&format!(
        "local far, near = Entity({far}), Entity({near})
         return far:IsValid() and far:IsDormant() and not near:IsDormant() and Entity(2):GetViewModel():IsDormant()
             and NOTIFIED:find(' {far}false ', 1, true) and not NOTIFIED:find('{near}', 1, true)"
    )));
    assert_eq!(drawn(&client), all_drawn - 2, "the boxes of the third and fourth rooms");
    // it comes back in sight
    client.runtime.run_string("NOTIFIED = ' '", "reset").unwrap();
    server.eval::<()>(&format!("BOX[4]:SetPos({} + Vector(0, 40, 0))", room(1)));
    client.runtime.apply_snapshot(&for_player(&server, alice));
    assert!(client.eval::<bool>(&format!("return not Entity({far}):IsDormant() and NOTIFIED == ' {far}true '")));
    // out of sight and removed on the server: gone from the client too
    server.eval::<()>(&format!("BOX[4]:SetPos({} + Vector(0, 40, 0)) BOX[3]:Remove()", room(3)));
    server.runtime.think(0.02);
    let near_gone = index(&server, "BOX[2]");
    client.runtime.apply_snapshot(&for_player(&server, alice));
    assert!(client.eval::<bool>(&format!("return Entity({far}):IsDormant() and Entity({near_gone}):IsValid()")));
    server.eval::<()>("BOX[4]:Remove()");
    server.runtime.think(0.02);
    client.runtime.apply_snapshot(&for_player(&server, alice));
    assert!(client.eval::<bool>(&format!("return not Entity({far}):IsValid()")));
    assert!(client.messages().is_empty(), "{:?}", client.messages());
}

/// [`rooms_map`] with each room an area of its own (1 to 4) and portals between them: key 1
/// between the first two (a `func_areaportal` shut at first), key 2 between the second and the
/// third (one that goes with the door `door`, a `func_door` in the third room), key 3 between
/// the last two (open).
fn portal_rooms_map() -> Map {
    use gmod_bsp::testmap::{TestBrush, TestMap};
    let length = STEP * 4.0;
    let mut world = vec![
        TestBrush::cuboid([-16.0, -16.0, -16.0], [length, 272.0, 0.0]),
        TestBrush::cuboid([-16.0, -16.0, 192.0], [length, 272.0, 208.0]),
        TestBrush::cuboid([-16.0, -16.0, 0.0], [length, 0.0, 192.0]),
        TestBrush::cuboid([-16.0, 256.0, 0.0], [length, 272.0, 192.0]),
        TestBrush::cuboid([-16.0, 0.0, 0.0], [0.0, 256.0, 192.0]),
    ];
    let (mut clusters, mut areas) = (Vec::new(), Vec::new());
    for i in 0..4 {
        let lo = STEP * i as f32;
        clusters.push(([lo, 0.0, 0.0], [lo + ROOM, 256.0, 192.0]));
        areas.push(([lo, 0.0, 0.0], [lo + ROOM, 256.0, 192.0], i as u16 + 1));
        world.push(TestBrush::cuboid([lo + ROOM, 0.0, 0.0], [lo + STEP, 256.0, 192.0]));
    }
    let door = vec![TestBrush::cuboid([STEP * 2.0 + 200.0, 200.0, 0.0], [STEP * 2.0 + 240.0, 240.0, 64.0])];
    let test = TestMap {
        models: vec![world, door],
        clusters,
        sees: vec![(0, 1), (1, 2), (2, 3)],
        areas,
        area_portals: vec![(1, 2, 1), (2, 3, 2), (3, 4, 3)],
        ..TestMap::default()
    };
    let bsp = gmod_bsp::Bsp::parse("portals.bsp".into(), Arc::from(test.bytes())).unwrap();
    let mut map = Map::from_bsp("gm_portals", &bsp, Arc::new(gmod_bsp::Collision::new(&bsp)));
    let pairs = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
    map.entities = vec![
        pairs(&[("classname", "worldspawn")]),
        pairs(&[("classname", "info_player_start"), ("origin", "128 128 0"), ("angles", "0 0 0")]),
        pairs(&[("classname", "func_areaportal"), ("targetname", "first"), ("portalnumber", "1"), ("StartOpen", "0")]),
        pairs(&[("classname", "func_areaportal"), ("portalnumber", "2"), ("target", "door")]),
        pairs(&[("classname", "func_areaportal"), ("portalnumber", "3")]),
        pairs(&[("classname", "func_door"), ("targetname", "door"), ("model", "*1"), ("speed", "100"), ("movedir", "0 90 0")]),
    ];
    map
}

#[test]
fn shut_portals_keep_what_is_behind_them() {
    let mdl = gmod_model::testmodel::mdl(1, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    let g = game_with(
        Realm::Server,
        &[
            ("lua/includes/init.lua", ENTITY_LUA.as_bytes()),
            ("models/box.mdl", &mdl),
            ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes()),
        ],
    );
    g.runtime.set_max_players(2);
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime.spawn_map(&portal_rooms_map());
    let alice = g.runtime.join(alice()).unwrap();
    let bob = g.runtime.join(PlayerInfo { name: "Bob".into(), ..PlayerInfo::default() }).unwrap();
    g.eval::<()>(&format!(
        "Entity(2):SetPos({})
         BOX = {{}}
         for i, at in ipairs({{ {}, {}, {}, {} }}) do
             local b = ents.Create('prop_physics') b:SetModel('models/box.mdl') b:SetPos(at + Vector(0, 40, 0)) b:Spawn()
             BOX[i] = b
         end",
        room(2),
        room(0),
        room(1),
        room(2),
        room(3)
    ));
    let boxes: Vec<u32> = (1..=4).map(|i| index(&g, &format!("BOX[{i}]"))).collect();
    // the first portal shut: Alice does not get the second room's box, though she may see it
    assert!(!sent(&g, alice).contains(&boxes[1]));
    // the door shut: Bob does not get the second room's box either; the last portal is open
    let to_bob = sent(&g, bob);
    assert!(!to_bob.contains(&boxes[1]) && to_bob.contains(&boxes[3]) && to_bob.contains(&boxes[2]));
    // opened, shut, toggled
    g.eval::<()>("ents.FindByName('first')[1]:Fire('Open')");
    g.runtime.think(0.05);
    assert!(sent(&g, alice).contains(&boxes[1]));
    g.eval::<()>("ents.FindByName('first')[1]:Fire('Close')");
    g.runtime.think(0.05);
    assert!(!sent(&g, alice).contains(&boxes[1]));
    g.eval::<()>("ents.FindByName('first')[1]:Fire('Toggle')");
    g.runtime.think(0.05);
    assert!(sent(&g, alice).contains(&boxes[1]));
    // the door opening opens its portal
    g.eval::<()>("ents.FindByName('door')[1]:Fire('Open')");
    g.runtime.think(0.2);
    assert!(sent(&g, bob).contains(&boxes[1]));
    // and the portals themselves go to no client
    let portals: Vec<u32> =
        g.eval::<Vec<u32>>("local t = {} for _, p in ipairs(ents.FindByClass('func_areaportal')) do t[#t + 1] = p:EntIndex() end return t");
    assert_eq!(portals.len(), 3);
    assert!(portals.iter().all(|p| !sent(&g, alice).contains(p)));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
