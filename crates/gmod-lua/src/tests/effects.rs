//! Effects, particles, the engine's events, the faces of brushes.

use super::*;
use crate::player::ToClient;
use crate::render3d::Shape;
use crate::vector::Vector;

/// GMod's `effects` module, as `includes/modules/effects.lua` has it.
const EFFECTS_LUA: &str = "
    effects = { list = {} }
    function effects.Register(t, name) effects.list[name:lower()] = t end
    function effects.Create(name, t)
        local stored = effects.list[name:lower()]
        if not stored then return nil end
        t = t or {}
        for k, v in pairs(stored) do t[k] = v end
        return t
    end";

#[test]
fn particles_move_grow_old_and_go() {
    let g = client_game(&[]);
    g.runtime
        .run_string(
            "E = ParticleEmitter(Vector(0, 0, 0))
             P = E:Add('effects/spark', Vector(0, 0, 0))
             P:SetDieTime(1) P:SetVelocity(Vector(100, 0, 0)) P:SetGravity(Vector(0, 0, -100))
             P:SetStartSize(4) P:SetEndSize(0) P:SetStartAlpha(255) P:SetEndAlpha(0) P:SetColor(255, 0, 0)
             P:SetThinkFunction(function(p) THOUGHT = p:GetLifeTime() end) P:SetNextThink(CurTime())",
            "particles",
        )
        .unwrap();
    assert!(g.eval::<bool>("return E:IsValid() and E:GetNumActiveParticles() == 1 and E:Is3D() == false"));
    g.runtime.think(0.5);
    // gravity, then the way it goes: half a second of both
    assert!(g.eval::<bool>("return close(P:GetPos(), Vector(50, 0, -25)) and close(P:GetVelocity(), Vector(100, 0, -50))"));
    assert!(g.eval::<bool>("return THOUGHT == 0.5"), "its think function ran");
    assert_eq!(g.eval::<(f64, f64, f64)>("return P:GetColor()"), (255.0, 0.0, 0.0));
    g.runtime.drawn(EntityHandle::NULL);
    let drawn = g.runtime.take_primitives();
    let spark = drawn.iter().find(|p| p.material.name == "effects/spark").expect("it is drawn");
    match &spark.shape {
        // half way through its life: half its size and alpha, red
        Shape::Quad { corners, color } => {
            assert_eq!(*color, [1.0, 0.0, 0.0, 127.5 / 255.0]);
            assert!(((corners[0] - corners[2]).length() - 2.0 * 2.0f32.sqrt() * 2.0).abs() < 0.01, "{corners:?}");
        }
        other => panic!("{other:?}"),
    }
    // older than it lives: gone; an emitter Lua let go of goes with its last particle
    g.runtime.run_string("E:Finish()", "finish").unwrap();
    assert!(g.eval::<bool>("return not E:IsValid()"));
    g.runtime.think(0.6);
    assert!(g.eval::<bool>("return E:GetNumActiveParticles() == 0"));
    g.runtime.drawn(EntityHandle::NULL);
    assert!(g.runtime.take_primitives().iter().all(|p| p.material.name != "effects/spark"));
}

#[test]
fn meshes_are_made_once_and_drawn_again() {
    let g = client_game(&[]);
    g.runtime
        .run_string(
            "M = Mesh()
             mesh.Begin(M, MATERIAL_TRIANGLES, 1)
             mesh.Position(Vector(0, 0, 0)) mesh.AdvanceVertex()
             mesh.Position(Vector(10, 0, 0)) mesh.AdvanceVertex()
             mesh.Position(Vector(0, 10, 0)) mesh.AdvanceVertex()
             mesh.End()
             N = Mesh()
             N:BuildFromTriangles({ { pos = Vector(0, 0, 0) }, { pos = Vector(1, 0, 0) }, { pos = Vector(0, 1, 0), color = { r = 255, g = 0, b = 0, a = 255 } } })
             HOOKS.PostDrawOpaqueRenderables = function() M:Draw() M:Draw() N:Draw() end",
            "meshes",
        )
        .unwrap();
    g.runtime.drawn(EntityHandle::NULL);
    let triangles: Vec<_> = g.runtime.take_primitives().into_iter().filter(|p| matches!(p.shape, Shape::Triangles(_))).collect();
    assert_eq!(triangles.len(), 3, "made once, drawn as often as asked: {triangles:?}");
    assert!(g.eval::<bool>("local valid = M:IsValid() M:Destroy() return valid and not M:IsValid()"));
}

