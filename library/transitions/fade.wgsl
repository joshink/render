/* TRANSITION_METADATA:
{
  "type": "fade",
  "params": []
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

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    let color_from = textureLoad(tex_from, coords, 0);
    let color_to = textureLoad(tex_to, coords, 0);
    let blended_color = mix(color_from, color_to, engine.progress);
    textureStore(output_tex, coords, blended_color);
}
