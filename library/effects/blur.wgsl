/* EFFECTS_METADATA:
{
  "type": "blur",
  "params": [
    { "name": "radius", "type": "float", "default": 0.0 }
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
    radius: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    var color = textureLoad(input_tex, coords, 0);

    if (params.radius > 0.0) {
        let r_limit = i32(clamp(params.radius, 0.0, 15.0));
        var sum = vec4<f32>(0.0);
        var weight = 0.0;
        for (var dy = -r_limit; dy <= r_limit; dy = dy + 1) {
            for (var dx = -r_limit; dx <= r_limit; dx = dx + 1) {
                let sample_coords = coords + vec2<i32>(dx, dy);
                let clamped_coords = clamp(sample_coords, vec2<i32>(0), vec2<i32>(i32(engine.width) - 1, i32(engine.height) - 1));
                let c = textureLoad(input_tex, clamped_coords, 0);
                let dist_sq = f32(dx * dx + dy * dy);
                let w = exp(-dist_sq / max(2.0 * params.radius * params.radius, 0.1));
                sum = sum + c * w;
                weight = weight + w;
            }
        }
        color = sum / max(weight, 0.0001);
    }
    textureStore(output_tex, coords, color);
}
