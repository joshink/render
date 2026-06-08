/* EFFECTS_METADATA:
{
  "type": "hue_rotate",
  "params": [
    { "name": "angle", "type": "float", "default": 0.0 }
  ]
}
*/

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
    angle: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let min_val = min(c.r, min(c.g, c.b));
    let max_val = max(c.r, max(c.g, c.b));
    let delta = max_val - min_val;
    
    var h = 0.0;
    var s = 0.0;
    let l = (max_val + min_val) * 0.5;
    
    if (delta > 0.0) {
        if (l < 0.5) {
            s = delta / (max_val + min_val);
        } else {
            s = delta / (2.0 - max_val - min_val);
        }
        
        if (c.r >= max_val) {
            h = (c.g - c.b) / delta + select(6.0, 0.0, c.g >= c.b);
        } else if (c.g >= max_val) {
            h = (c.b - c.r) / delta + 2.0;
        } else {
            h = (c.r - c.g) / delta + 4.0;
        }
        h = h / 6.0;
    }
    return vec3<f32>(h, s, l);
}

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) { t = t + 1.0; }
    if (t > 1.0) { t = t - 1.0; }
    if (t < 1.0 / 6.0) { return p + (q - p) * 6.0 * t; }
    if (t < 1.0 / 2.0) { return q; }
    if (t < 2.0 / 3.0) { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
    return p;
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    if (hsl.y == 0.0) {
        return vec3<f32>(hsl.z);
    }
    let q = select(hsl.z + hsl.y - hsl.z * hsl.y, hsl.z * (1.0 + hsl.y), hsl.z < 0.5);
    let p = 2.0 * hsl.z - q;
    let r = hue_to_rgb(p, q, hsl.x + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, hsl.x);
    let b = hue_to_rgb(p, q, hsl.x - 1.0 / 3.0);
    return vec3<f32>(r, g, b);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    var color = textureLoad(input_tex, coords, 0);
    if (params.angle != 0.0) {
        var hsl = rgb_to_hsl(color.rgb);
        hsl.x = fract(hsl.x + params.angle / 360.0);
        color = vec4<f32>(hsl_to_rgb(hsl), color.a);
    }
    textureStore(output_tex, coords, color);
}
