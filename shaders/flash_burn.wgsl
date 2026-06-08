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
    burn_intensity: f32,
    flash_intensity: f32,
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

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    
    let uv = vec2<f32>(f32(id.x), f32(id.y)) / vec2<f32>(f32(engine.width), f32(engine.height));
    let center = vec2<f32>(0.5, 0.5);
    
    // Calculate distance to center, perturbed by noise for organic feel
    let dist_to_center = length(uv - center);
    let n = noise(uv * 10.0) * 0.12 * params.burn_intensity;
    let perturbed_dist = dist_to_center + n;
    
    // Threshold increases from 0.0 to 1.15 to ensure complete coverage of the screen (max corner distance is ~0.707)
    let threshold = engine.progress * 1.15;
    
    var final_color = vec4<f32>(0.0);
    
    if (perturbed_dist < threshold) {
        // Revealed incoming clip (tex_to)
        let color_to = textureLoad(tex_to, coords, 0);
        let diff = threshold - perturbed_dist;
        
        if (diff < 0.04) {
            // Hot cooling edge
            let hot_factor = 1.0 - (diff / 0.04);
            let glow = vec3<f32>(1.0, 0.5 * hot_factor, 0.1 * hot_factor) * hot_factor * 1.5;
            final_color = vec4<f32>(clamp(color_to.rgb + glow, vec3<f32>(0.0), vec3<f32>(1.0)), color_to.a);
        } else {
            final_color = color_to;
        }
    } else {
        // Outgoing clip (tex_from)
        let color_from = textureLoad(tex_from, coords, 0);
        let diff = perturbed_dist - threshold;
        
        if (diff < 0.08) {
            // Burning edge approaching
            let burn_factor = 1.0 - (diff / 0.08);
            let glow = vec3<f32>(1.0, 0.3 * burn_factor, 0.0) * burn_factor * 2.0;
            final_color = vec4<f32>(clamp(color_from.rgb + glow, vec3<f32>(0.0), vec3<f32>(1.0)), color_from.a);
        } else {
            final_color = color_from;
        }
    }
    
    // Flash intensity peaks in the middle (bell curve)
    let flash_curve = sin(engine.progress * 3.14159265);
    let flash_brightness = flash_curve * params.flash_intensity;
    
    let brightened_rgb = clamp(final_color.rgb + vec3<f32>(flash_brightness), vec3<f32>(0.0), vec3<f32>(1.0));
    
    textureStore(output_tex, coords, vec4<f32>(brightened_rgb, final_color.a));
}
