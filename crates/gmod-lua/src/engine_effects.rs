//! The effects the engine makes itself when `util.Effect` names none of Lua's: sparks, an
//! explosion's fire and smoke, a gun's flash, a bullet's dust, blood and splashes, tracers -
//! made of particles ([`crate::particles`]) after the look of Source's own.

use mlua::Lua;

use crate::effect::EffectData;
use crate::entity::EntityHandle;
use crate::host;
use crate::particles::{self, ParticleState, rand, rand_dir};
use crate::render3d::Material3d;
use crate::vector::Vector;

fn material(name: &str) -> Material3d {
    Material3d { name: name.to_string(), texture: None, additive: None }
}

/// The light it adds: a glow, a flash, fire.
fn additive(name: &str) -> Material3d {
    Material3d { name: name.to_string(), texture: None, additive: Some(true) }
}

/// A particle of a material, at a place, going some way, for some seconds.
fn particle(material: Material3d, pos: Vector, velocity: Vector, die_time: f32) -> ParticleState {
    let mut p = ParticleState::new(material, pos);
    p.velocity = velocity;
    p.die_time = die_time;
    p
}

/// Makes the engine's effect of that name, when it has one.
pub(crate) fn make(lua: &Lua, name: &str, data: EffectData) {
    match name.to_ascii_lowercase().as_str() {
        "sparks" | "manhacksparks" | "metalspark" | "electricspark" | "stunstickimpact" | "cball_bounce" => sparks(lua, &data, false),
        "ar2impact" | "bluespark" => sparks(lua, &data, true),
        "explosion" | "helicoptermegabomb" | "explosionsmall" | "ar2explosion" | "rpgshotdown" => explosion(lua, &data),
        "cball_explode" => flash(lua, data.origin, "effects/combinemuzzle2", 64.0, [200.0, 220.0, 255.0]),
        "muzzleeffect"
        | "muzzleflash"
        | "cs_muzzleflash"
        | "cs_muzzleflash_x"
        | "chopper_muzzleflash"
        | "gunshipmuzzleflash"
        | "stridermuzzleflash"
        | "airboatmuzzleflash"
        | "chopper_muzzle_flash" => muzzle_flash(lua, &data, false),
        "ar2muzzleflash" | "combine_muzzleflash" => muzzle_flash(lua, &data, true),
        "impact" | "impactgauss" | "impactjeep" | "boltimpact" | "helicopterimpact" => impact(lua, &data),
        "bloodimpact" | "bloodspray" | "striderblood" | "antliongib" | "hunterdamage" => {
            blood(lua, &data, name.eq_ignore_ascii_case("antliongib"))
        }
        "glassimpact" => debris(lua, &data, "effects/fleck_glass1", 10),
        "watersplash" | "gunshotsplash" | "watersurfaceexplosion" | "waterripple" => splash(lua, &data),
        "wheeldust" | "thumperdust" | "dirt" | "dust" => dust(lua, &data),
        "tracer" | "ar2tracer" | "airboatguntracer" | "helicoptertracer" | "gunshiptracer" | "stridertracer" | "gausstracer"
        | "tracersound" => tracer(lua, &data, name),
        _ => {}
    }
}

/// Sparks from a point, out along its normal, falling as they cool.
fn sparks(lua: &Lua, data: &EffectData, blue: bool) {
    let count = ((data.magnitude.max(1.0) * 8.0) as usize).clamp(6, 48);
    let normal = if data.normal.length() > 0.1 { data.normal.normalized() } else { Vector::new(0.0, 0.0, 1.0) };
    let color = if blue { [150.0, 200.0, 255.0] } else { [255.0, 220.0, 150.0] };
    let mut list = Vec::with_capacity(count + 1);
    for _ in 0..count {
        let dir = (normal + rand_dir(lua) * 0.8).normalized();
        let mut p = particle(additive("effects/spark"), data.origin, dir * rand(lua, 150.0, 450.0), rand(lua, 0.2, 0.6));
        p.gravity = Vector::new(0.0, 0.0, -600.0);
        p.start_size = rand(lua, 0.6, 1.4);
        p.end_size = 0.0;
        p.start_length = rand(lua, 6.0, 14.0) * data.radius.clamp(0.5, 4.0).max(1.0);
        p.end_length = 0.0;
        p.color = color;
        p.collide = true;
        p.bounce = 0.3;
        list.push(p);
    }
    let mut glow = particle(additive("sprites/light_glow02_add"), data.origin + normal, Vector::default(), 0.1);
    glow.start_size = 16.0;
    glow.end_size = 4.0;
    glow.color = color;
    list.push(glow);
    particles::emit(lua, false, list);
}

