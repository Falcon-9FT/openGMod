// The world, brush entities, props and the models of entities: base texture (blended with a
// second one on displacements), times light from the light map or the vertex - by the bump
// map from the three bumped light maps where there is one - times the tint;
// then fog by the distance from the eye. The base texture's coordinates go through its
// transform, and an animated one shows the layer of its frame.

struct Camera {
    view_projection: mat4x4<f32>,
    // in the space of the vertices; w: how many units of the world one of theirs is
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
    // a posed model: x its first bone in `bones`, y 1 when it is posed
    skin: vec4<f32>,
    // fog: rgb its colour, w how much it covers at most; x where it starts, y one over how
    // far it thickens, z 1 when there is fog
    fog_color: vec4<f32>,
    fog_range: vec4<f32>,
};

struct Surface {
    // rgb tint, alpha
    tint: vec4<f32>,
    // rgb fog colour, w: 1 for water
    water: vec4<f32>,
    // x: alpha test reference (0: none), y: light (0 none, 1 light map, 2 vertex),
    // z: 1 when the second texture blends in, w: how fog covers it (0 towards its colour,
    // 1 towards nothing for what adds its light, 2 not at all)
    options: vec4<f32>,
    // the base texture's transform, a row for u and one for v; the w of the first is the
    // layer of the frame that shows, of the second 1 when it modulates what is behind
    transform_u: vec4<f32>,
    transform_v: vec4<f32>,
    // the detail texture: x how much finer, y how much of it,
    // z how it mixes in (0 doubling or darkening, 1 adding), w 1 when there is one
    detail: vec4<f32>,
    // the bump map: x how it is taken (0 not at all, 1 directions, 2 the light along each of
    // the bumped light maps' directions, `$ssbump`), y 1 when a second one blends in
    bump: vec4<f32>,
    // the bump map's transform, a row for u and one for v
    bump_u: vec4<f32>,
    bump_v: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> surface: Surface;
@group(1) @binding(1) var base_texture: texture_2d_array<f32>;
@group(1) @binding(2) var blend_texture: texture_2d_array<f32>;
@group(1) @binding(3) var texture_sampler: sampler;
@group(1) @binding(4) var detail_texture: texture_2d_array<f32>;
@group(1) @binding(5) var bump_texture: texture_2d_array<f32>;
@group(1) @binding(6) var bump2_texture: texture_2d_array<f32>;
// the flat light map, then - when some face is bump mapped - the light along each direction
// of BUMP_BASIS
@group(2) @binding(0) var lightmap: texture_2d_array<f32>;
@group(2) @binding(1) var lightmap_sampler: sampler;
// the bones of the posed models of the frame: three rows of each one's matrix, in the space of
// its model
@group(3) @binding(0) var<storage, read> bones: array<vec4<f32>>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) lightmap_uv: vec2<f32>,
    @location(3) alpha: f32,
    @location(4) light: vec3<f32>,
    @location(5) normal: vec3<f32>,
    // a model's: along the texture's u, and w the side of its v (0 for none)
    @location(8) tangent: vec4<f32>,
};

// the bones that move a vertex of a posed model, and how much each
struct Skin {
    @location(6) bones: vec4<u32>,
    @location(7) weights: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) lightmap_uv: vec2<f32>,
    @location(2) alpha: f32,
    @location(3) light: vec3<f32>,
    @location(4) position: vec3<f32>,
    @location(5) normal: vec3<f32>,
    // the coordinates of the second texture, which the transform does not move
    @location(6) blend_uv: vec2<f32>,
    @location(7) bump_uv: vec2<f32>,
    @location(8) tangent: vec4<f32>,
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

// the directions of the bumped light maps, in the space of a face's texture, as vrad bakes them
const BUMP_BASIS = array<vec3<f32>, 3>(
    vec3<f32>(0.81649658, 0.0, 0.57735027),
    vec3<f32>(-0.40824829, 0.70710678, 0.57735027),
    vec3<f32>(-0.40824829, -0.70710678, 0.57735027),
);

