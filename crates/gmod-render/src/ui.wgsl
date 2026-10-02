// The 2D of the HUD and the panels over the 3D view: triangles in pixels from the top left
// corner, a texture times a colour.

struct Screen {
    size: vec2<f32>,
    // 1 when the target is sRGB: the colours, given as the screen shows them, are made
    // linear first
    srgb: f32,
    pad: f32,
};

@group(0) @binding(0) var<uniform> screen: Screen;
@group(1) @binding(0) var ui_texture: texture_2d<f32>;
@group(1) @binding(1) var ui_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> VertexOutput {
    var output: VertexOutput;
    output.clip = vec4<f32>(pos.x / screen.size.x * 2.0 - 1.0, 1.0 - pos.y / screen.size.y * 2.0, 0.0, 1.0);
    output.uv = uv;
    output.color = color;
    if (screen.srgb > 0.5) {
        output.color = vec4<f32>(linear(color.rgb), color.a);
    }
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(ui_texture, ui_sampler, input.uv) * input.color;
}
