/* TRANSITION_METADATA:
{
  "type": "focus_in",
  "params": [
    { "name": "max_blur", "type": "float", "default": 20.0 }
  ]
}
*/

@group(0) @binding(0) var tex_from: texture_2d<f32>;
@group(0) @binding(1) var tex_to: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct TransitionEngineParams {
    progress: f32,
    duration: f32,
    width: u32,
    height: u32,
}
@group(0) @binding(3) var<uniform> engine: TransitionEngineParams;

struct CustomParams {
    max_blur: f32,
}
@group(0) @binding(4) var<uniform> params: CustomParams;

fn get_blurred_color(tex: texture_2d<f32>, uv: vec2<f32>, radius: f32, width: f32, height: f32) -> vec4<f32> {
    let base_coords = uv * vec2<f32>(width, height);
    if (radius <= 0.5) {
        let clamped_coords = clamp(vec2<i32>(base_coords), vec2<i32>(0), vec2<i32>(i32(width) - 1, i32(height) - 1));
        return textureLoad(tex, clamped_coords, 0);
    }
    
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    
    // Efficient 5x5 box-blur sample pattern
    let step = radius / 2.0;
    for (var dy = -2.0; dy <= 2.0; dy += 1.0) {
        for (var dx = -2.0; dx <= 2.0; dx += 1.0) {
            let offset = vec2<f32>(dx, dy) * step;
            let sample_coords = clamp(base_coords + offset, vec2<f32>(0.0), vec2<f32>(width - 1.0, height - 1.0));
            sum += textureLoad(tex, vec2<i32>(sample_coords), 0);
            weight += 1.0;
        }
    }
    return sum / weight;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    
    let uv = vec2<f32>(f32(id.x), f32(id.y)) / vec2<f32>(f32(engine.width), f32(engine.height));
    let center = vec2<f32>(0.5, 0.5);
    
    let w_f32 = f32(engine.width);
    let h_f32 = f32(engine.height);
    
    // Focus breathing zoom scale factors
    let scale_from = 1.0 + 0.04 * engine.progress;
    let scale_to = 1.0 + 0.04 * (1.0 - engine.progress);
    
    let uv_from = center + (uv - center) / scale_from;
    let uv_to = center + (uv - center) / scale_to;
    
    // Radius goes from 0 to max_blur for outgoing clip, and max_blur to 0 for incoming clip
    let radius_from = params.max_blur * engine.progress;
    let radius_to = params.max_blur * (1.0 - engine.progress);
    
    let color_from = get_blurred_color(tex_from, uv_from, radius_from, w_f32, h_f32);
    let color_to = get_blurred_color(tex_to, uv_to, radius_to, w_f32, h_f32);
    
    // Blended crossfade of the blurred/zoomed textures
    let blended_color = mix(color_from, color_to, engine.progress);
    
    textureStore(output_tex, coords, blended_color);
}
