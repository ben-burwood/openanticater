// VK-01 viewer shader: simple two-sided Blinn-Phong for the metal parts, plus
// an emissive path for the glowing LED seam and its additive halo.

struct Globals {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    light_dir: vec4<f32>,
};

struct Obj {
    model: mat4x4<f32>,
    nmat: mat4x4<f32>,
    color: vec4<f32>,     // rgb = base colour, a = alpha / additive strength
    emissive: vec4<f32>,  // rgb = glow colour * intensity
    params: vec4<f32>,    // x = emissive mix (0 lit .. 1 emissive), y = specular
};

@group(0) @binding(0) var<uniform> G: Globals;
@group(1) @binding(0) var<uniform> O: Obj;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>) -> VsOut {
    var out: VsOut;
    let wp = O.model * vec4<f32>(pos, 1.0);
    out.world = wp.xyz;
    out.normal = (O.nmat * vec4<f32>(nrm, 0.0)).xyz;
    out.clip = G.view_proj * wp;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    var N = normalize(in.normal);
    let V = normalize(G.cam_pos.xyz - in.world);
    // Two-sided shading: flip the normal toward the viewer.
    if (dot(N, V) < 0.0) {
        N = -N;
    }
    let L = normalize(-G.light_dir.xyz);
    let H = normalize(L + V);

    let diff = max(dot(N, L), 0.0);
    let spec = pow(max(dot(N, H), 0.0), 48.0) * O.params.y;
    let ambient = 0.20;
    let fres = pow(1.0 - max(dot(N, V), 0.0), 3.0) * 0.30;

    let lit = O.color.rgb * (ambient + diff * 0.9) + vec3<f32>(spec) + O.color.rgb * fres;
    let col = mix(lit, O.emissive.rgb, O.params.x);
    return vec4<f32>(col, O.color.a);
}