/// A flash of light at a point, gone in a moment.
fn flash(lua: &Lua, pos: Vector, material: &str, size: f32, color: [f32; 3]) {
    let mut p = particle(additive(material), pos, Vector::default(), 0.12);
    p.start_size = size;
    p.end_size = size * 0.3;
    p.color = color;
    p.roll = rand(lua, 0.0, std::f32::consts::TAU);
    particles::emit(lua, false, vec![p]);
}

/// An explosion: a ball of fire that grows and fades, the smoke after it, sparks thrown out.
fn explosion(lua: &Lua, data: &EffectData) {
    let scale = if data.scale > 0.0 { data.scale } else { 1.0 };
    let mut list = Vec::new();
    for _ in 0..14 {
        let dir = rand_dir(lua);
        let mut p = particle(
            additive("effects/fire_cloud1"),
            data.origin + dir * rand(lua, 0.0, 24.0) * scale,
            dir * rand(lua, 60.0, 220.0) * scale,
            rand(lua, 0.35, 0.7),
        );
        p.air_resistance = 300.0;
        p.start_size = rand(lua, 30.0, 50.0) * scale;
        p.end_size = rand(lua, 60.0, 90.0) * scale;
        p.end_alpha = 0.0;
        p.roll = rand(lua, 0.0, std::f32::consts::TAU);
        p.roll_delta = rand(lua, -2.0, 2.0);
        list.push(p);
    }
    for _ in 0..12 {
        let dir = rand_dir(lua);
        let mut p = particle(
            material("particle/particle_smokegrenade"),
            data.origin + dir * 16.0 * scale,
            dir * rand(lua, 40.0, 140.0) * scale,
            rand(lua, 2.0, 4.0),
        );
        p.air_resistance = 120.0;
        p.gravity = Vector::new(0.0, 0.0, rand(lua, 10.0, 40.0));
        p.start_size = 40.0 * scale;
        p.end_size = rand(lua, 90.0, 140.0) * scale;
        p.start_alpha = rand(lua, 120.0, 180.0);
        p.end_alpha = 0.0;
        let grey = rand(lua, 50.0, 90.0);
        p.color = [grey; 3];
        p.roll = rand(lua, 0.0, std::f32::consts::TAU);
        p.roll_delta = rand(lua, -0.5, 0.5);
        list.push(p);
    }
    particles::emit(lua, false, list);
    let sparks_data = EffectData { normal: Vector::new(0.0, 0.0, 1.0), magnitude: 4.0, radius: 2.0, ..*data };
    sparks(lua, &sparks_data, false);
    flash(lua, data.origin, "sprites/light_glow02_add", 160.0 * scale, [255.0, 200.0, 140.0]);
}

/// Where a gun's flash is: its muzzle - as the view model shows it, for a gun its holder looks
/// down - or where the effect was put.
fn muzzle(lua: &Lua, data: &EffectData) -> Option<(Vector, Vector)> {
    let entity = data.entity;
    let owner = host(lua).entities.get(entity).map_or(EntityHandle::NULL, |e| e.owner);
    let local = host(lua).client.local_player.get();
    let first_person = owner == local && owner != EntityHandle::NULL && !host(lua).client.draw_viewer.get();
    let view = host(lua).client.view.get();
    let pos = if first_person {
        crate::anim::view_model_attachment(lua, owner, &["muzzle", "muzzle_flash", "1"])
    } else {
        usize::try_from(data.attachment - 1).ok().and_then(|i| crate::anim::attachment_world(lua, entity, i)).map(|(pos, _)| pos)
    };
    let dir = if first_person { view.angles.vectors().0 } else { data.angles.vectors().0 };
    pos.map(|pos| (pos, dir))
}

