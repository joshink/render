/* EFFECTS_METADATA:
{
  "type": "depth_blur",
  "params": [
    { "name": "far_blur", "type": "float", "default": 0.0 },
    { "name": "focus_radius", "type": "float", "default": 0.2 },
    { "name": "focus_x", "type": "float", "default": 0.5 },
    { "name": "focus_y", "type": "float", "default": 0.5 },
    { "name": "near_blur", "type": "float", "default": 0.0 },
    { "name": "depth_map", "type": "depth_map", "default": 0.0, "target": "use_map" }
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
    far_blur: f32,
    focus_radius: f32,
    focus_x: f32,
    focus_y: f32,
    near_blur: f32,
    use_map: i32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@group(0) @binding(4) var depth_tex: texture_2d<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    var color = textureLoad(input_tex, coords, 0);

    if (params.near_blur > 0.0 || params.far_blur > 0.0) {
        var depth = 0.0;
        if (params.use_map == 1) {
            let depth_color = textureLoad(depth_tex, coords, 0);
            depth = depth_color.r;
        } else {
            let dist_to_focus = distance(uv, vec2<f32>(params.focus_x, params.focus_y));
            depth = smoothstep(0.0, params.focus_radius, dist_to_focus);
        }
        
        let blur_r = mix(params.near_blur, params.far_blur, depth);
        if (blur_r > 0.1) {
            let r_limit = i32(clamp(blur_r, 0.0, 15.0));
            var sum = vec4<f32>(0.0);
            var weight = 0.0;
            for (var dy = -r_limit; dy <= r_limit; dy = dy + 1) {
                for (var dx = -r_limit; dx <= r_limit; dx = dx + 1) {
                    let sample_coords = coords + vec2<i32>(dx, dy);
                    let clamped_coords = clamp(sample_coords, vec2<i32>(0), vec2<i32>(i32(engine.width) - 1, i32(engine.height) - 1));
                    let c = textureLoad(input_tex, clamped_coords, 0);
                    let dist_sq = f32(dx * dx + dy * dy);
                    let w = exp(-dist_sq / max(2.0 * blur_r * blur_r, 0.1));
                    sum = sum + c * w;
                    weight = weight + w;
                }
            }
            color = sum / max(weight, 0.0001);
        }
    }
    textureStore(output_tex, coords, color);
}
