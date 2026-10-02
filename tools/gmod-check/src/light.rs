//! `light`: what light a map has for the world and for the models of entities, and what a
//! model gets at a point - to compare with the game when models or the world look too dark.

use gmod_bsp::{Bsp, Collision, LightKind, rgbe_to_linear};
use gmod_world::light::MapLight;

fn luminance(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// The middle and the brightest tenth of the luxels of a lighting lump.
fn lightmap_stats(lighting: &[u8]) -> String {
    let mut values: Vec<f32> = lighting.as_chunks::<4>().0.iter().map(|p| luminance(rgbe_to_linear(*p))).collect();
    if values.is_empty() {
        return "none".to_string();
    }
    values.sort_by(f32::total_cmp);
    let at = |q: f64| values[((values.len() - 1) as f64 * q) as usize];
    format!("{} luxels, median {:.3}, 90% {:.3}, max {:.3}", values.len(), at(0.5), at(0.9), at(1.0))
}

fn cube_line(cube: &gmod_bsp::LightCube) -> String {
    let side = |c: [f32; 3]| format!("{:.3} {:.3} {:.3}", c[0], c[1], c[2]);
    format!(
        "+x {} | -x {} | +y {} | -y {} | +z {} | -z {}",
        side(cube[0]),
        side(cube[1]),
        side(cube[2]),
        side(cube[3]),
        side(cube[4]),
        side(cube[5])
    )
}

/// The light of a map, and what a model gets at each of `points`.
pub fn report(bsp: &Bsp, collision: &Collision, points: &[[f32; 3]]) -> Vec<String> {
    let mut out = vec![format!("{}: version {}", bsp.label.display(), bsp.version)];
    out.push(format!("  light maps, LDR: {}", lightmap_stats(&bsp.lighting)));
    out.push(format!("  light maps, HDR: {}", lightmap_stats(&bsp.lighting_hdr)));
    match bsp.ambient_samples() {
        Ok(samples) => {
            let with = samples.iter().filter(|s| !s.is_empty()).count();
            let mut tops: Vec<f32> = samples.iter().flatten().map(|s| luminance(s.cube[4])).collect();
            tops.sort_by(f32::total_cmp);
            let median = tops.get(tops.len() / 2).copied().unwrap_or(0.0);
            out.push(format!(
                "  ambient samples: {} in {with} of {} leaves; light from above: median {median:.3}, max {:.3}",
                tops.len(),
                samples.len(),
                tops.last().copied().unwrap_or(0.0)
            ));
        }
        Err(e) => out.push(format!("  ambient samples: {e}")),
    }
    let lights = bsp.world_lights();
    let count = |k: fn(&LightKind) -> bool| lights.iter().filter(|l| k(&l.kind)).count();
    out.push(format!(
        "  lights: {} - {} points, {} spots, {} sun, {} sky ambient, {} surfaces",
        lights.len(),
        count(|k| matches!(k, LightKind::Point | LightKind::Quake)),
        count(|k| matches!(k, LightKind::Spot)),
        count(|k| matches!(k, LightKind::Sky)),
        count(|k| matches!(k, LightKind::SkyAmbient)),
        count(|k| matches!(k, LightKind::Surface)),
    ));
    for sun in lights.iter().filter(|l| matches!(l.kind, LightKind::Sky | LightKind::SkyAmbient)) {
        out.push(format!(
            "    {:?}: light {:.3} {:.3} {:.3}, along {:.2} {:.2} {:.2}",
            sun.kind, sun.intensity[0], sun.intensity[1], sun.intensity[2], sun.normal[0], sun.normal[1], sun.normal[2]
        ));
    }
    let map_light = MapLight::new(bsp);
    for &p in points {
        let leaf = collision.leaf_at(p);
        let samples = leaf.and_then(|l| bsp.ambient_samples().ok().map(|s| s.get(l).map_or(0, Vec::len)));
        let light = map_light.at(collision, p);
        out.push(format!("  at {} {} {}: leaf {leaf:?} with {} samples", p[0], p[1], p[2], samples.unwrap_or(0)));
        out.push(format!("    ambient {}", cube_line(&light.ambient)));
        for (towards, color) in &light.lights {
            out.push(format!(
                "    light from {:.0} {:.0} {:.0}: {:.3} {:.3} {:.3}",
                towards[0], towards[1], towards[2], color[0], color[1], color[2]
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use gmod_bsp::testmap::{TestBrush, TestLight, TestMap};

    use super::*;

    #[test]
    fn tells_the_light_of_a_map_and_at_a_point() {
        let floor = TestBrush::cuboid([-512.0, -512.0, -16.0], [512.0, 512.0, 0.0]);
        let sun = TestLight { kind: 3, origin: [0.0; 3], intensity: [2.0, 1.8, 1.5], normal: [0.0, 0.0, -1.0] };
        let map = TestMap { models: vec![vec![floor]], ambient: Some([[128, 128, 128, 0]; 6]), lights: vec![sun], ..TestMap::default() };
        let bsp = Bsp::parse("test.bsp".into(), std::sync::Arc::from(map.bytes())).unwrap();
        let collision = Collision::new(&bsp);
        let lines = report(&bsp, &collision, &[[0.0, 0.0, 32.0]]).join("\n");
        assert!(lines.contains("1 sun"), "{lines}");
        assert!(lines.contains("ambient +x 0.502"), "{lines}");
    }
}
