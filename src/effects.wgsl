// ================================================================
// effects.wgsl — Built-in post-processing effects pipeline
//
// A compute shader applied per-pixel to the rendered frame. Each
// invocation processes one pixel through a chain of image effects
// controlled by the `Params` uniform. The pipeline stages are:
//
//   1. Initial color sample
//   2. Fluid flow (advection with temporal feedback buffer)
//   3. Contrast & HSL adjustment (grayscale, saturation, hue, brightness)
//   4. Gaussian blur
//   5. Glow (thresholded additive bloom)
//   6. Depth-of-field blur
//   7. Film grain
//   8. Film flicker
//
// Workgroup size: 16×16 threads (256 pixels per workgroup).
// ================================================================

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct Params {
    grayscale_enabled: u32,
    brightness_factor: f32,
    contrast_factor: f32,
    saturation_factor: f32,
    
    hue_rotate_angle: f32,
    blur_radius: f32,
    glow_intensity: f32,
    glow_radius: f32,
    
    glow_threshold: f32,
    film_grain_amount: f32,
    film_grain_speed: f32,
    film_flicker_amount: f32,
    
    film_flicker_speed: f32,
    depth_blur_focus_x: f32,
    depth_blur_focus_y: f32,
    depth_blur_focus_radius: f32,
    
    depth_blur_near_blur: f32,
    depth_blur_far_blur: f32,
    depth_blur_use_map: u32,
    flow_amount: f32,
    
    flow_speed: f32,
    flow_decay: f32,
    time: f32,
    clip_time: f32,
    
    width: u32,
    height: u32,
    padding1: u32,
    padding2: u32,
}
@group(0) @binding(2) var<uniform> params: Params;

@group(0) @binding(3) var depth_tex: texture_2d<f32>;
@group(0) @binding(4) var feedback_in_tex: texture_2d<f32>;
@group(0) @binding(5) var feedback_out_tex: texture_storage_2d<rgba8unorm, write>;


// ============================================================
// UTILITY FUNCTIONS — Noise, flow fields, and color conversion
// ============================================================

// Fast pseudo-random hash. Input: 2D coordinate. Output: [0, 1) float.
fn hash(p: vec2<f32>) -> f32 {
    let p3 = fract(vec3<f32>(p.xyx) * vec3<f32>(0.1031, 0.1030, 0.0973));
    let p3_added = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3_added.x + p3_added.y) * p3_added.z);
}

// 2D value noise with Hermite smoothing. Returns [0, 1).
// Uses hash() at four integer lattice corners, then bilinearly
// interpolates with a smooth (3t²−2t³) curve.
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

// Generates a time-varying 2D displacement vector field for fluid flow.
// Returns [-1, 1] per component. Two noise samples at different temporal
// offsets yield independent X and Y displacement.
fn get_flow_vector(p: vec2<f32>, t: f32) -> vec2<f32> {
    let scale = 12.0;
    let n1 = noise(p * scale + vec2<f32>(t * 0.8, t * 0.5));
    let n2 = noise(p * scale + vec2<f32>(-t * 0.4, t + 50.0));
    return vec2<f32>(n1 * 2.0 - 1.0, n2 * 2.0 - 1.0);
}

// Converts linear RGB [0,1] to HSL. H is [0,1] (not degrees), S and L are [0,1].
fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let min_val = min(c.r, min(c.g, c.b));
    let max_val = max(c.r, max(c.g, c.b));
    let delta = max_val - min_val;
    
    var h = 0.0;
    var s = 0.0;
    let l = (max_val + min_val) * 0.5;
    
    if (delta > 0.0) {
        if (l < 0.5) {
            s = delta / (max_val + min_val);
        } else {
            s = delta / (2.0 - max_val - min_val);
        }
        
        if (c.r >= max_val) {
            h = (c.g - c.b) / delta + select(6.0, 0.0, c.g >= c.b);
        } else if (c.g >= max_val) {
            h = (c.b - c.r) / delta + 2.0;
        } else {
            h = (c.r - c.g) / delta + 4.0;
        }
        h = h / 6.0;
    }
    return vec3<f32>(h, s, l);
}

