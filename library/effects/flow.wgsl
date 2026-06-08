/* EFFECTS_METADATA:
{
  "type": "flow",
  "params": [
    { "name": "amount", "type": "float", "default": 0.0 },
    { "name": "decay", "type": "float", "default": 0.95 },
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
    decay: f32,
    speed: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@group(0) @binding(5) var feedback_in_tex: texture_2d<f32>;
@group(0) @binding(6) var feedback_out_tex: texture_storage_2d<rgba8unorm, write>;

fn hash(p: vec2<f32>) -> f32 {
    let p3 = fract(vec3<f32>(p.xyx) * vec3<f32>(0.1031, 0.1030, 0.0973));
    let p3_added = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3_added.x + p3_added.y) * p3_added.z);
}

fn noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    
    let a = hash(i + vec2<f32>(0.0, 0.0));
    let b = hash(i + vec2<f32>(1.0, 0.0));
    let c = hash(i + vec2<f32>(0.0, 1.0));
    let d = hash(i + vec2<f32>(1.0, 1.0));
    
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn get_flow_vector(p: vec2<f32>, t: f32) -> vec2<f32> {
    let scale = 12.0;
    let n1 = noise(p * scale + vec2<f32>(t * 0.8, t * 0.5));
    let n2 = noise(p * scale + vec2<f32>(-t * 0.4, t + 50.0));
    return vec2<f32>(n1 * 2.0 - 1.0, n2 * 2.0 - 1.0);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    var color = textureLoad(input_tex, coords, 0);

    if (params.amount > 0.0) {
        let flow_dir = get_flow_vector(uv, engine.time * params.speed);
        let displace_offset = flow_dir * params.amount * f32(engine.width) * 0.02;
        let sample_coords = vec2<i32>(vec2<f32>(coords) - displace_offset);
        let clamped_sample_coords = clamp(sample_coords, vec2<i32>(0), vec2<i32>(i32(engine.width) - 1, i32(engine.height) - 1));
        
        let prev_color = textureLoad(feedback_in_tex, clamped_sample_coords, 0);
        let blend = clamp(params.decay, 0.0, 0.99);
        color = mix(color, prev_color, blend);
        
        textureStore(feedback_out_tex, coords, color);
    } else {
        textureStore(feedback_out_tex, coords, color);
    }

    textureStore(output_tex, coords, color);
}
