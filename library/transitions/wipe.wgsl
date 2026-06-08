/* TRANSITION_METADATA:
{
  "type": "wipe",
  "params": [
    { "name": "direction", "type": "float", "default": 0.0 }
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
    direction: f32, // 0.0 for horizontal, 1.0 for vertical
}
@group(0) @binding(4) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    let color_from = textureLoad(tex_from, coords, 0);
    let color_to = textureLoad(tex_to, coords, 0);
    
    let uv = vec2<f32>(f32(id.x), f32(id.y)) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    var alpha = 0.0;
    if (params.direction > 0.5) {
        // Vertical wipe (bottom to top)
        if (uv.y > (1.0 - engine.progress)) {
            alpha = 1.0;
        }
    } else {
        // Horizontal wipe (left to right)
        if (uv.x < engine.progress) {
            alpha = 1.0;
        }
    }
    
    let blended_color = mix(color_from, color_to, alpha);
    textureStore(output_tex, coords, blended_color);
}