// Helper for HSL→RGB. Converts a single hue sector to an RGB channel value.
fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) { t = t + 1.0; }
    if (t > 1.0) { t = t - 1.0; }
    if (t < 1.0 / 6.0) { return p + (q - p) * 6.0 * t; }
    if (t < 1.0 / 2.0) { return q; }
    if (t < 2.0 / 3.0) { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
    return p;
}

// Converts HSL back to linear RGB [0,1].
fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    if (hsl.y == 0.0) {
        return vec3<f32>(hsl.z); // achromatic
    }
    let q = select(hsl.z + hsl.y - hsl.z * hsl.y, hsl.z * (1.0 + hsl.y), hsl.z < 0.5);
    let p = 2.0 * hsl.z - q;
    let r = hue_to_rgb(p, q, hsl.x + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, hsl.x);
    let b = hue_to_rgb(p, q, hsl.x - 1.0 / 3.0);
    return vec3<f32>(r, g, b);
}


// ============================================================
// MAIN COMPUTE ENTRY POINT — Per-pixel effects pipeline
// ============================================================

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.width || id.y >= params.height) {
        return;
    }

    let coords = vec2<i32>(id.xy);
    // Normalized pixel coordinates in [0, 1]
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(params.width), f32(params.height));

    // ============================================================
    // 1. INITIAL COLOR SAMPLE — Read source pixel
    // ============================================================
    var color = textureLoad(input_tex, coords, 0);

    // ============================================================
    // 2. FLUID FLOW — Advection using temporal feedback buffer
    // ============================================================
    if (params.flow_amount > 0.0) {
        let flow_dir = get_flow_vector(uv, params.time * params.flow_speed);
        let displace_offset = flow_dir * params.flow_amount * f32(params.width) * 0.02;
        let sample_coords = vec2<i32>(vec2<f32>(coords) - displace_offset);
        let clamped_sample_coords = clamp(sample_coords, vec2<i32>(0), vec2<i32>(i32(params.width) - 1, i32(params.height) - 1));
        
        let prev_color = textureLoad(feedback_in_tex, clamped_sample_coords, 0);
        let blend = clamp(params.flow_decay, 0.0, 0.99);
        color = mix(color, prev_color, blend);
        
        textureStore(feedback_out_tex, coords, color);
    } else {
        textureStore(feedback_out_tex, coords, color);
    }

    // ============================================================
    // 3. CONTRAST & HSL ADJUST — Grayscale, saturation, hue, brightness
    // ============================================================
    // Grayscale
    if (params.grayscale_enabled == 1u) {
        let gray = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        color = vec4<f32>(gray, gray, gray, color.a);
    }

    // Saturation and Hue Rotation
    if (params.saturation_factor != 1.0 || params.hue_rotate_angle != 0.0) {
        var hsl = rgb_to_hsl(color.rgb);
        hsl.x = fract(hsl.x + params.hue_rotate_angle / 360.0);
        hsl.y = clamp(hsl.y * params.saturation_factor, 0.0, 1.0);
        color = vec4<f32>(hsl_to_rgb(hsl), color.a);
    }

    // Brightness factor
    color = vec4<f32>(color.rgb * params.brightness_factor, color.a);

    // Contrast
    if (params.contrast_factor != 1.0) {
        color = vec4<f32>((color.rgb - 0.5) * params.contrast_factor + 0.5, color.a);
    }

    // ============================================================
    // 4. GAUSSIAN BLUR — Separable approximation via box kernel
    // ============================================================
    if (params.blur_radius > 0.0) {
        let r_limit = i32(clamp(params.blur_radius, 0.0, 15.0));
        var sum = vec4<f32>(0.0);
        var weight = 0.0;
        for (var dy = -r_limit; dy <= r_limit; dy = dy + 1) {
            for (var dx = -r_limit; dx <= r_limit; dx = dx + 1) {
                let sample_coords = coords + vec2<i32>(dx, dy);
                let clamped_coords = clamp(sample_coords, vec2<i32>(0), vec2<i32>(i32(params.width) - 1, i32(params.height) - 1));
                let c = textureLoad(input_tex, clamped_coords, 0);
                let dist_sq = f32(dx * dx + dy * dy);
                let w = exp(-dist_sq / max(2.0 * params.blur_radius * params.blur_radius, 0.1));
                sum = sum + c * w;
                weight = weight + w;
            }
        }
        color = sum / max(weight, 0.0001);
    }

    // ============================================================
    // 5. GLOW — Additive high-pass blurred blend (bloom)
    // ============================================================
    if (params.glow_intensity > 0.0) {
        let r_limit = i32(clamp(params.glow_radius, 0.0, 15.0));
        var glow_sum = vec3<f32>(0.0);
        var weight = 0.0;
        for (var dy = -r_limit; dy <= r_limit; dy = dy + 1) {
            for (var dx = -r_limit; dx <= r_limit; dx = dx + 1) {
                let sample_coords = coords + vec2<i32>(dx, dy);
                let clamped_coords = clamp(sample_coords, vec2<i32>(0), vec2<i32>(i32(params.width) - 1, i32(params.height) - 1));
                let c = textureLoad(input_tex, clamped_coords, 0).rgb;
                
                let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
                let threshold_factor = smoothstep(params.glow_threshold - 0.1, params.glow_threshold + 0.1, luma);
                let bright_color = c * threshold_factor;
                
                let dist_sq = f32(dx * dx + dy * dy);
                let w = exp(-dist_sq / max(2.0 * params.glow_radius * params.glow_radius, 0.1));
                glow_sum = glow_sum + bright_color * w;
                weight = weight + w;
            }
        }
        let blurred_glow = (glow_sum / max(weight, 0.0001)) * params.glow_intensity;
        color = vec4<f32>(color.rgb + blurred_glow, color.a);
    }

    // ============================================================
    // 6. DEPTH BLUR — Distance-based depth-of-field
    // ============================================================
    if (params.depth_blur_near_blur > 0.0 || params.depth_blur_far_blur > 0.0) {
        var depth = 0.0;
        if (params.depth_blur_use_map == 1u) {
            let depth_color = textureLoad(depth_tex, coords, 0);
            depth = depth_color.r;
        } else {
            let dist_to_focus = distance(uv, vec2<f32>(params.depth_blur_focus_x, params.depth_blur_focus_y));
            depth = smoothstep(0.0, params.depth_blur_focus_radius, dist_to_focus);
        }
        
        let blur_r = mix(params.depth_blur_near_blur, params.depth_blur_far_blur, depth);
        if (blur_r > 0.1) {
            let r_limit = i32(clamp(blur_r, 0.0, 15.0));
            var sum = vec4<f32>(0.0);
            var weight = 0.0;
            for (var dy = -r_limit; dy <= r_limit; dy = dy + 1) {
                for (var dx = -r_limit; dx <= r_limit; dx = dx + 1) {
                    let sample_coords = coords + vec2<i32>(dx, dy);
                    let clamped_coords = clamp(sample_coords, vec2<i32>(0), vec2<i32>(i32(params.width) - 1, i32(params.height) - 1));
                    let c = textureLoad(input_tex, clamped_coords, 0);
                    let dist_sq = f32(dx * dx + dy * dy);
                    let w = exp(-dist_sq / max(2.0 * blur_r * blur_r, 0.1));
                    sum = sum + c * w;
                    weight = weight + w;
                }
            }
            color = sum / max(weight, 0.0001);
        }
    }

    // ============================================================
    // 7. FILM GRAIN — Temporal noise modulated by luminance
    // ============================================================
    if (params.film_grain_amount > 0.0) {
        let grain_fps = 24.0;
        let t_grain = floor(params.time * params.film_grain_speed * grain_fps) / grain_fps;
        let noise_val = hash(uv * 1234.5 + t_grain * 987.6) * 2.0 - 1.0;
        
        let luma = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        // Parabolic midtone mask: grain is strongest at mid-luminance, zero at black/white
        let grain_mask = 4.0 * luma * (1.0 - luma);
        
        color = vec4<f32>(color.rgb + noise_val * params.film_grain_amount * grain_mask, color.a);
    }

    // ============================================================
    // 8. FILM FLICKER — Organic brightness oscillation
    // ============================================================
    if (params.film_flicker_amount > 0.0) {
        let t_flicker = params.time * params.film_flicker_speed;
        // Product of incommensurate frequencies creates organic, non-repeating flicker
        let flicker = sin(t_flicker * 11.3) * cos(t_flicker * 5.7) * sin(t_flicker * 23.1);
        let flicker_factor = 1.0 + flicker * params.film_flicker_amount;
        color = vec4<f32>(color.rgb * flicker_factor, color.a);
    }

    textureStore(output_tex, coords, color);
}
