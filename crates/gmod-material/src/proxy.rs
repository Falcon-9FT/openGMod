//! Material proxies: the `Proxies` block of a material, which sets its parameters every frame.
//!
//! ```text
//! "Proxies"
//! {
//!     "AnimatedTexture" { "animatedtexturevar" "$basetexture" "animatedtextureframenumvar" "$frame" "animatedtextureframerate" 10 }
//!     "Sine" { "sinemin" 0.2 "sinemax" 1 "sineperiod" 2 "resultvar" "$alpha" }
//! }
//! ```
//!
//! Each proxy reads the time or other parameters and writes one, in the order they are
//! written; the next proxy sees what the last one wrote, so proxies chain through parameters
//! of the material's own (`$sinevar`, any name will do). A parameter is a number, a vector or
//! a texture transform; `$color[1]` names one component of a vector. Where a proxy takes a
//! number (`sinemax`, `rate` ...) a parameter's name gives its value instead.
//!
//! What [`Proxies::at`] works out is a pure function of the time: every frame starts again
//! from the material's values, and the noise proxies draw from a hash of the time.
//!
//! Proxies read here: `AnimatedTexture`, `TextureScroll`, `TextureTransform`, `Sine`,
//! `LinearRamp`, `CurrentTime`, `UniformNoise`, `GaussianNoise`, `Add`, `Subtract`,
//! `Multiply`, `Divide`, `Equals`, `Clamp`, `Abs`, `Frac`, `Int`. The others read the entity
//! being drawn or the game (`PlayerColor`, `ToggleTexture`, `MaterialModify`, `WaterLOD`,
//! `Camo` ...): they are listed in [`Proxies::ignored`] and leave the parameters as the
//! material sets them.

use gmod_kv::Block;

use crate::Material;
use crate::vmt::{leading_number, parse_vector};

/// A texture coordinate transform: `u' = m[0] . (u, v, 1)`, `v' = m[1] . (u, v, 1)`.
pub type Matrix = [[f32; 3]; 2];

pub const IDENTITY: Matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];

/// The value of a parameter as proxies see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Var {
    Number(f32),
    Vector([f32; 3]),
    Matrix(Matrix),
}

impl Var {
    /// A parameter's text: a transform (`center .5 .5 scale 1 1 rotate 0 translate 0 0`), a
    /// vector (`[1 0 0]`, `{255 0 0}`) or a number (0 when it is none).
    pub fn parse(text: &str) -> Var {
        let trimmed = text.trim_start();
        if trimmed.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return transform(text).map_or(Var::Number(0.0), Var::Matrix);
        }
        if trimmed.starts_with(['[', '{']) {
            return parse_vector(text).map_or(Var::Number(0.0), Var::Vector);
        }
        Var::Number(leading_number(text).unwrap_or(0.0))
    }

    /// As one number: a vector's first component; a transform counts as 0.
    pub fn number(self) -> f32 {
        match self {
            Var::Number(n) => n,
            Var::Vector(v) => v[0],
            Var::Matrix(_) => 0.0,
        }
    }

    fn vector(self) -> [f32; 3] {
        match self {
            Var::Number(n) => [n; 3],
            Var::Vector(v) => v,
            Var::Matrix(_) => [0.0; 3],
        }
    }

    /// Each component through `f`, a number with each of the other's: a number with a number
    /// is a number, anything with a vector a vector. A transform stays as it was.
    fn zip(self, other: Var, f: impl Fn(f32, f32) -> f32) -> Var {
        match (self, other) {
            (Var::Matrix(_), _) | (_, Var::Matrix(_)) => self,
            (Var::Number(a), Var::Number(b)) => Var::Number(f(a, b)),
            (a, b) => {
                let (a, b) = (a.vector(), b.vector());
                Var::Vector(std::array::from_fn(|i| f(a[i], b[i])))
            }
        }
    }

    fn map(self, f: impl Fn(f32) -> f32) -> Var {
        self.zip(self, |a, _| f(a))
    }
}