/// A gun's flash: a burst of fire out of its muzzle, and its light.
fn muzzle_flash(lua: &Lua, data: &EffectData, combine: bool) {
    let (pos, dir) = muzzle(lua, data).unwrap_or((data.origin, data.angles.vectors().0));
    let scale = if data.scale > 0.0 { data.scale } else { 1.0 };
    let material = if combine { "effects/combinemuzzle1" } else { "effects/muzzleflash1" };
    let mut list = Vec::new();
    for i in 0..4 {
        let along = i as f32 * 2.5 * scale;
        let mut p = particle(additive(material), pos + dir * along, dir * 20.0, 0.06);
        p.start_size = (6.0 - i as f32) * scale * 1.5;
        p.end_size = p.start_size * 0.5;
        p.roll = rand(lua, 0.0, std::f32::consts::TAU);
        list.push(p);
    }
    particles::emit(lua, false, list);
    let color = if combine { [180.0, 210.0, 255.0] } else { [255.0, 200.0, 120.0] };
    flash(lua, pos, "sprites/light_glow02_add", 24.0 * scale, color);
}

/// A bullet's hit: dust off the surface, bits of it thrown out.
fn impact(lua: &Lua, data: &EffectData) {
    let normal = if data.normal.length() > 0.1 { data.normal.normalized() } else { Vector::new(0.0, 0.0, 1.0) };
    let mut list = Vec::new();
    for _ in 0..3 {
        let dir = (normal + rand_dir(lua) * 0.4).normalized();
        let mut p = particle(
            material("particle/particle_smokegrenade"),
            data.origin + normal * 2.0,
            dir * rand(lua, 20.0, 60.0),
            rand(lua, 0.8, 1.6),
        );
        p.air_resistance = 200.0;
        p.gravity = Vector::new(0.0, 0.0, -20.0);
        p.start_size = rand(lua, 3.0, 5.0);
        p.end_size = rand(lua, 12.0, 20.0);
        p.start_alpha = 140.0;
        p.end_alpha = 0.0;
        p.color = [160.0, 150.0, 140.0];
        p.roll = rand(lua, 0.0, std::f32::consts::TAU);
        list.push(p);
    }
    particles::emit(lua, false, list);
    debris(lua, data, "effects/fleck_cement1", 4);
}

/// Bits of a surface, thrown out of it and falling.
fn debris(lua: &Lua, data: &EffectData, material_name: &str, count: usize) {
    let normal = if data.normal.length() > 0.1 { data.normal.normalized() } else { Vector::new(0.0, 0.0, 1.0) };
    let list = (0..count)
        .map(|_| {
            let dir = (normal + rand_dir(lua) * 0.7).normalized();
            let mut p = particle(material(material_name), data.origin + normal, dir * rand(lua, 80.0, 220.0), rand(lua, 0.6, 1.2));
            p.gravity = Vector::new(0.0, 0.0, -800.0);
            p.start_size = rand(lua, 0.6, 1.4);
            p.end_size = p.start_size;
            p.roll = rand(lua, 0.0, std::f32::consts::TAU);
            p.roll_delta = rand(lua, -8.0, 8.0);
            p.collide = true;
            p.bounce = 0.3;
            p
        })
        .collect();
    particles::emit(lua, false, list);
}

