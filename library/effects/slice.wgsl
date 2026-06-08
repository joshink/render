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
    count: f32,
    direction: f32, // 0.0 = Horizontal, 1.0 = Vertical
    offset: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    var sample_uv = uv;
    let cnt = max(params.count, 1.0);
    
    if (params.direction < 0.5) {
        // Horizontal slices (cut vertically, shift horizontally)
        let slice = floor(uv.y * cnt);
        let shift = mix(params.offset, -params.offset, slice % 2.0);
        sample_uv.x = fract(sample_uv.x + shift);
    } else {
        // Vertical slices (cut horizontally, shift vertically)
        let slice = floor(uv.x * cnt);
        let shift = mix(params.offset, -params.offset, slice % 2.0);
        sample_uv.y = fract(sample_uv.y + shift);
    }
    
    let sample_coords = vec2<i32>(
        i32(clamp(sample_uv.x * f32(engine.width - 1u), 0.0, f32(engine.width - 1u))),
        i32(clamp(sample_uv.y * f32(engine.height - 1u), 0.0, f32(engine.height - 1u)))
    );
    
    let color = textureLoad(input_tex, sample_coords, 0);
    textureStore(output_tex, coords, color);
}
