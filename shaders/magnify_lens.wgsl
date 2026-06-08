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
    centerX: f32,
    centerY: f32,
    magnification: f32,
    radius: f32,
    refraction: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let center = vec2<f32>(params.centerX, params.centerY);
    let to_center = uv - center;
    let dist = length(to_center);
    
    var sample_uv = uv;
    
    if (dist < params.radius) {
        let percent = dist / max(params.radius, 0.001);
        // bulge mapping
        let bulge = mix(1.0, percent * percent, params.refraction);
        sample_uv = center + to_center * percent / max(params.magnification * bulge, 0.001);
    }
    
    let sample_coords = vec2<i32>(
        i32(clamp(sample_uv.x * f32(engine.width - 1u), 0.0, f32(engine.width - 1u))),
        i32(clamp(sample_uv.y * f32(engine.height - 1u), 0.0, f32(engine.height - 1u)))
    );
    
    let color = textureLoad(input_tex, sample_coords, 0);
    textureStore(output_tex, coords, color);
}
