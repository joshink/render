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
    amplitude: f32,
    frequency: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let offset = sin(uv.y * params.frequency + engine.clip_time * 5.0) * params.amplitude;
    var sample_x = i32(f32(coords.x) + offset * f32(engine.width));
    sample_x = clamp(sample_x, 0, i32(engine.width) - 1);
    let sample_coords = vec2<i32>(sample_x, coords.y);
    
    let color = textureLoad(input_tex, sample_coords, 0);
    textureStore(output_tex, coords, color);
}
