/* EFFECTS_METADATA:
{
  "type": "film_grain",
  "params": [
    { "name": "amount", "type": "float", "default": 0.0 },
    { "name": "speed", "type": "float", "default": 1.0 }
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
    amount: f32,
    speed: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn hash(p: vec2<f32>) -> f32 {
    let p3 = fract(vec3<f32>(p.xyx) * vec3<f32>(0.1031, 0.1030, 0.0973));
    let p3_added = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3_added.x + p3_added.y) * p3_added.z);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    var color = textureLoad(input_tex, coords, 0);

    if (params.amount > 0.0) {
        let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
        let grain_fps = 24.0;
        let t_grain = floor(engine.time * params.speed * grain_fps) / grain_fps;
        let noise_val = hash(uv * 1234.5 + t_grain * 987.6) * 2.0 - 1.0;
        
        let luma = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        let grain_mask = 4.0 * luma * (1.0 - luma);
        
        color = vec4<f32>(color.rgb + noise_val * params.amount * grain_mask, color.a);
    }
    textureStore(output_tex, coords, color);
}