/// Reads a texture transform as materials write it: `center`, `scale`, `rotate` (degrees)
/// and `translate`, in any order, each optional. Scaled and turned about the centre, then
/// moved.
pub fn transform(text: &str) -> Option<Matrix> {
    let (mut center, mut scale, mut rotate, mut translate) = ([0.5, 0.5], [1.0, 1.0], 0.0, [0.0, 0.0]);
    let words: Vec<&str> = text.split_whitespace().collect();
    let number = |i: usize| words.get(i).and_then(|w| leading_number(w));
    let mut i = 0;
    while i < words.len() {
        match words[i].to_ascii_lowercase().as_str() {
            "center" => center = [number(i + 1)?, number(i + 2)?],
            "scale" => scale = [number(i + 1)?, number(i + 2)?],
            "translate" => translate = [number(i + 1)?, number(i + 2)?],
            "rotate" => {
                rotate = number(i + 1)?;
                i += 2;
                continue;
            }
            _ => return None,
        }
        i += 3;
    }
    Some(compose(center, scale, rotate, translate))
}

/// The transform that scales and turns about `center`, then moves by `translate`.
fn compose(center: [f32; 2], scale: [f32; 2], rotate: f32, translate: [f32; 2]) -> Matrix {
    let (s, c) = rotate.to_radians().sin_cos();
    let row = |a: f32, b: f32, k: usize| {
        let (a, b) = (a * scale[0], b * scale[1]);
        [a, b, center[k] + translate[k] - a * center[0] - b * center[1]]
    };
    [row(c, -s, 0), row(s, c, 1)]
}

/// Applies a transform to texture coordinates.
pub fn apply(m: &Matrix, [u, v]: [f32; 2]) -> [f32; 2] {
    [m[0][0] * u + m[0][1] * v + m[0][2], m[1][0] * u + m[1][1] * v + m[1][2]]
}

/// A parameter, or one component of it (`$color[1]`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ref {
    var: usize,
    component: Option<usize>,
}

/// A number a proxy takes: written out, or a parameter's.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Input {
    Value(f32),
    Var(Ref),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Sine { period: Input, min: Input, max: Input, offset: Input, result: Ref },
    LinearRamp { rate: Input, initial: Input, result: Ref },
    CurrentTime { result: Ref },
    UniformNoise { min: Input, max: Input, result: Ref },
    GaussianNoise { mean: Input, half_width: Input, min: Input, max: Input, result: Ref },
    Math { kind: MathKind, a: Ref, b: Ref, result: Ref },
    Clamp { min: Input, max: Input, src: Ref, result: Ref },
    Each { kind: EachKind, src: Ref, result: Ref },
    AnimatedTexture { frame: Ref, rate: Input },
    TextureScroll { result: Ref, rate: Input, angle: Input, scale: Input },
    TextureTransform { center: Option<Ref>, scale: Option<Ref>, rotate: Option<Ref>, translate: Option<Ref>, result: Ref },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MathKind {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum EachKind {
    Equals,
    Abs,
    Frac,
    Int,
}

/// A material's proxies, ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct Proxies {
    /// The parameters proxies touch (lower case) and their values in the material.
    names: Vec<String>,
    initial: Vec<Var>,
    ops: Vec<Op>,
    /// Proxies not run: those that need an entity or the game, and unknown ones (lower case).
    pub ignored: Vec<String>,
}

/// The parameters proxies touched at a moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Vars<'a> {
    proxies: &'a Proxies,
    values: Vec<Var>,
}

impl Vars<'_> {
    /// A parameter proxies read or wrote, by name (`$alpha`).
    pub fn get(&self, name: &str) -> Option<Var> {
        let i = self.proxies.names.iter().position(|n| n.eq_ignore_ascii_case(name))?;
        Some(self.values[i])
    }

    pub fn number(&self, name: &str) -> Option<f32> {
        self.get(name).map(Var::number)
    }

    /// A vector parameter; a number counts as all three components.
    pub fn vector(&self, name: &str) -> Option<[f32; 3]> {
        match self.get(name)? {
            Var::Matrix(_) => None,
            var => Some(var.vector()),
        }
    }

    pub fn matrix(&self, name: &str) -> Option<Matrix> {
        match self.get(name)? {
            Var::Matrix(m) => Some(m),
            _ => None,
        }
    }
}

