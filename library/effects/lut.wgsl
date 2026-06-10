/* EFFECTS_METADATA:
{
  "type": "lut",
  "params": [
    { "name": "amount", "type": "float", "default": 1.0 },
    { "name": "lut", "type": "lut", "default": 0.0, "target": "has_lut" }
  ]
}
*/

// Applies a color look-up table loaded from a `.cube` or HALD file. The LUT is
// uploaded as a horizontal "strip" atlas (binding 7): `size` blue-slices laid
// left-to-right, each `size × size` pixels (red across X, green down Y). We
// recover `size` from the atlas height and trilinearly interpolate by hand
// (textureLoad — no sampler), so the grid resolution is fully data-driven.

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct EngineParams {
    time: f32,
    clip_time: f32,
    progress: f32,
    width: u32,
    height: u32,
}
@group(0) @binding(2) var<uniform> engine: EngineParams;

struct CustomParams {
    amount: f32,
    has_lut: i32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@group(0) @binding(7) var lut_tex: texture_2d<f32>;

fn lut_texel(slice: i32, ri: i32, gi: i32, size: i32) -> vec3<f32> {
    return textureLoad(lut_tex, vec2<i32>(slice * size + ri, gi), 0).rgb;
}

// Bilinear interpolation of the red/green plane within one blue slice.
fn lut_slice(slice: i32, r0: i32, r1: i32, fr: f32, g0: i32, g1: i32, fg: f32, size: i32) -> vec3<f32> {
    let c00 = lut_texel(slice, r0, g0, size);
    let c10 = lut_texel(slice, r1, g0, size);
    let c01 = lut_texel(slice, r0, g1, size);
    let c11 = lut_texel(slice, r1, g1, size);
    let c0 = mix(c00, c10, fr);
    let c1 = mix(c01, c11, fr);
    return mix(c0, c1, fg);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    let color = textureLoad(input_tex, coords, 0);

    let size = i32(textureDimensions(lut_tex).y);
    if (params.has_lut == 0 || size < 2) {
        textureStore(output_tex, coords, color);
        return;
    }

    let rgb = clamp(color.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let maxi = f32(size - 1);
    let rf = rgb.r * maxi;
    let gf = rgb.g * maxi;
    let bf = rgb.b * maxi;

    let r0 = i32(floor(rf)); let r1 = min(r0 + 1, size - 1); let fr = rf - floor(rf);
    let g0 = i32(floor(gf)); let g1 = min(g0 + 1, size - 1); let fg = gf - floor(gf);
    let b0 = i32(floor(bf)); let b1 = min(b0 + 1, size - 1); let fb = bf - floor(bf);

    let lo = lut_slice(b0, r0, r1, fr, g0, g1, fg, size);
    let hi = lut_slice(b1, r0, r1, fr, g0, g1, fg, size);
    let graded = mix(lo, hi, fb);

    let out = mix(color.rgb, graded, clamp(params.amount, 0.0, 1.0));
    textureStore(output_tex, coords, vec4<f32>(out, color.a));
}
