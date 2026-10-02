// The world, brush entities, props and the models of entities: base texture (blended with a
// second one on displacements), times light from the light map or the vertex, times the tint.

struct Camera {
    view_projection: mat4x4<f32>,
    eye: vec4<f32>,
    // for a model of an entity, in the model's space: the ambient light cube (+x, -x, +y, -y,
    // +z, -z; w of the first 1), the directions towards its lights (w 1 for a light) and their
    // light; the world (w 0) has its light baked
    ambient: array<vec4<f32>, 6>,
    light_dir: array<vec4<f32>, 2>,
    light_color: array<vec4<f32>, 2>,
};

struct Surface {
    // rgb tint, alpha
    tint: vec4<f32>,
    // rgb fog colour, w: 1 for water
    water: vec4<f32>,
    // x: alpha test reference (0: none), y: light (0 none, 1 light map, 2 vertex),
    // z: 1 when the second texture blends in
    options: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> surface: Surface;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var blend_texture: texture_2d<f32>;
@group(1) @binding(3) var texture_sampler: sampler;
@group(2) @binding(0) var lightmap: texture_2d<f32>;
@group(2) @binding(1) var lightmap_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) lightmap_uv: vec2<f32>,
    @location(3) alpha: f32,
    @location(4) light: vec3<f32>,
    @location(5) normal: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) lightmap_uv: vec2<f32>,
    @location(2) alpha: f32,
    @location(3) light: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip = camera.view_projection * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    output.lightmap_uv = input.lightmap_uv;
    output.alpha = input.alpha;
    output.light = input.light;
    if (camera.ambient[0].w > 0.5) {
        // the cube, each side by the square of the normal along it, and the lights that face it
        let n = input.normal / max(length(input.normal), 1e-6);
        let w = n * n;
        let x = select(camera.ambient[1].rgb, camera.ambient[0].rgb, n.x >= 0.0);
        let y = select(camera.ambient[3].rgb, camera.ambient[2].rgb, n.y >= 0.0);
        let z = select(camera.ambient[5].rgb, camera.ambient[4].rgb, n.z >= 0.0);
        var light = w.x * x + w.y * y + w.z * z;
        for (var i = 0; i < 2; i++) {
            if (camera.light_dir[i].w > 0.5) {
                light += max(dot(n, camera.light_dir[i].xyz), 0.0) * camera.light_color[i].rgb;
            }
        }
        output.light = light;
    }
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // sampled before any branch: texture lookups need every pixel of a quad to take them
    var albedo = textureSample(base_texture, texture_sampler, input.uv);
    let blended = textureSample(blend_texture, texture_sampler, input.uv);
    let mapped = textureSample(lightmap, lightmap_sampler, input.lightmap_uv).rgb;
    if (surface.options.z > 0.5) {
        albedo = mix(albedo, blended, clamp(input.alpha, 0.0, 1.0));
    }
    if (surface.options.x > 0.0 && albedo.a < surface.options.x) {
        discard;
    }
    if (surface.water.w > 0.5) {
        return vec4<f32>(surface.water.rgb, 0.75);
    }
    var light = vec3<f32>(1.0, 1.0, 1.0);
    if (surface.options.y > 1.5) {
        light = input.light;
    } else if (surface.options.y > 0.5) {
        light = mapped;
    }
    return vec4<f32>(albedo.rgb * surface.tint.rgb * light, albedo.a * surface.tint.a);
}