/// The light of a bump-mapped texel `texel` of the bump map, from the bumped light maps at
/// `uv`: as LightmappedGeneric takes it, each by the square of how much the texel faces its
/// direction, the squares adding up to 1; a self-shadowed one says how much of each reaches it.
fn bumped_light(texel: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let l1 = textureSampleLevel(lightmap, lightmap_sampler, uv, 1, 0.0).rgb;
    let l2 = textureSampleLevel(lightmap, lightmap_sampler, uv, 2, 0.0).rgb;
    let l3 = textureSampleLevel(lightmap, lightmap_sampler, uv, 3, 0.0).rgb;
    if (surface.bump.x > 1.5) {
        return texel.x * l1 + texel.y * l2 + texel.z * l3;
    }
    let n = normalize(texel * 2.0 - 1.0);
    var dp = vec3<f32>(
        clamp(dot(n, BUMP_BASIS[0]), 0.0, 1.0),
        clamp(dot(n, BUMP_BASIS[1]), 0.0, 1.0),
        clamp(dot(n, BUMP_BASIS[2]), 0.0, 1.0),
    );
    dp *= dp;
    return (dp.x * l1 + dp.y * l2 + dp.z * l3) / max(dp.x + dp.y + dp.z, 1e-4);
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    return shade(input);
}

// a model moved by its bones: each vertex by the matrices of its bones, weighed
@vertex
fn vs_posed(input: VertexInput, skin: Skin) -> VertexOutput {
    var posed = input;
    if (camera.skin.y > 0.5) {
        let first = u32(camera.skin.x);
        let ids = skin.bones;
        let weights = skin.weights;
        let p = vec4<f32>(input.position, 1.0);
        let n = vec4<f32>(input.normal, 0.0);
        let t = vec4<f32>(input.tangent.xyz, 0.0);
        var position = vec3<f32>(0.0);
        var normal = vec3<f32>(0.0);
        var tangent = vec3<f32>(0.0);
        var total = 0.0;
        for (var i = 0; i < 3; i++) {
            let w = weights[i];
            if (w > 0.0) {
                let b = (first + ids[i]) * 3u;
                position += w * vec3<f32>(dot(bones[b], p), dot(bones[b + 1u], p), dot(bones[b + 2u], p));
                normal += w * vec3<f32>(dot(bones[b], n), dot(bones[b + 1u], n), dot(bones[b + 2u], n));
                tangent += w * vec3<f32>(dot(bones[b], t), dot(bones[b + 1u], t), dot(bones[b + 2u], t));
                total += w;
            }
        }
        if (total > 0.001) {
            posed.position = position / total;
            posed.normal = normal;
            posed.tangent = vec4<f32>(tangent, input.tangent.w);
        }
    }
    return shade(posed);
}

// what every vertex goes through: clip space, and the light of a model's
fn shade(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip = camera.view_projection * vec4<f32>(input.position, 1.0);
    let uv = vec3<f32>(input.uv, 1.0);
    output.uv = vec2<f32>(dot(surface.transform_u.xyz, uv), dot(surface.transform_v.xyz, uv));
    output.blend_uv = input.uv;
    output.bump_uv = vec2<f32>(dot(surface.bump_u.xyz, uv), dot(surface.bump_v.xyz, uv));
    output.lightmap_uv = input.lightmap_uv;
    output.alpha = input.alpha;
    output.light = input.light;
    output.position = input.position;
    output.normal = input.normal;
    output.tangent = input.tangent;
    if (camera.ambient[0].w > 0.5) {
        output.light = model_light(input.normal / max(length(input.normal), 1e-6));
    }
    return output;
}

// the light of a model's surface facing `n`: the cube, each side by the square of the normal
// along it, and the lights that face it
fn model_light(n: vec3<f32>) -> vec3<f32> {
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
    return light;
}