/// Blood off a wound: a red puff, and drops falling.
fn blood(lua: &Lua, data: &EffectData, yellow: bool) {
    let normal = if data.normal.length() > 0.1 { data.normal.normalized() } else { rand_dir(lua) };
    let color = if yellow { [200.0, 180.0, 40.0] } else { [110.0, 0.0, 0.0] };
    let mut list = Vec::new();
    for _ in 0..4 {
        let dir = (normal + rand_dir(lua) * 0.5).normalized();
        let mut p = particle(material("effects/blood_puff"), data.origin, dir * rand(lua, 10.0, 40.0), rand(lua, 0.4, 0.8));
        p.air_resistance = 200.0;
        p.start_size = rand(lua, 4.0, 8.0);
        p.end_size = rand(lua, 14.0, 22.0);
        p.start_alpha = 220.0;
        p.end_alpha = 0.0;
        p.color = color;
        p.roll = rand(lua, 0.0, std::f32::consts::TAU);
        list.push(p);
    }
    for _ in 0..6 {
        let dir = (normal + rand_dir(lua) * 0.6).normalized();
        let mut p = particle(material("effects/blood_core"), data.origin, dir * rand(lua, 60.0, 160.0), rand(lua, 0.5, 1.0));
        p.gravity = Vector::new(0.0, 0.0, -600.0);
        p.start_size = rand(lua, 1.0, 2.0);
        p.end_size = 0.5;
        p.color = color;
        p.collide = true;
        list.push(p);
    }
    particles::emit(lua, false, list);
}

/// Water thrown up off its surface.
fn splash(lua: &Lua, data: &EffectData) {
    let scale = if data.scale > 0.0 { (data.scale / 4.0).clamp(0.5, 4.0) } else { 1.0 };
    let list = (0..16)
        .map(|_| {
            let dir = (Vector::new(0.0, 0.0, 2.0) + rand_dir(lua)).normalized();
            let mut p = particle(material("effects/splash2"), data.origin, dir * rand(lua, 80.0, 220.0) * scale, rand(lua, 0.5, 1.0));
            p.gravity = Vector::new(0.0, 0.0, -600.0);
            p.start_size = rand(lua, 2.0, 4.0) * scale;
            p.end_size = p.start_size * 2.0;
            p.start_alpha = 200.0;
            p.end_alpha = 0.0;
            p.roll = rand(lua, 0.0, std::f32::consts::TAU);
            p
        })
        .collect();
    particles::emit(lua, false, list);
}

/// Dust kicked up off the ground, spreading low.
fn dust(lua: &Lua, data: &EffectData) {
    let scale = if data.scale > 0.0 { data.scale.clamp(0.2, 8.0) } else { 1.0 };
    let list = (0..8)
        .map(|_| {
            let mut dir = rand_dir(lua);
            dir.z = dir.z.abs() * 0.3;
            let mut p = particle(
                material("particle/particle_smokegrenade"),
                data.origin,
                dir.normalized() * rand(lua, 30.0, 90.0) * scale,
                rand(lua, 1.0, 2.0),
            );
            p.air_resistance = 150.0;
            p.start_size = 10.0 * scale;
            p.end_size = 30.0 * scale;
            p.start_alpha = 90.0;
            p.end_alpha = 0.0;
            p.color = [150.0, 140.0, 120.0];
            p.roll = rand(lua, 0.0, std::f32::consts::TAU);
            p
        })
        .collect();
    particles::emit(lua, false, list);
}

/// A bullet's streak of light, from where it was fired to where it hit, fast.
fn tracer(lua: &Lua, data: &EffectData, name: &str) {
    let start = muzzle(lua, data).map_or(data.start, |(pos, _)| pos);
    let to = data.origin - start;
    let distance = to.length();
    if distance < 1.0 {
        return;
    }
    let dir = to * (1.0 / distance);
    let speed = 6000.0;
    let (material_name, color, width) = match name.to_ascii_lowercase().as_str() {
        "ar2tracer" | "gunshiptracer" | "stridertracer" | "helicoptertracer" => ("effects/gunshiptracer", [255.0, 255.0, 255.0], 4.0),
        "gausstracer" => ("effects/spark", [255.0, 190.0, 80.0], 3.0),
        _ => ("effects/spark", [255.0, 220.0, 160.0], 1.5),
    };
    let mut p = particle(additive(material_name), start, dir * speed, distance / speed);
    p.start_size = width;
    p.end_size = width;
    p.start_length = distance.min(200.0);
    p.end_length = distance.min(200.0);
    p.color = color;
    particles::emit(lua, false, vec![p]);
}
