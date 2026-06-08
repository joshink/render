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
    bgColorB: f32,
    bgColorG: f32,
    bgColorR: f32,
    colorMode: f32,
    invert: f32,
    lineColorB: f32,
    lineColorG: f32,
    lineColorR: f32,
    strength: f32,
    threshold: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn get_luma(coords: vec2<i32>) -> f32 {
    let cl = clamp(coords, vec2<i32>(0), vec2<i32>(i32(engine.width) - 1, i32(engine.height) - 1));
    let color = textureLoad(input_tex, cl, 0);
    return dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    
    let tl = get_luma(coords + vec2<i32>(-1, -1));
    let tc = get_luma(coords + vec2<i32>(0, -1));
    let tr = get_luma(coords + vec2<i32>(1, -1));
    let ml = get_luma(coords + vec2<i32>(-1, 0));
    let mr = get_luma(coords + vec2<i32>(1, 0));
    let bl = get_luma(coords + vec2<i32>(-1, 1));
    let bc = get_luma(coords + vec2<i32>(0, 1));
    let br = get_luma(coords + vec2<i32>(1, 1));

    let gx = -tl + tr - 2.0 * ml + 2.0 * mr - bl + br;
    let gy = -tl - 2.0 * tc - tr + bl + 2.0 * bc + br;

    var edge_magnitude = clamp(sqrt(gx * gx + gy * gy) * params.strength, 0.0, 1.0);
    
    if (edge_magnitude < params.threshold) {
        edge_magnitude = 0.0;
    }

    let final_edge = mix(edge_magnitude, 1.0 - edge_magnitude, params.invert);
    
    let center_color = textureLoad(input_tex, coords, 0);
    
    var output_color: vec3<f32>;
    if (params.colorMode > 1.5) {
        // Source color mode (2.0)
        output_color = center_color.rgb * final_edge;
    } else if (params.colorMode > 0.5) {
        // Mono mode (1.0)
        let line_col = vec3<f32>(params.lineColorR, params.lineColorG, params.lineColorB);
        let bg_col = vec3<f32>(params.bgColorR, params.bgColorG, params.bgColorB);
        output_color = mix(bg_col, line_col, final_edge);
    } else {
        // Overlay mode (0.0): white edges on source
        output_color = mix(center_color.rgb, vec3<f32>(1.0), final_edge);
    }
    
    textureStore(output_tex, coords, vec4<f32>(output_color, center_color.a));
}
