// What the game draws in the world frame by frame - beams, sprites, quads (the physics gun's
// beam, the effects and drawings of Lua): triangles in the world with a texture and a colour
// at each corner, blended over what is behind them or added to it.

struct Camera {
    view_projection: mat4x4<f32>,
    eye: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var base: texture_2d_array<f32>;
@group(1) @binding(1) var base_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    // sRGB as the game gives it, with alpha
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip = camera.view_projection * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    output.color = vec4<f32>(to_linear(input.color.rgb), input.color.a);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(base, base_sampler, input.uv, 0) * input.color;
}