/// What a parameter is before a material sets it: what the shaders take it as.
fn default_of(name: &str) -> Var {
    match name {
        "$alpha" => Var::Number(1.0),
        "$color" | "$color2" => Var::Vector([1.0; 3]),
        _ if name.ends_with("transform") => Var::Matrix(IDENTITY),
        _ => Var::Number(0.0),
    }
}

/// 64 bits of a hash of `x`, for noise that is the same for the same moment.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A number from 0 to 1 (not 1) for the moment `time` and the proxy `salt`.
fn noise(time: f32, salt: u64) -> f32 {
    (mix(u64::from(time.to_bits()) ^ mix(salt)) >> 40) as f32 / (1u64 << 24) as f32
}

impl Proxies {
    /// The proxies of a material; `None` when it has none that run.
    pub fn new(material: &Material) -> Option<Proxies> {
        let block = material.proxies()?;
        let mut proxies = Proxies { names: Vec::new(), initial: Vec::new(), ops: Vec::new(), ignored: Vec::new() };
        for pair in block.iter() {
            let name = pair.key.to_ascii_lowercase();
            let empty = Block::default();
            let params = pair.value.as_block().unwrap_or(&empty);
            match proxies.op(material, &name, params) {
                Some(op) => proxies.ops.push(op),
                None => proxies.ignored.push(name),
            }
        }
        (!proxies.ops.is_empty()).then_some(proxies)
    }

    /// Whether a proxy writes a parameter (`$alpha`).
    pub fn writes(&self, name: &str) -> bool {
        let written = |r: &Ref| self.names[r.var].eq_ignore_ascii_case(name);
        self.ops.iter().any(|op| match op {
            Op::Sine { result, .. }
            | Op::LinearRamp { result, .. }
            | Op::CurrentTime { result }
            | Op::UniformNoise { result, .. }
            | Op::GaussianNoise { result, .. }
            | Op::Math { result, .. }
            | Op::Clamp { result, .. }
            | Op::Each { result, .. }
            | Op::TextureScroll { result, .. }
            | Op::TextureTransform { result, .. } => written(result),
            Op::AnimatedTexture { frame, .. } => written(frame),
        })
    }

    /// A parameter by name, added with its value in the material the first time.
    fn var(&mut self, material: &Material, text: &str) -> Ref {
        let text = text.trim().to_ascii_lowercase();
        // `$color[1]`: one component
        let (name, component) = match text.strip_suffix(']').and_then(|t| t.split_once('[')) {
            Some((name, index)) => (name.trim().to_string(), index.trim().parse::<usize>().ok().filter(|&i| i < 3)),
            None => (text, None),
        };
        let var = match self.names.iter().position(|n| *n == name) {
            Some(i) => i,
            None => {
                self.initial.push(material.get(&name).map_or_else(|| default_of(&name), Var::parse));
                self.names.push(name);
                self.names.len() - 1
            }
        };
        Ref { var, component }
    }

    /// A number a proxy takes: a parameter's when it names one, else as written; `default`
    /// when it is not there.
    fn input(&mut self, material: &Material, params: &Block, key: &str, default: f32) -> Input {
        match params.str(key) {
            Some(text) if text.trim_start().starts_with('$') => Input::Var(self.var(material, text)),
            Some(text) => Input::Value(leading_number(text).unwrap_or(default)),
            None => Input::Value(default),
        }
    }

