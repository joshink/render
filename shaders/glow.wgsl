/* EFFECTS_METADATA:
{
  "type": "glow",
  "params": [
    { "name": "intensity", "type": "float", "default": 0.0 },
    { "name": "radius", "type": "float", "default": 0.0 },
    { "name": "threshold", "type": "float", "default": 0.5 }
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
    intensity: f32,
    radius: f32,
    threshold: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    var color = textureLoad(input_tex, coords, 0);

    if (params.intensity > 0.0) {
        let r_limit = i32(clamp(params.radius, 0.0, 15.0));
        var glow_sum = vec3<f32>(0.0);
        var weight = 0.0;
        for (var dy = -r_limit; dy <= r_limit; dy = dy + 1) {
            for (var dx = -r_limit; dx <= r_limit; dx = dx + 1) {
                let sample_coords = coords + vec2<i32>(dx, dy);
                let clamped_coords = clamp(sample_coords, vec2<i32>(0), vec2<i32>(i32(engine.width) - 1, i32(engine.height) - 1));
                let c = textureLoad(input_tex, clamped_coords, 0).rgb;
                
                let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
                let threshold_factor = smoothstep(params.threshold - 0.1, params.threshold + 0.1, luma);
                let bright_color = c * threshold_factor;
                
                let dist_sq = f32(dx * dx + dy * dy);
                let w = exp(-dist_sq / max(2.0 * params.radius * params.radius, 0.1));
                glow_sum = glow_sum + bright_color * w;
                weight = weight + w;
            }
        }
        let blurred_glow = (glow_sum / max(weight, 0.0001)) * params.intensity;
        color = vec4<f32>(color.rgb + blurred_glow, color.a);
    }
    textureStore(output_tex, coords, color);
}
