use super::*;
use crate::proxy::{IDENTITY, Matrix, Var, apply, transform};

fn proxies(body: &str, params: &str) -> Proxies {
    let m = material(&format!("\"UnlitGeneric\"\n{{\n{params}\n\"Proxies\"\n{{\n{body}\n}}\n}}\n"));
    Proxies::new(&m).expect("proxies that run")
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

fn close_matrix(a: Matrix, b: Matrix) -> bool {
    a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| close(*x, *y))
}

#[test]
fn sine_and_ramps_follow_the_time() {
    let p = proxies(
        r#""Sine" { "sinemin" 2 "sinemax" 4 "sineperiod" 4 "resultvar" "$alpha" }
           "LinearRamp" { "rate" 3 "initialvalue" 1 "resultvar" "$ramp" }
           "CurrentTime" { "resultvar" "$now" }"#,
        "",
    );
    // a quarter period in: the top of the wave; three quarters: the bottom
    assert!(close(p.at(0.0).number("$alpha").unwrap(), 3.0));
    assert!(close(p.at(1.0).number("$alpha").unwrap(), 4.0));
    assert!(close(p.at(3.0).number("$alpha").unwrap(), 2.0));
    assert!(close(p.at(2.0).number("$ramp").unwrap(), 7.0));
    assert_eq!(p.at(5.5).number("$now"), Some(5.5));
    assert!(p.writes("$alpha") && p.writes("$ramp") && !p.writes("$color"));
    assert_eq!(p.at(1.0).number("$nothing"), None);
}

