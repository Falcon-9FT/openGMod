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
    // the lights of the moment, in the same space as the vertices: points (xyz, radius - 0
    // for none) and their light, then a cone: position and reach, direction and the cosine
    // of its edge, light and the cosine where it starts to fade (reach 0 for none)
    point_pos: array<vec4<f32>, 4>,
    point_color: array<vec4<f32>, 4>,
    spot_pos: vec4<f32>,
    spot_dir: vec4<f32>,
    spot_color: vec4<f32>,
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
    @location(4) position: vec3<f32>,
    @location(5) normal: vec3<f32>,
};

/// The lights of the moment on a point facing `n`.
fn moment_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    var light = vec3<f32>(0.0);
    for (var i = 0; i < 4; i++) {
        let radius = camera.point_pos[i].w;
        if (radius > 0.0) {
            let to = camera.point_pos[i].xyz - p;
            let d = length(to);
            let fall = clamp(1.0 - d / radius, 0.0, 1.0);
            let facing = select(1.0, max(dot(n, to / max(d, 1e-3)), 0.0), length(n) > 0.5);
            light += camera.point_color[i].rgb * fall * fall * facing;
        }
    }
    let far = camera.spot_pos.w;
    if (far > 0.0) {
        let to = p - camera.spot_pos.xyz;
        let d = length(to);
        let along = dot(to / max(d, 1e-3), camera.spot_dir.xyz);
        let cone = smoothstep(camera.spot_dir.w, camera.spot_color.w, along);
        let fall = clamp(1.0 - d / far, 0.0, 1.0);
        let facing = select(1.0, max(dot(n, -to / max(d, 1e-3)), 0.0), length(n) > 0.5);
        light += camera.spot_color.rgb * cone * fall * facing;
    }
    return light;
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip = camera.view_projection * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    output.lightmap_uv = input.lightmap_uv;
    output.alpha = input.alpha;
    output.light = input.light;
    output.position = input.position;
    output.normal = input.normal;
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
    if (surface.options.y > 0.5) {
        let n = input.normal / max(length(input.normal), 1e-6);
        light += moment_light(input.position, n);
    }
    return vec4<f32>(albedo.rgb * surface.tint.rgb * light, albedo.a * surface.tint.a);
}