#[test]
fn effects_of_lua_think_and_draw_until_done() {
    let g = client_game(&[]);
    g.runtime.run_string(EFFECTS_LUA, "effects").unwrap();
    g.runtime
        .run_string(
            "effects.Register({
                 Init = function(self, data) INIT = data:GetOrigin() self.Left = 2 end,
                 Think = function(self) self.Left = self.Left - 1 return self.Left > 0 end,
                 Render = function(self) RENDERED = (RENDERED or 0) + 1 end,
             }, 'test_fx')
             local d = EffectData() d:SetOrigin(Vector(1, 2, 3))
             util.Effect('Test_FX', d)
             FX = ents.FindByClass('test_fx')[1]",
            "effect",
        )
        .unwrap();
    assert!(g.eval::<bool>("return INIT == Vector(1, 2, 3) and FX:IsValid() and FX:GetPos() == Vector(1, 2, 3)"));
    g.runtime.drawn(EntityHandle::NULL);
    assert!(g.eval::<bool>("return RENDERED == 1"));
    g.runtime.think(0.01);
    assert!(g.eval::<bool>("return FX:IsValid()"), "it said it goes on");
    g.runtime.think(0.01);
    assert!(g.eval::<bool>("return not FX:IsValid()"), "done");

    // a name no Lua has: the engine's own, made of particles
    g.runtime
        .run_string("local d = EffectData() d:SetOrigin(Vector(0, 0, 0)) d:SetNormal(Vector(0, 0, 1)) util.Effect('Sparks', d)", "sparks")
        .unwrap();
    g.runtime.drawn(EntityHandle::NULL);
    assert!(g.runtime.take_primitives().iter().any(|p| p.material.name == "effects/spark"));
}

#[test]
fn the_servers_effects_go_to_its_players() {
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
    g.runtime.spawn_map(&player_map());
    let alice = g.runtime.join(PlayerInfo { name: "Alice".into(), ..PlayerInfo::default() }).unwrap();
    g.runtime.take_to_clients();
    g.runtime.run_string("local d = EffectData() d:SetOrigin(Vector(1, 2, 3)) util.Effect('Explosion', d)", "effect").unwrap();
    let sent = g.runtime.take_to_clients();
    assert!(
        matches!(sent.as_slice(), [(to, ToClient::Effect(name, data))] if *to == alice && name == "Explosion" && data.origin == Vector::new(1.0, 2.0, 3.0)),
        "{sent:?}"
    );
}

#[test]
fn the_engines_events_reach_who_listens() {
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
    g.runtime.spawn_map(&player_map());
    g.runtime
        .run_string(
            "gameevent.Listen('player_connect') gameevent.Listen('player_spawn')
             HOOKS.player_connect = function(data) CONNECTED = data.name .. ' ' .. data.userid .. ' ' .. data.index .. ' ' .. data.bot end
             HOOKS.player_spawn = function(data) SPAWNED = data.userid end",
            "listen",
        )
        .unwrap();
    g.runtime.join(PlayerInfo { name: "Alice".into(), ..PlayerInfo::default() }).unwrap();
    assert_eq!(g.eval::<String>("return CONNECTED"), "Alice 1 0 0");
    assert_eq!(g.eval::<i64>("return SPAWNED"), 1);
    // the clients hear it as theirs, without where the player connects from
    let sent = g.runtime.take_to_clients();
    let connect = sent.iter().find_map(|(_, what)| match what {
        ToClient::GameEvent(name, fields) if name == "player_connect_client" => Some(fields.clone()),
        _ => None,
    });
    let connect = connect.expect("sent");
    assert!(connect.iter().all(|(k, _)| k != "address"), "{connect:?}");

    let client = client_game(&[]);
    client
        .runtime
        .run_string("gameevent.Listen('player_connect_client') HOOKS.player_connect_client = function(d) HEARD = d.name end", "l")
        .unwrap();
    client.runtime.from_server(&ToClient::GameEvent("player_connect_client".into(), connect));
    assert_eq!(client.eval::<String>("return HEARD"), "Alice");
}

#[test]
fn the_world_has_its_faces() {
    let g = entity_game(Realm::Server);
    let bsp = gmod_bsp::Bsp::parse("test.bsp".into(), Arc::from(gmod_bsp::testmap::bytes())).unwrap();
    g.runtime.spawn_map(&Map::from_bsp("gm_test", &bsp, Arc::new(gmod_bsp::Collision::new(&bsp))));
    // a floor, a wall, a displacement, a face of sky and one not drawn
    let (count, sky, nodraw, corners): (i64, i64, i64, i64) = g.eval(
        "local faces, sky, nodraw = Entity(0):GetBrushSurfaces(), 0, 0
         for _, f in ipairs(faces) do
             if f:IsSky() then sky = sky + 1 end
             if f:IsNoDraw() then nodraw = nodraw + 1 end
         end
         return #faces, sky, nodraw, #faces[1]:GetVertices()",
    );
    assert_eq!((count, sky, nodraw, corners), (5, 1, 1, 4));
    assert!(g.eval::<bool>("return ents.Create('prop_physics'):GetBrushSurfaces() == nil"), "no brushes, no faces");
}
