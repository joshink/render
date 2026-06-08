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
    colorMode: f32, // 0 = CMYK, 1 = Monochrome
    dotSize: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn get_halftone_dot(uv: vec2<f32>, rot_angle: f32, cellSize: f32, channel_value: f32) -> f32 {
    let rad = rot_angle * 3.14159265 / 180.0;
    let cos_a = cos(rad);
    let sin_a = sin(rad);
    
    let scale = vec2<f32>(f32(engine.width), f32(engine.height));
    let rot_uv = vec2<f32>(
        uv.x * cos_a - uv.y * sin_a,
        uv.x * sin_a + uv.y * cos_a
    ) * scale / cellSize;
    
    let cell_coord = fract(rot_uv) - 0.5;
    let dist = length(cell_coord);
    
    // Dot radius scales with channel value (inverse, larger dot for darker values or vice versa)
    let radius = 0.7 * channel_value;
    return smoothstep(radius, radius - 0.1, dist);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let src_color = textureLoad(input_tex, coords, 0);
    let cell_size = max(params.dotSize, 2.0);
    
    var final_color: vec3<f32>;
    
    if (params.colorMode > 0.5) {
        // Monochrome halftone
        let luma = dot(src_color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        let dot_val = get_halftone_dot(uv, params.angle, cell_size, luma);
        final_color = vec3<f32>(dot_val);
    } else {
        // CMYK halftone
        // Convert RGB to CMY
        let c = 1.0 - src_color.r;
        let m = 1.0 - src_color.g;
        let y = 1.0 - src_color.b;
        let k = min(c, min(m, y));
        
        let c_pure = clamp((c - k) / max(1.0 - k, 0.001), 0.0, 1.0);
        let m_pure = clamp((m - k) / max(1.0 - k, 0.001), 0.0, 1.0);
        let y_pure = clamp((y - k) / max(1.0 - k, 0.001), 0.0, 1.0);
        
        let dot_c = get_halftone_dot(uv, params.angle + 15.0, cell_size, c_pure);
        let dot_m = get_halftone_dot(uv, params.angle + 75.0, cell_size, m_pure);
        let dot_y = get_halftone_dot(uv, params.angle + 0.0, cell_size, y_pure);
        let dot_k = get_halftone_dot(uv, params.angle + 45.0, cell_size, k);
        
        // Blend CMYK dots
        let r_out = (1.0 - dot_c) * (1.0 - dot_k);
        let g_out = (1.0 - dot_m) * (1.0 - dot_k);
        let b_out = (1.0 - dot_y) * (1.0 - dot_k);
        
        final_color = vec3<f32>(r_out, g_out, b_out);
    }
    
    textureStore(output_tex, coords, vec4<f32>(final_color, src_color.a));
}