#[test]
fn proxies_chain_through_parameters() {
    // the ramp feeds a product, the product a sum; `sinemax` reads a parameter; a component of
    // the colour is written alone
    let p = proxies(
        r#""LinearRamp" { "rate" 1 "initialvalue" 0 "resultvar" "$t" }
           "Multiply" { "srcvar1" "$t" "srcvar2" "$two" "resultvar" "$double" }
           "Add" { "srcvar1" "$double" "srcvar2" "$one" "resultvar" "$sum" }
           "Subtract" { "srcvar1" "$sum" "srcvar2" "$two" "resultvar" "$less" }
           "Divide" { "srcvar1" "$sum" "srcvar2" "$zero" "resultvar" "$divided" }
           "Clamp" { "min" 0 "max" 4 "srcvar1" "$sum" "resultvar" "$clamped" }
           "Equals" { "srcvar1" "$clamped" "resultvar" "$color[1]" }
           "Frac" { "srcvar1" "$t" "resultvar" "$frac" }
           "Sine" { "sinemin" 0 "sinemax" "$two" "sineperiod" 4 "resultvar" "$wave" }"#,
        r#""$two" 2 "$one" 1 "$zero" 0 "$color" "[1 0.5 0.25]""#,
    );
    let at = p.at(2.5);
    assert_eq!(at.number("$double"), Some(5.0));
    assert_eq!(at.number("$sum"), Some(6.0));
    assert_eq!(at.number("$less"), Some(4.0));
    assert_eq!(at.number("$divided"), Some(6.0), "dividing by 0 leaves the first");
    assert_eq!(at.number("$clamped"), Some(4.0));
    assert_eq!(at.vector("$color"), Some([1.0, 4.0, 0.25]));
    assert_eq!(at.number("$frac"), Some(0.5));
    assert!(close(p.at(1.0).number("$wave").unwrap(), 2.0), "the top of a wave up to $two");
    // vectors and numbers mix component by component
    let p = proxies(r#""Multiply" { "srcvar1" "$color" "srcvar2" "$half" "resultvar" "$color" }"#, r#""$color" "{255 0 255}" "$half" 0.5"#);
    assert_eq!(p.at(0.0).vector("$color"), Some([0.5, 0.0, 0.5]));
}

#[test]
fn animated_textures_count_frames() {
    let p = proxies(
        r#""AnimatedTexture" { "animatedtexturevar" "$basetexture" "animatedtextureframenumvar" "$frame" "animatedtextureframerate" 10 }"#,
        r#""$basetexture" "dev/water""#,
    );
    assert_eq!(p.at(0.0).number("$frame"), Some(0.0));
    assert_eq!(p.at(0.15).number("$frame"), Some(1.0));
    assert_eq!(p.at(2.0).number("$frame"), Some(20.0), "who draws it wraps it to its frames");
    // the frame rate defaults to 15
    let p = proxies(r#""AnimatedTexture" { "animatedtexturevar" "$basetexture" }"#, "");
    assert_eq!(p.at(1.0).number("$frame"), Some(15.0));
}

#[test]
fn texture_scroll_moves_the_transform() {
    let p =
        proxies(r#""TextureScroll" { "texturescrollvar" "$basetexturetransform" "texturescrollrate" 0.25 "texturescrollangle" 90 }"#, "");
    assert!(close_matrix(p.at(0.0).matrix("$basetexturetransform").unwrap(), IDENTITY));
    // a quarter of the texture a second, along v
    let m = p.at(1.0).matrix("$basetexturetransform").unwrap();
    assert!(close_matrix(m, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.25]]), "{m:?}");
    // whole repeats are dropped
    let m = p.at(5.0).matrix("$basetexturetransform").unwrap();
    assert!(close(m[1][2], 0.25), "{m:?}");
    // into a vector
    let p = proxies(r#""TextureScroll" { "texturescrollvar" "$offset" "texturescrollrate" 0.5 }"#, r#""$offset" "[0 0 0]""#);
    assert_eq!(p.at(1.0).vector("$offset"), Some([0.5, 0.0, 0.0]));
}

#[test]
fn texture_transforms() {
    let m = transform("center .5 .5 scale 2 2 rotate 90 translate 0.25 0").unwrap();
    // scaled and turned about the centre, then moved
    let [u, v] = apply(&m, [1.0, 0.5]);
    assert!(close(u, 0.75) && close(v, 1.5), "{u} {v}");
    assert_eq!(transform("scale 1 1"), Some(IDENTITY));
    assert_eq!(transform("bogus 1 1"), None);
    assert_eq!(transform("scale 1"), None);
    assert_eq!(Var::parse("center .5 .5 scale 1 1 rotate 0 translate 0 0"), Var::Matrix(IDENTITY));
    assert_eq!(Var::parse("[1 2 3]"), Var::Vector([1.0, 2.0, 3.0]));
    assert_eq!(Var::parse("0.5"), Var::Number(0.5));
    assert_eq!(Var::parse("rubbish"), Var::Number(0.0));

    // the TextureTransform proxy builds one from parameters: a ramp turns the texture
    let p = proxies(
        r#""LinearRamp" { "rate" 90 "initialvalue" 0 "resultvar" "$angle" }
           "TextureTransform" { "rotatevar" "$angle" "resultvar" "$basetexturetransform" }"#,
        "",
    );
    let m = p.at(1.0).matrix("$basetexturetransform").unwrap();
    let [u, v] = apply(&m, [1.0, 0.5]);
    assert!(close(u, 0.5) && close(v, 1.0), "{u} {v}");
}

#[test]
fn noise_stays_in_range_and_moves() {
    let p = proxies(
        r#""UniformNoise" { "minval" 2 "maxval" 3 "resultvar" "$u" }
           "GaussianNoise" { "mean" 0 "halfwidth" 1 "minval" -0.5 "maxval" 0.5 "resultvar" "$g" }"#,
        "",
    );
    let values: Vec<f32> = (0..50).map(|i| p.at(i as f32 * 0.1).number("$u").unwrap()).collect();
    assert!(values.iter().all(|v| (2.0..3.0).contains(v)), "{values:?}");
    assert!(values.windows(2).any(|w| w[0] != w[1]), "noise that does not change");
    assert_eq!(p.at(1.0), p.at(1.0), "the same moment, the same noise");
    assert!((0..50).all(|i| (-0.5..=0.5).contains(&p.at(i as f32).number("$g").unwrap())));
}

#[test]
fn proxies_of_entities_are_left_alone() {
    let m = material(
        r#""VertexLitGeneric"
        {
            "$color2" "[1 1 1]"
            "Proxies"
            {
                "PlayerColor" { "resultvar" "$color2" "default" "1 0 0" }
                "MaterialModify" { }
                "Multiply" { "srcvar1" "$missing" }
            }
        }"#,
    );
    assert_eq!(Proxies::new(&m), None, "nothing to run");
    let p = proxies(r#""PlayerColor" { "resultvar" "$color2" } "CurrentTime" { "resultvar" "$t" }"#, r#""$color2" "[0 1 0]""#);
    assert_eq!(p.ignored, ["playercolor"]);
    assert_eq!(p.at(3.0).vector("$color2"), Some([0.0, 1.0, 0.0]));
    assert!(Proxies::new(&material("\"UnlitGeneric\" { \"$alpha\" 1 }")).is_none());
}
