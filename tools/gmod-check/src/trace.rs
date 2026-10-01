//! `trace`: a sweep through a map, as `util.TraceLine` and `util.TraceHull` make it in the game,
//! printed under the names of GMod's `TraceResult`, so that the same sweep here and in the game
//! can be compared line by line. And the check that a player put at a map's start stands there.

use gmod_bsp::{Bsp, Collision, contents, surf};
use gmod_movement::{Command, Player, Settings};

/// The contents worth naming.
const NAMES: [(u32, &str); 14] = [
    (contents::SOLID, "SOLID"),
    (contents::WINDOW, "WINDOW"),
    (contents::GRATE, "GRATE"),
    (contents::SLIME, "SLIME"),
    (contents::WATER, "WATER"),
    (contents::OPAQUE, "OPAQUE"),
    (contents::MOVEABLE, "MOVEABLE"),
    (contents::PLAYERCLIP, "PLAYERCLIP"),
    (contents::MONSTERCLIP, "MONSTERCLIP"),
    (contents::MONSTER, "MONSTER"),
    (contents::DEBRIS, "DEBRIS"),
    (contents::DETAIL, "DETAIL"),
    (contents::TRANSLUCENT, "TRANSLUCENT"),
    (contents::LADDER, "LADDER"),
];

fn vector(v: [f32; 3]) -> String {
    format!("{} {} {}", v[0], v[1], v[2])
}

/// The sweep of the box `mins`..`maxs` - a ray when it has no size - from `from` to `to`
/// through what `mask` names, as lines.
pub fn report(bsp: &Bsp, from: [f32; 3], to: [f32; 3], mins: [f32; 3], maxs: [f32; 3], mask: u32) -> Vec<String> {
    let collision = Collision::new(bsp);
    let trace = collision.trace(from, to, mins, maxs, mask);
    let swept =
        if mins == [0.0; 3] && maxs == [0.0; 3] { "a ray".to_string() } else { format!("the box {} to {}", vector(mins), vector(maxs)) };
    let material = trace.texinfo.and_then(|t| bsp.texinfo.get(t)).and_then(|info| bsp.materials.get(usize::try_from(info.texdata).ok()?));
    let texture = match (trace.displacement, material) {
        (true, Some(material)) => format!("**displacement** ({material})"),
        (_, Some(material)) => material.clone(),
        (_, None) => String::new(),
    };
    let named: Vec<&str> = NAMES.iter().filter(|(flag, _)| trace.contents & flag != 0).map(|(_, name)| *name).collect();
    let contents = if named.is_empty() { trace.contents.to_string() } else { format!("{} ({})", trace.contents, named.join(" | ")) };
    let entity = match trace.model {
        None => String::new(),
        Some(0) => "worldspawn".into(),
        Some(model) => {
            let placed = collision.placements.iter().find(|p| p.model == model).and_then(|p| p.entity);
            let class = placed.and_then(|i| bsp.entities.get(i)).map_or("", |e| e.class_name());
            format!("{class} (model *{model})")
        }
    };
    vec![
        format!("{}: {swept} from {} to {}, mask {mask}", bsp.label.display(), vector(from), vector(to)),
        format!("  Hit                {}", trace.hit()),
        format!("  Fraction           {}", trace.fraction),
        format!("  HitPos             {}", vector(trace.end)),
        format!("  HitNormal          {}", vector(trace.normal)),
        format!("  StartSolid         {}", trace.start_solid),
        format!("  AllSolid           {}", trace.all_solid),
        format!("  FractionLeftSolid  {}", trace.fraction_left_solid),
        format!("  Contents           {contents}"),
        format!("  HitTexture         {texture}"),
        format!("  SurfaceFlags       {}", trace.surface_flags),
        format!("  HitSky             {}", trace.surface_flags & surf::SKY != 0),
        format!("  HitNoDraw          {}", trace.surface_flags & surf::NODRAW != 0),
        format!("  HitWorld           {}", trace.model == Some(0)),
        format!("  Entity             {entity}"),
    ]
}

