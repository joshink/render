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
    gamma: f32,
    levels: f32,
    mode: f32, // 0.0 = RGB, 1.0 = Luma
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn quantize(val: f32, steps: f32, gamma: f32) -> f32 {
    let encoded = pow(clamp(val, 0.0, 1.0), gamma);
    let quantized = floor(encoded * steps + 0.5) / steps;
    return pow(quantized, 1.0 / max(gamma, 0.0001));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let src_color = textureLoad(input_tex, coords, 0);
    
    let steps = max(params.levels - 1.0, 1.0);
    let gamma = max(params.gamma, 0.0001);
    
    var final_color: vec3<f32>;
    
    if (params.mode > 0.5) {
        // Luma mode
        let luma = dot(src_color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        let quantized_luma = quantize(luma, steps, gamma);
        let scale = quantized_luma / max(luma, 0.001);
        final_color = clamp(src_color.rgb * scale, vec3<f32>(0.0), vec3<f32>(1.0));
    } else {
        // RGB mode
        final_color = vec3<f32>(
            quantize(src_color.r, steps, gamma),
            quantize(src_color.g, steps, gamma),
            quantize(src_color.b, steps, gamma)
        );
    }
    
    textureStore(output_tex, coords, vec4<f32>(final_color, src_color.a));
}
