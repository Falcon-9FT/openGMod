//! Pictures of models for the spawn icons the game has none of (`SpawnIcon`, `ModelImage`):
//! a model drawn alone as GMod pictures it (`PositionSpawnIcon`) - from above and in front, to
//! the left, all of it in view - on nothing, and kept in the user's folder
//! (`spawnicons/models/....png`) for the next time; never in the game's.

use std::path::PathBuf;

use gmod_fs::UserDir;
use gmod_material::png::Rgba;
use gmod_world::gpu::Camera;

/// The side of an icon in pixels.
pub const SIZE: u32 = 128;

/// The camera GMod's `PositionSpawnIcon` sets for a model of these bounds at the origin: from
/// 25 degrees above, turned 220, fifteen times its size away, narrow enough to fill the icon.
pub fn camera(mins: [f32; 3], maxs: [f32; 3]) -> Camera {
    let middle = [0, 1, 2].map(|i| (mins[i] + maxs[i]) / 2.0);
    let size = (0..3).map(|i| mins[i].abs() + maxs[i].abs()).fold(0.0f32, f32::max).max(1.0);
    let mut camera = Camera { eye: [0.0; 3], yaw: 220.0, pitch: 25.0, fov: 4.0 + size * 0.04 };
    let forward = camera.forward();
    camera.eye = [0, 1, 2].map(|i| middle[i] - forward[i] * size * 15.0);
    camera
}

/// Where a model's icon is kept: `spawnicons/` in the user's folder, by the model's path; none
/// for a path that would lead out of it.
pub fn kept(model: &str) -> Option<PathBuf> {
    let model = model.replace('\\', "/").to_lowercase();
    let stem = model.strip_suffix(".mdl").unwrap_or(&model);
    let fine = |part: &str| !part.is_empty() && part != "." && part != ".." && !part.contains(':');
    if !stem.split('/').all(fine) {
        return None;
    }
    Some(UserDir::default_root()?.join("spawnicons").join(format!("{stem}.png")))
}

/// The icon kept for a model, if there is one.
pub fn read(model: &str) -> Option<Rgba> {
    let data = std::fs::read(kept(model)?).ok()?;
    gmod_material::jpeg::decode_image(&data).ok()
}

/// Keeps a model's icon for the next time.
pub fn keep(model: &str, picture: &Rgba) {
    let Some(path) = kept(model) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, gmod_material::png::encode(picture.width, picture.height, 4, &picture.pixels));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_model_is_in_view() {
        // a box 40 across, its middle 20 up
        let camera = camera([-20.0, -20.0, 0.0], [20.0, 20.0, 40.0]);
        let forward = camera.forward();
        // looking down at its middle, from the front left, 600 away
        let to = [0.0 - camera.eye[0], 0.0 - camera.eye[1], 20.0 - camera.eye[2]];
        let distance = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
        assert!((distance - 600.0).abs() < 0.5);
        assert!((0..3).all(|i| (to[i] / distance - forward[i]).abs() < 1e-4));
        assert!(camera.eye[2] > 20.0 && camera.eye[0] > 0.0 && camera.eye[1] > 0.0);
        // half of what is seen across, at its distance, takes in more than half of it
        let half = (camera.fov.to_radians() / 2.0).tan() * 0.75 * distance;
        assert!(half > 20.0 && half < 40.0, "{half}");
    }

    #[test]
    fn icons_are_kept_by_the_models_path_and_nowhere_else() {
        let path = kept("models/props_c17/oildrum001.mdl").expect("a home folder");
        assert!(path.ends_with("spawnicons/models/props_c17/oildrum001.png"));
        assert!(kept("models/../../evil.mdl").is_none());
        assert!(kept("C:/evil.mdl").is_none());
    }
}
