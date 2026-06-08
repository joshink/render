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
    barrelDistortion: f32,
    beamFocus: f32,
    brightness: f32,
    cellSize: f32,
    chromaRetention: f32,
    chromaticAberration: f32,
    crtMode: f32, // 0 = Slot Mask, 1 = Aperture Grille, 2 = Composite TV
    flickerIntensity: f32,
    glitchIntensity: f32,
    glitchSpeed: f32,
    highlightDrive: f32,
    highlightThreshold: f32,
    maskIntensity: f32,
    persistence: f32,
    scanlineIntensity: f32,
    shadowLift: f32,
    shoulder: f32,
    signalArtifacts: f32,
    vignetteIntensity: f32,
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
    
    // 1. Barrel Distortion
    let centered = uv - 0.5;
    let dist_sq = dot(centered, centered);
    let distorted_uv = uv + centered * dist_sq * params.barrelDistortion;
    
    // Bounds check
    if (distorted_uv.x < 0.0 || distorted_uv.x > 1.0 || distorted_uv.y < 0.0 || distorted_uv.y > 1.0) {
        textureStore(output_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
    }
    
    // 2. Chromatic Aberration
    let ca_offset = params.chromaticAberration / f32(engine.width);
    let sampleR = sample_color(distorted_uv + vec2<f32>(ca_offset, 0.0));
    let sampleG = sample_color(distorted_uv);
    let sampleB = sample_color(distorted_uv - vec2<f32>(ca_offset, 0.0));
    
    var color = vec3<f32>(sampleR.r, sampleG.g, sampleB.b) * params.brightness;
    
    // 3. Scanline Envelope
    let pitch = max(params.cellSize, 2.0);
    let scanline_y = distorted_uv.y * f32(engine.height);
    let scanline_weight = 1.0 - params.scanlineIntensity * (0.5 + 0.5 * sin(scanline_y / pitch * 6.28318));
    color = color * scanline_weight;
    
    // 4. Phosphor Mask (simplified aperture grille / slot mask)
    let mask_x = distorted_uv.x * f32(engine.width);
    var mask = vec3<f32>(1.0);
    
    if (params.crtMode < 0.5) {
        // Slot Mask
        let cell_x = i32(mask_x / pitch);
        let cell_y = i32(scanline_y / pitch);
        let stripe = (cell_x + cell_y % 2) % 3;
        var r_weight = 0.5;
        var g_weight = 0.5;
        var b_weight = 0.5;
        if (stripe == 0) { r_weight = 1.0; }
        else if (stripe == 1) { g_weight = 1.0; }
        else { b_weight = 1.0; }
        mask = mix(vec3<f32>(1.0), vec3<f32>(r_weight, g_weight, b_weight), params.maskIntensity);
    } else if (params.crtMode < 1.5) {
        // Aperture Grille (vertical RGB stripes)
        let stripe = i32(mask_x / (pitch / 3.0)) % 3;
        var r_weight = 0.4;
        var g_weight = 0.4;
        var b_weight = 0.4;
        if (stripe == 0) { r_weight = 1.0; }
        else if (stripe == 1) { g_weight = 1.0; }
        else { b_weight = 1.0; }
        mask = mix(vec3<f32>(1.0), vec3<f32>(r_weight, g_weight, b_weight), params.maskIntensity);
    } else {
        // Composite TV (horizontal/vertical phase artifacts)
        let checker = (i32(mask_x) + i32(scanline_y)) % 2;
        let artifact = mix(0.8, 1.2, f32(checker) * params.signalArtifacts);
        mask = vec3<f32>(artifact);
    }
    color = color * mask;
    
    // 5. Vignette
    let vignette_dist = length(centered * 2.0);
    let vignette_factor = clamp(1.0 - vignette_dist * vignette_dist * params.vignetteIntensity * 0.5, 0.0, 1.0);
    color = color * vignette_factor;
    
    // 6. Flicker
    let flicker_noise = sin(engine.clip_time * 60.0) * 0.5 + 0.5;
    let flicker_factor = 1.0 - params.flickerIntensity * flicker_noise * 0.1;
    color = color * flicker_factor;
    
    textureStore(output_tex, coords, vec4<f32>(color, sampleG.a));
}