    fn op(&mut self, m: &Material, name: &str, p: &Block) -> Option<Op> {
        let var = |this: &mut Proxies, key: &str| p.str(key).filter(|t| !t.trim().is_empty()).map(|t| this.var(m, t));
        let result = var(self, "resultvar");
        let math = match name {
            "add" => Some(MathKind::Add),
            "subtract" => Some(MathKind::Subtract),
            "multiply" => Some(MathKind::Multiply),
            "divide" => Some(MathKind::Divide),
            _ => None,
        };
        if let Some(kind) = math {
            return Some(Op::Math { kind, a: var(self, "srcvar1")?, b: var(self, "srcvar2")?, result: result? });
        }
        let each = match name {
            "equals" => Some(EachKind::Equals),
            "abs" => Some(EachKind::Abs),
            "frac" => Some(EachKind::Frac),
            "int" => Some(EachKind::Int),
            _ => None,
        };
        if let Some(kind) = each {
            return Some(Op::Each { kind, src: var(self, "srcvar1")?, result: result? });
        }
        Some(match name {
            "sine" => Op::Sine {
                period: self.input(m, p, "sineperiod", 1.0),
                min: self.input(m, p, "sinemin", 0.0),
                max: self.input(m, p, "sinemax", 1.0),
                offset: self.input(m, p, "timeoffset", 0.0),
                result: result?,
            },
            "linearramp" => {
                Op::LinearRamp { rate: self.input(m, p, "rate", 1.0), initial: self.input(m, p, "initialvalue", 0.0), result: result? }
            }
            "currenttime" => Op::CurrentTime { result: result? },
            "uniformnoise" => {
                Op::UniformNoise { min: self.input(m, p, "minval", 0.0), max: self.input(m, p, "maxval", 1.0), result: result? }
            }
            "gaussiannoise" => Op::GaussianNoise {
                mean: self.input(m, p, "mean", 0.0),
                half_width: self.input(m, p, "halfwidth", 0.0),
                min: self.input(m, p, "minval", f32::MIN),
                max: self.input(m, p, "maxval", f32::MAX),
                result: result?,
            },
            "clamp" => Op::Clamp {
                min: self.input(m, p, "min", 0.0),
                max: self.input(m, p, "max", 1.0),
                src: var(self, "srcvar1")?,
                result: result?,
            },
            "animatedtexture" => Op::AnimatedTexture {
                frame: var(self, "animatedtextureframenumvar").unwrap_or_else(|| self.var(m, "$frame")),
                rate: self.input(m, p, "animatedtextureframerate", 15.0),
            },
            "texturescroll" => Op::TextureScroll {
                result: var(self, "texturescrollvar").unwrap_or_else(|| self.var(m, "$basetexturetransform")),
                rate: self.input(m, p, "texturescrollrate", 1.0),
                angle: self.input(m, p, "texturescrollangle", 0.0),
                scale: self.input(m, p, "texturescale", 1.0),
            },
            "texturetransform" => Op::TextureTransform {
                center: var(self, "centervar"),
                scale: var(self, "scalevar"),
                rotate: var(self, "rotatevar"),
                translate: var(self, "translatevar"),
                result: result.unwrap_or_else(|| self.var(m, "$basetexturetransform")),
            },
            _ => return None,
        })
    }

