// The sky box: six faces around the eye, behind everything.

struct Camera {
    view_projection: mat4x4<f32>,
    eye: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var sky_texture: texture_2d<f32>;
@group(1) @binding(1) var sky_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) direction: vec3<f32>, @location(1) uv: vec2<f32>) -> VertexOutput {
    var output: VertexOutput;
    let clip = camera.view_projection * vec4<f32>(direction, 1.0);
    // infinitely far: depth 0, as depth runs from 1 at the eye to 0
    output.clip = vec4<f32>(clip.xy, 0.0, clip.w);
    output.uv = uv;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(sky_texture, sky_sampler, input.uv).rgb, 1.0);
}