// which way a model's bump-mapped texel faces: the bump map's direction in the space of the
// tangent, its bitangent (on the side w says) and the normal
fn bumped_normal(texel: vec3<f32>, normal: vec3<f32>, tangent: vec4<f32>) -> vec3<f32> {
    let n = normalize(normal);
    let t = normalize(tangent.xyz - n * dot(n, tangent.xyz));
    let b = cross(n, t) * select(1.0, -1.0, tangent.w < 0.0);
    let d = normalize(texel * 2.0 - 1.0);
    return normalize(t * d.x + b * d.y + n * d.z);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // sampled before any branch: texture lookups need every pixel of a quad to take them
    var albedo = textureSample(base_texture, texture_sampler, input.uv, i32(surface.transform_u.w));
    let has_detail = surface.detail.w > 0.5;
    let blended = textureSample(blend_texture, texture_sampler, input.blend_uv, 0);
    let detailed = textureSample(detail_texture, texture_sampler, input.blend_uv * surface.detail.x, 0);
    let mapped = textureSample(lightmap, lightmap_sampler, input.lightmap_uv, 0).rgb;
    var bump = textureSample(bump_texture, texture_sampler, input.bump_uv, 0).rgb;
    let bump2 = textureSample(bump2_texture, texture_sampler, input.bump_uv, 0).rgb;
    if (surface.bump.y > 0.5) {
        bump = mix(bump, bump2, clamp(input.alpha, 0.0, 1.0));
    }
    if (surface.options.z > 0.5) {
        albedo = mix(albedo, blended, clamp(input.alpha, 0.0, 1.0));
    }
    if (has_detail) {
        if (surface.detail.z > 0.5 && surface.detail.z < 1.5) {
            albedo = vec4<f32>(albedo.rgb + detailed.rgb * surface.detail.y, albedo.a);
        } else if (surface.detail.z < 0.5) {
            // as the game mixes it: grey (0.5 as stored) changes nothing
            let stored = pow(max(detailed.rgb, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
            albedo = vec4<f32>(albedo.rgb * mix(vec3<f32>(1.0), 2.0 * stored, surface.detail.y), albedo.a);
        }
    }
    if (surface.options.x > 0.0 && albedo.a < surface.options.x) {
        discard;
    }
    if (surface.water.w > 0.5) {
        return vec4<f32>(fogged(surface.water.rgb, input.position), 0.75);
    }
    if (surface.transform_v.w > 0.5) {
        // the game modulates in gamma space, where grey (0.5) changes nothing: half of what
        // multiplies the linear colour behind, the blend doubling it; transparent is grey, and
        // so is what fog covers. The target keeps what the shader gives to 1, so the light
        // parts lighten what is behind twice at most, where the game goes further.
        let shown = albedo.a * surface.tint.a * (1.0 - fog_amount(input.position));
        let gamma = mix(vec3<f32>(0.5), pow(albedo.rgb * surface.tint.rgb, vec3<f32>(1.0 / 2.2)), shown);
        return vec4<f32>(0.5 * pow(2.0 * gamma, vec3<f32>(2.2)), 1.0);
    }
    var light = vec3<f32>(1.0, 1.0, 1.0);
    if (surface.options.y > 1.5) {
        light = input.light;
        let tangent_length = length(input.tangent.xyz);
        if (camera.ambient[0].w > 0.5 && surface.bump.x > 0.5 && tangent_length > 0.5 && length(input.normal) > 0.5) {
            light = model_light(bumped_normal(bump, input.normal, input.tangent));
        }
    } else if (surface.options.y > 0.5) {
        light = mapped;
        if (surface.bump.x > 0.5 && textureNumLayers(lightmap) >= 4u) {
            light = bumped_light(bump, input.lightmap_uv);
        }
    }
    if (surface.options.y > 0.5) {
        let n = input.normal / max(length(input.normal), 1e-6);
        light += moment_light(input.position, n);
    }
    return vec4<f32>(fogged(albedo.rgb * surface.tint.rgb * light, input.position), albedo.a * surface.tint.a);
}

// fog, linear with the distance from the eye: towards its colour, or for what adds its light
// towards nothing; `$nofog` surfaces it leaves as they are
fn fogged(color: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    let amount = fog_amount(position);
    if (surface.options.w > 0.5) {
        return color * (1.0 - amount);
    }
    return mix(color, camera.fog_color.rgb, amount);
}

// how much fog covers what is at `position`: none without fog or for `$nofog` surfaces
fn fog_amount(position: vec3<f32>) -> f32 {
    if (camera.fog_range.z < 0.5 || surface.options.w > 1.5) {
        return 0.0;
    }
    let d = length(position - camera.eye.xyz) * camera.eye.w;
    return clamp((d - camera.fog_range.x) * camera.fog_range.y, 0.0, 1.0) * camera.fog_color.w;
}
