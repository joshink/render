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
    centerX: f32,
    centerY: f32,
    direction: f32, // 0.0 = Radial, 1.0 = Horizontal, 2.0 = Vertical
    intensity: f32,
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
    
    var offsetDir: vec2<f32>;
    
    if (params.direction < 0.5) {
        // Radial mode
        let center = vec2<f32>(params.centerX, params.centerY);
        let toPixel = uv - center;
        let dist = length(toPixel);
        offsetDir = toPixel / (dist + 0.0001) * dist;
    } else if (params.direction < 1.5) {
        // Horizontal/angled mode
        let rad = params.angle * 3.14159265 / 180.0;
        offsetDir = vec2<f32>(cos(rad), sin(rad));
    } else {
        // Vertical mode
        offsetDir = vec2<f32>(0.0, 1.0);
    }
    
    let scaleX = params.intensity / f32(engine.width);
    let scaleY = params.intensity / f32(engine.height);
    let offset = offsetDir * vec2<f32>(scaleX, scaleY);
    
    let sampleR = sample_color(uv + offset);
    let sampleG = sample_color(uv);
    let sampleB = sample_color(uv - offset);
    
    textureStore(output_tex, coords, vec4<f32>(sampleR.r, sampleG.g, sampleB.b, sampleG.a));
}
