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
    strength: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn sample_color(uv: vec2<f32>) -> vec4<f32> {
    let coords = vec2<i32>(
        i32(clamp(uv.x * f32(engine.width), 0.0, f32(engine.width) - 1.0)),
        i32(clamp(uv.y * f32(engine.height), 0.0, f32(engine.height) - 1.0))
    );
    return textureLoad(input_tex, coords, 0);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let rad = params.angle * 3.14159265 / 180.0;
    let dir = vec2<f32>(cos(rad), sin(rad));
    
    let step_scale_x = params.strength / f32(engine.width);
    let step_scale_y = params.strength / f32(engine.height);
    let step_offset = dir * vec2<f32>(step_scale_x, step_scale_y);
    
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    
    let samples = 9;
    let half_samples = samples / 2;
    
    for (var i = -half_samples; i <= half_samples; i = i + 1) {
        let offset = f32(i) * step_offset;
        let c = sample_color(uv + offset);
        
        // Linear weight distribution
        let w = 1.0 - abs(f32(i)) / f32(half_samples + 1);
        sum = sum + c * w;
        weight = weight + w;
    }
    
    let final_color = sum / max(weight, 0.0001);
    textureStore(output_tex, coords, final_color);
}