    /// The parameters at `time` (seconds): the material's, as the proxies set them in turn.
    pub fn at(&self, time: f32) -> Vars<'_> {
        let mut values = self.initial.clone();
        let read = |values: &[Var], r: Ref| match r.component {
            Some(i) => Var::Number(values[r.var].vector()[i]),
            None => values[r.var],
        };
        let input = |values: &[Var], i: Input| match i {
            Input::Value(v) => v,
            Input::Var(r) => read(values, r).number(),
        };
        let write = |values: &mut [Var], r: Ref, var: Var| match r.component {
            Some(i) => {
                let mut v = values[r.var].vector();
                v[i] = var.number();
                values[r.var] = Var::Vector(v);
            }
            None => values[r.var] = var,
        };
        for (n, op) in self.ops.iter().enumerate() {
            let salt = n as u64;
            match *op {
                Op::Sine { period, min, max, offset, result } => {
                    let period = input(&values, period);
                    let (min, max) = (input(&values, min), input(&values, max));
                    let phase = if period == 0.0 { 0.0 } else { (time - input(&values, offset)) / period };
                    let wave = (std::f32::consts::TAU * phase).sin() * 0.5 + 0.5;
                    write(&mut values, result, Var::Number(min + (max - min) * wave));
                }
                Op::LinearRamp { rate, initial, result } => {
                    let v = input(&values, initial) + input(&values, rate) * time;
                    write(&mut values, result, Var::Number(v));
                }
                Op::CurrentTime { result } => write(&mut values, result, Var::Number(time)),
                Op::UniformNoise { min, max, result } => {
                    let (min, max) = (input(&values, min), input(&values, max));
                    write(&mut values, result, Var::Number(min + (max - min) * noise(time, salt)));
                }
                Op::GaussianNoise { mean, half_width, min, max, result } => {
                    // Box-Muller: a normal distribution from two uniform numbers
                    let (a, b) = (noise(time, salt).max(1e-7), noise(time, salt ^ 0x5bd1_e995));
                    let normal = (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos();
                    let v = input(&values, mean) + input(&values, half_width) * normal;
                    let (min, max) = (input(&values, min), input(&values, max));
                    write(&mut values, result, Var::Number(v.max(min).min(max)));
                }
                Op::Math { kind, a, b, result } => {
                    let (a, b) = (read(&values, a), read(&values, b));
                    let v = match kind {
                        MathKind::Add => a.zip(b, |x, y| x + y),
                        MathKind::Subtract => a.zip(b, |x, y| x - y),
                        MathKind::Multiply => a.zip(b, |x, y| x * y),
                        // nothing comes of dividing by 0, as the engine has it
                        MathKind::Divide => a.zip(b, |x, y| if y == 0.0 { x } else { x / y }),
                    };
                    write(&mut values, result, v);
                }
                Op::Clamp { min, max, src, result } => {
                    let (min, max) = (input(&values, min), input(&values, max));
                    let v = read(&values, src).map(|x| x.max(min).min(max));
                    write(&mut values, result, v);
                }
                Op::Each { kind, src, result } => {
                    let v = read(&values, src);
                    let v = match kind {
                        EachKind::Equals => v,
                        EachKind::Abs => v.map(f32::abs),
                        EachKind::Frac => v.map(|x| x - x.floor()),
                        EachKind::Int => v.map(f32::trunc),
                    };
                    write(&mut values, result, v);
                }
                Op::AnimatedTexture { frame, rate } => {
                    // the frame counts on; who draws it wraps it to the frames the texture has
                    let v = (input(&values, rate) * time).max(0.0).floor();
                    write(&mut values, frame, Var::Number(v));
                }
                Op::TextureScroll { result, rate, angle, scale } => {
                    let (s, c) = input(&values, angle).to_radians().sin_cos();
                    // whole repeats change nothing; dropping them keeps precision as time goes on
                    let offset = |d: f32| (d * input(&values, rate) * time).fract();
                    let (u, v) = (offset(c), offset(s));
                    let scale = input(&values, scale);
                    let var = match values[result.var] {
                        _ if result.component.is_some() => Var::Number(u),
                        Var::Vector(_) => Var::Vector([u, v, 0.0]),
                        _ => Var::Matrix([[scale, 0.0, u], [0.0, scale, v]]),
                    };
                    write(&mut values, result, var);
                }
                Op::TextureTransform { center, scale, rotate, translate, result } => {
                    let pair = |r: Option<Ref>, default: [f32; 2]| {
                        r.map_or(default, |r| match read(&values, r) {
                            Var::Number(n) => [n, n],
                            Var::Vector(v) => [v[0], v[1]],
                            Var::Matrix(_) => default,
                        })
                    };
                    let m = compose(
                        pair(center, [0.5, 0.5]),
                        pair(scale, [1.0, 1.0]),
                        rotate.map_or(0.0, |r| read(&values, r).number()),
                        pair(translate, [0.0, 0.0]),
                    );
                    write(&mut values, result, Var::Matrix(m));
                }
            }
        }
        Vars { proxies: self, values }
    }
}
