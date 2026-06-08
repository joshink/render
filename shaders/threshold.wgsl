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
    invert: f32, // 0.0 = Normal, 1.0 = Inverted
    threshold: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let center_color = textureLoad(input_tex, coords, 0);
    
    let luma = dot(center_color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    
    var val = 0.0;
    if (luma >= params.threshold) {
        val = 1.0;
    }
    
    let final_val = mix(val, 1.0 - val, params.invert);
    
    textureStore(output_tex, coords, vec4<f32>(vec3<f32>(final_val), center_color.a));
}