/// Puts a player at the map's start and lets two seconds pass: they should stand there, on
/// the map's brushes or displacements, or swim. `None` for a map without a player start or
/// without brushes.
pub fn stand_at_start(bsp: &Bsp) -> Option<Result<(), String>> {
    let (origin, _) = gmod_world::spawn_point(bsp)?;
    let collision = Collision::new(bsp);
    if collision.is_empty() {
        return None;
    }
    let settings = Settings::default();
    let Some(mut player) = Player::spawn(&collision, &settings, origin) else {
        return Some(Err(format!("no room for a player at the player start ({})", vector(origin))));
    };
    for _ in 0..133 {
        player.tick(&collision, &settings, &Command::default(), 0.015);
    }
    Some(if player.on_ground || player.water_level >= 2 {
        Ok(())
    } else {
        Err(format!("a player put at the start ({}) still falls 2 s later, at {}", vector(origin), vector(player.origin)))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gmod_bsp::mask;
    use gmod_bsp::testmap::{TestBrush, TestMap};

    use super::*;

    fn map(start: Option<[f32; 3]>) -> Bsp {
        let world = vec![
            TestBrush::cuboid([-512.0, -512.0, -32.0], [512.0, 512.0, 0.0]),
            TestBrush::cuboid([100.0, -512.0, 0.0], [132.0, 512.0, 200.0]).material("BRICK/BRICKWALL001A", 0),
        ];
        let door = vec![TestBrush::cuboid([-16.0, -16.0, 0.0], [16.0, 16.0, 64.0])];
        let mut entities = "{\n\"classname\" \"func_door\"\n\"model\" \"*1\"\n\"origin\" \"-200 0 0\"\n}\n".to_string();
        if let Some([x, y, z]) = start {
            entities += &format!("{{\n\"classname\" \"info_player_start\"\n\"origin\" \"{x} {y} {z}\"\n}}\n");
        }
        let bytes = TestMap { models: vec![world, door], entities, ..TestMap::default() }.bytes();
        Bsp::parse("maps/test.bsp".into(), Arc::from(bytes)).unwrap()
    }

    #[test]
    fn traces_read_as_in_the_game() {
        let bsp = map(None);
        let lines = report(&bsp, [0.0, 0.0, 64.0], [400.0, 0.0, 64.0], [0.0; 3], [0.0; 3], mask::SOLID);
        assert_eq!(lines[0], "maps/test.bsp: a ray from 0 0 64 to 400 0 64, mask 33570827");
        assert!(lines.contains(&"  HitPos             99.96875 0 64".to_string()), "{lines:#?}");
        assert!(lines.contains(&"  HitNormal          -1 0 0".to_string()));
        assert!(lines.contains(&"  Contents           1 (SOLID)".to_string()));
        assert!(lines.contains(&"  HitTexture         BRICK/BRICKWALL001A".to_string()));
        assert!(lines.contains(&"  Entity             worldspawn".to_string()));
        let door = report(&bsp, [-200.0, 0.0, 100.0], [-200.0, 0.0, -100.0], [-16.0, -16.0, 0.0], [16.0, 16.0, 72.0], mask::PLAYERSOLID);
        assert!(door[0].contains("the box -16 -16 0 to 16 16 72"));
        assert!(door.contains(&"  Entity             func_door (model *1)".to_string()), "{door:#?}");
        assert!(door.contains(&"  HitWorld           false".to_string()));
    }

    #[test]
    fn players_stand_at_the_start() {
        assert_eq!(stand_at_start(&map(Some([0.0, 0.0, 0.0]))), Some(Ok(())));
        assert_eq!(stand_at_start(&map(Some([0.0, 0.0, 40.0]))), Some(Ok(())));
        let walled = stand_at_start(&map(Some([116.0, 0.0, 0.0])));
        assert!(walled.as_ref().is_some_and(|r| r.as_ref().is_err_and(|e| e.contains("no room"))), "{walled:?}");
        // off the edge of the floor: falling
        let falling = stand_at_start(&map(Some([2000.0, 0.0, 0.0])));
        assert!(falling.as_ref().is_some_and(|r| r.as_ref().is_err_and(|e| e.contains("still falls"))), "{falling:?}");
        assert_eq!(stand_at_start(&map(None)), None);
    }
}
