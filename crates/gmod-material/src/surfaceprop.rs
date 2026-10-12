//! Surface properties: what a surface is made of, for physics and for the sounds it makes.
//!
//! `scripts/surfaceproperties_manifest.txt` lists files (`"file"` keys) of KeyValues, a block
//! a surface:
//!
//! ```text
//! "wood"
//! {
//!     "base"        "default"           // everything not given here comes from it
//!     "density"     "700"
//!     "stepleft"    "Wood.StepLeft"     // sound scripts
//!     "impacthard"  "Wood.ImpactHard"
//!     "gamematerial" "W"                // MAT_WOOD, the character's code
//! }
//! ```
//!
//! Materials name theirs with `$surfaceprop`, models in their `.phy`. Surfaces are numbered in
//! the order they are read, `default` first, as `SurfaceProps` in a trace counts them.

use std::collections::HashMap;

use gmod_fs::Vfs;

/// One surface.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceProp {
    pub name: String,
    /// Kilograms a cubic metre.
    pub density: f32,
    pub elasticity: f32,
    pub friction: f32,
    pub dampening: f32,
    /// Sound scripts: footsteps, impacts, scraping, bullets, breaking.
    pub step_left: String,
    pub step_right: String,
    pub impact_soft: String,
    pub impact_hard: String,
    pub scrape_smooth: String,
    pub scrape_rough: String,
    pub bullet_impact: String,
    pub break_sound: String,
    /// `MAT_*`: the code of the `gamematerial` character (`C` concrete, `M` metal ...).
    pub game_material: u8,
}

impl Default for SurfaceProp {
    fn default() -> SurfaceProp {
        SurfaceProp {
            name: "default".to_string(),
            density: 2000.0,
            elasticity: 0.25,
            friction: 0.8,
            dampening: 0.0,
            step_left: "Default.StepLeft".to_string(),
            step_right: "Default.StepRight".to_string(),
            impact_soft: "Default.ImpactSoft".to_string(),
            impact_hard: "Default.ImpactHard".to_string(),
            scrape_smooth: "Default.ScrapeSmooth".to_string(),
            scrape_rough: "Default.ScrapeRough".to_string(),
            bullet_impact: "Default.BulletImpact".to_string(),
            break_sound: String::new(),
            game_material: b'C',
        }
    }
}

/// The surfaces of a game.
#[derive(Debug, Clone)]
pub struct SurfaceProps {
    list: Vec<SurfaceProp>,
    by_name: HashMap<String, usize>,
}

impl Default for SurfaceProps {
    fn default() -> SurfaceProps {
        SurfaceProps { list: vec![SurfaceProp::default()], by_name: HashMap::from([("default".to_string(), 0)]) }
    }
}

impl SurfaceProps {
    /// The surfaces of the files the manifest lists.
    pub fn load(vfs: &Vfs) -> SurfaceProps {
        let mut props = SurfaceProps::default();
        let Ok(data) = vfs.read("scripts/surfaceproperties_manifest.txt", "GAME") else { return props };
        let (document, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&data), gmod_kv::Options::default());
        let Some(manifest) = document.root.iter().find_map(|p| p.value.as_block()) else { return props };
        for pair in manifest {
            if pair.key.eq_ignore_ascii_case("file")
                && let Some(file) = pair.value.as_str()
                && let Ok(data) = vfs.read(file, "GAME")
            {
                props.add_file(&gmod_kv::decode_text(&data));
            }
        }
        props
    }

    /// Adds the surfaces of one file's text; one defined again is replaced, keeping its number.
    pub fn add_file(&mut self, text: &str) {
        let (document, _) = gmod_kv::parse_lenient(text, gmod_kv::Options::default());
        for pair in &document.root {
            let Some(block) = pair.value.as_block() else { continue };
            let name = pair.key.to_lowercase();
            let mut prop = block
                .str("base")
                .and_then(|base| self.get(base))
                .cloned()
                .unwrap_or_else(|| if name == "default" { SurfaceProp::default() } else { self.list[0].clone() });
            prop.name = name.clone();
            let number = |key: &str| block.str(key).and_then(|v| v.trim().parse::<f32>().ok());
            for (key, field) in [
                ("density", &mut prop.density),
                ("elasticity", &mut prop.elasticity),
                ("friction", &mut prop.friction),
                ("dampening", &mut prop.dampening),
            ] {
                if let Some(value) = number(key) {
                    *field = value;
                }
            }
            for (key, field) in [
                ("stepleft", &mut prop.step_left),
                ("stepright", &mut prop.step_right),
                ("impactsoft", &mut prop.impact_soft),
                ("impacthard", &mut prop.impact_hard),
                ("scrapesmooth", &mut prop.scrape_smooth),
                ("scraperough", &mut prop.scrape_rough),
                ("bulletimpact", &mut prop.bullet_impact),
                ("break", &mut prop.break_sound),
            ] {
                if let Some(value) = block.str(key) {
                    *field = value.to_string();
                }
            }
            if let Some(&c) = block.str("gamematerial").and_then(|v| v.trim().as_bytes().first()) {
                prop.game_material = c;
            }
            match self.by_name.get(&name) {
                Some(&index) => self.list[index] = prop,
                None => {
                    self.by_name.insert(name, self.list.len());
                    self.list.push(prop);
                }
            }
        }
    }

    /// A surface by name, any case.
    pub fn get(&self, name: &str) -> Option<&SurfaceProp> {
        self.index(name).map(|i| &self.list[i])
    }

    /// A surface's number.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_lowercase()).copied()
    }

    /// A surface by name, or `default` for one there is not.
    pub fn or_default(&self, name: &str) -> &SurfaceProp {
        self.get(name).unwrap_or(&self.list[0])
    }

    /// A surface by number.
    pub fn at(&self, index: usize) -> Option<&SurfaceProp> {
        self.list.get(index)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}
