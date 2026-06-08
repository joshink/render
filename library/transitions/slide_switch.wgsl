/* TRANSITION_METADATA:
{
  "type": "slide_switch",
  "params": [
    { "name": "direction", "type": "float", "default": 0.0 },
    { "name": "flicker_intensity", "type": "float", "default": 0.15 },
    { "name": "gap_size", "type": "float", "default": 0.1 },
    { "name": "z_padding", "type": "float", "default": 0.0 }
  ]
}
*/

@group(0) @binding(0) var tex_from: texture_2d<f32>;
@group(0) @binding(1) var tex_to: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct TransitionEngineParams {
    progress: f32,
    duration: f32,
    width: u32,
    height: u32,
}
@group(0) @binding(3) var<uniform> engine: TransitionEngineParams;

struct CustomParams {
    direction: f32,          // 0.0 for horizontal, 1.0 for vertical
    flicker_intensity: f32,  // Default 0.15
    gap_size: f32,           // Default 0.1
    z_padding: f32,
}
@group(0) @binding(4) var<uniform> params: CustomParams;

fn hash(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

fn noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    
    let a = hash(i + vec2<f32>(0.0, 0.0));
    let b = hash(i + vec2<f32>(1.0, 0.0));
    let c = hash(i + vec2<f32>(0.0, 1.0));
    let d = hash(i + vec2<f32>(1.0, 1.0));
    
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Elastic ease-out for mechanical snap-in settle bounce
fn elastic_out(t: f32) -> f32 {
    if (t <= 0.0) { return 0.0; }
    if (t >= 1.0) { return 1.0; }
    let p = 0.35;
    return pow(2.0, -10.0 * t) * sin((t - p / 4.0) * (2.0 * 3.14159265) / p) + 1.0;
}

// Directional motion blur helper
fn sample_motion_blur(tex: texture_2d<f32>, uv: vec2<f32>, blur_dir: vec2<f32>, width: f32, height: f32) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    
    // Sample 5 points along the blur vector
    for (var i = -2.0; i <= 2.0; i += 1.0) {
        let sample_uv = uv + blur_dir * (i / 2.0);
        if (sample_uv.x >= 0.0 && sample_uv.x <= 1.0 && sample_uv.y >= 0.0 && sample_uv.y <= 1.0) {
            let coords = clamp(vec2<i32>(sample_uv * vec2<f32>(width, height)), vec2<i32>(0), vec2<i32>(i32(width) - 1, i32(height) - 1));
            sum += textureLoad(tex, coords, 0);
            weight += 1.0;
        }
    }
    
    if (weight > 0.0) {
        return sum / weight;
    }
    return vec4<f32>(0.0);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    
    let uv = vec2<f32>(f32(id.x), f32(id.y)) / vec2<f32>(f32(engine.width), f32(engine.height));
    let w_f32 = f32(engine.width);
    let h_f32 = f32(engine.height);
    
    // Animate displacement using mechanical elastic settle bounce curve
    let shift_progress = elastic_out(engine.progress);
    let total_travel = 1.0 + params.gap_size;
    let shift = shift_progress * total_travel;
    
    // Calculate motion blur vector (peaks at progress = 0.5 where speed is highest)
    let speed_factor = sin(engine.progress * 3.14159265);
    let blur_length = 0.05 * speed_factor;
    
    var final_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    var dist_to_gap = 0.0;
    
    if (params.direction > 0.5) {
        // Vertical slide switch
        let uv_from = uv + vec2<f32>(0.0, shift);
        let uv_to = uv - vec2<f32>(0.0, total_travel - shift);
        
        let blur_dir = vec2<f32>(0.0, blur_length);
        
        if (uv_from.y >= 0.0 && uv_from.y <= 1.0) {
            final_color = sample_motion_blur(tex_from, uv_from, blur_dir, w_f32, h_f32);
        } else if (uv_to.y >= 0.0 && uv_to.y <= 1.0) {
            final_color = sample_motion_blur(tex_to, uv_to, blur_dir, w_f32, h_f32);
        } else {
            // Gap region
            final_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
        
        dist_to_gap = abs(uv.y - (1.0 - shift + params.gap_size / 2.0));
    } else {
        // Horizontal slide switch
        let uv_from = uv + vec2<f32>(shift, 0.0);
        let uv_to = uv - vec2<f32>(total_travel - shift, 0.0);
        
        let blur_dir = vec2<f32>(blur_length, 0.0);
        
        if (uv_from.x >= 0.0 && uv_from.x <= 1.0) {
            final_color = sample_motion_blur(tex_from, uv_from, blur_dir, w_f32, h_f32);
        } else if (uv_to.x >= 0.0 && uv_to.x <= 1.0) {
            final_color = sample_motion_blur(tex_to, uv_to, blur_dir, w_f32, h_f32);
        } else {
            // Gap region
            final_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
        
        dist_to_gap = abs(uv.x - (1.0 - shift + params.gap_size / 2.0));
    }
    
    // Projector lamp flicker
    let flicker = 1.0 - params.flicker_intensity * noise(vec2<f32>(engine.progress * 25.0, 0.0));
    var rgb = final_color.rgb * flicker;
    
    // warm light leak near the slide gap edge
    let leak_factor = smoothstep(params.gap_size * 1.5, 0.0, dist_to_gap);
    let leak_color = vec3<f32>(1.0, 0.35, 0.08) * leak_factor * 0.75 * speed_factor;
    rgb = clamp(rgb + leak_color, vec3<f32>(0.0), vec3<f32>(1.0));
    
    textureStore(output_tex, coords, vec4<f32>(rgb, final_color.a));
}
