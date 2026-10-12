//! Props by their shapes: traces, players' feet.

use super::*;
use gmod_model::testmodel::{self, TestConvex};

/// A wedge 64 long, 32 wide and 64 high at x = 0, sloping down to nothing at x = 64.
fn wedge() -> TestConvex {
    TestConvex {
        points: vec![[0.0, 0.0, 0.0], [64.0, 0.0, 0.0], [64.0, 32.0, 0.0], [0.0, 32.0, 0.0], [0.0, 0.0, 64.0], [0.0, 32.0, 64.0]],
        triangles: vec![[0, 3, 2], [0, 2, 1], [0, 4, 5], [0, 5, 3], [1, 2, 5], [1, 5, 4], [0, 1, 4], [3, 5, 2]],
    }
}

/// [`player_game`]'s server with a frozen wedge at x -300 to -236, y -16 to 16.
fn wedge_game() -> Game {
    let mdl = testmodel::mdl(9, ([0.0, 0.0, 0.0], [64.0, 32.0, 64.0]));
    let phy = testmodel::phy(9, &[vec![wedge()]], false, "solid { \"index\" \"0\" \"mass\" \"40\" }");
    let g = game_with(
        Realm::Server,
        &[
            ("lua/includes/init.lua", ENTITY_LUA.as_bytes()),
            ("models/wedge.mdl", &mdl),
            ("models/wedge.phy", &phy),
            ("scripts/weapons/weapon_pistol.txt", PISTOL.as_bytes()),
        ],
    );
    g.runtime.init();
    g.runtime.run_string(PLAYER_LUA, "players").unwrap();
    g.runtime.spawn_map(&player_map());
    g.runtime
        .run_string(
            "W = ents.Create('prop_physics') W:SetModel('models/wedge.mdl') W:SetPos(Vector(-300, -16, 0)) W:Spawn()
             W:GetPhysicsObject():EnableMotion(false)",
            "wedge",
        )
        .unwrap();
    g
}

#[test]
fn traces_go_by_the_shapes_of_props() {
    let g = wedge_game();
    // above the slope but inside the box around the wedge: a ray goes by
    assert!(g.eval::<bool>("return util.TraceLine({ start = Vector(-252, -100, 40), endpos = Vector(-252, 100, 40) }).Entity ~= W"));
    // down onto the slope: 16 high there, facing up and along +x
    assert!(g.eval::<bool>(
        "local t = util.TraceLine({ start = Vector(-252, 0, 100), endpos = Vector(-252, 0, -10) })
         return t.Entity == W and math.abs(t.HitPos.z - 16) < 0.5 and math.abs(t.HitNormal.x - 0.7071) < 0.01 and math.abs(t.HitNormal.z - 0.7071) < 0.01"
    ));
    // a hull is held by the slope where its corner meets it (20 high at x -256), not by the box
    assert!(g.eval::<bool>(
        "local t = util.TraceHull({ start = Vector(-252, 0, 100), endpos = Vector(-252, 0, -10), mins = Vector(-4, -4, 0), maxs = Vector(4, 4, 8) })
         return t.Entity == W and math.abs(t.HitPos.z - 20) < 0.5"
    ));
    // turned over on its back: what was the empty corner is the wedge now
    g.runtime.run_string("W:SetAngles(Angle(0, 180, 0)) W:SetPos(Vector(-236, 16, 0))", "turn").unwrap();
    assert!(g.eval::<bool>("return util.TraceLine({ start = Vector(-252, -100, 40), endpos = Vector(-252, 100, 40) }).Entity == W"));
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}

#[test]
fn players_stand_on_the_shapes_of_props() {
    let g = wedge_game();
    let alice = g.runtime.join(alice()).unwrap();
    // over the low end of the slope, where the box around the wedge is 64 high
    g.runtime.run_string("Entity(1):SetPos(Vector(-252, 0, 70))", "go").unwrap();
    g.runtime.think(1.0);
    let at = g.runtime.position(alice).unwrap();
    // the slope under the back edge of the player's box (16 behind the feet)
    let slope = 64.0 * (1.0 - (at.x - 16.0 + 300.0) / 64.0);
    assert!(at.z < 60.0 && (at.z - slope).abs() < 2.0, "on the slope, not on the box: {at:?}");
    assert!(g.messages().is_empty(), "{:?}", g.messages());
}
