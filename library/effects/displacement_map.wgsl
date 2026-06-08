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
    scale: f32,
    speed: f32,
    strength: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn hash2D(p: vec2<f32>) -> f32 {
    let p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    let p3_added = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3_added.x + p3_added.y) * p3_added.z);
}

fn noise2D(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    
    let a = hash2D(i + vec2<f32>(0.0, 0.0));
    let b = hash2D(i + vec2<f32>(1.0, 0.0));
    let c = hash2D(i + vec2<f32>(0.0, 1.0));
    let d = hash2D(i + vec2<f32>(1.0, 1.0));
    
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let t = engine.clip_time * params.speed;
    let n_scale = max(params.scale, 0.1);
    
    let n1 = noise2D(uv * n_scale + vec2<f32>(t, t * 0.5));
    let n2 = noise2D(uv * n_scale - vec2<f32>(t * 0.5, t));
    
    let displace_offset = vec2<f32>(n1 * 2.0 - 1.0, n2 * 2.0 - 1.0) * params.strength;
    let sample_uv = clamp(uv + displace_offset, vec2<f32>(0.0), vec2<f32>(1.0));
    
    let sample_coords = vec2<i32>(
        i32(sample_uv.x * f32(engine.width - 1u)),
        i32(sample_uv.y * f32(engine.height - 1u))
    );
    
    let color = textureLoad(input_tex, sample_coords, 0);
    textureStore(output_tex, coords, color);
}
