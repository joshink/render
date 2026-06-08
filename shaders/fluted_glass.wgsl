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
    amplitude: f32,
    angle: f32,
    frequency: f32,
    irregularity: f32,
    preset: f32, // 0 = Architectural, 1 = Painterly
    warp: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn hash2D(p: vec2<f32>) -> f32 {
    let p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    let p3_added = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3_added.x + p3_added.y) * p3_added.z);
}

fn noise2D(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash2D(i + vec2<f32>(0.0, 0.0));
    let b = hash2D(i + vec2<f32>(1.0, 0.0));
    let c = hash2D(i + vec2<f32>(0.0, 1.0));
    let d = hash2D(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y) * 2.0 - 1.0;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let rad = params.angle * 3.14159265 / 180.0;
    let cos_a = cos(rad);
    let sin_a = sin(rad);
    
    let alongAxis = uv.x * cos_a + uv.y * sin_a;
    let acrossAxis = -uv.x * sin_a + uv.y * cos_a;
    
    let coarse = noise2D(vec2<f32>(alongAxis * 1.35, acrossAxis * 1.1));
    let detail = noise2D(vec2<f32>(alongAxis * 3.9 + 11.7, acrossAxis * 2.4 - 4.3));
    let warpVal = noise2D(vec2<f32>(alongAxis * 1.6 - 8.2, acrossAxis * 4.8 + 2.7));
    
    var lensNormal: f32 = 0.0;
    var presetStrength: f32 = 1.7;
    
    if (params.preset > 0.5) {
        // Painterly
        presetStrength = 2.7;
        let bandIndex = floor(alongAxis * params.frequency);
        let bandNoise = noise2D(vec2<f32>(bandIndex * 0.19, acrossAxis * 0.45));
        let painterlyDomain = alongAxis + coarse * params.irregularity * 0.055 + bandNoise * params.irregularity * 0.045 + warpVal * params.warp * 0.02;
        let painterlyCell = fract(painterlyDomain * params.frequency + bandNoise * params.irregularity * 0.85);
        let localCoord = painterlyCell * 2.0 - 1.0;
        let absCoord = abs(localCoord);
        let body = max(0.0, 1.0 - localCoord * localCoord);
        let shoulder = smoothstep(0.08, 0.52, absCoord) * (1.0 - smoothstep(0.62, 0.98, absCoord));
        let profile = body * 0.38 + shoulder * 0.84 * (1.0 + detail * params.warp * 0.35);
        let internalWarp = detail * params.warp * body * 0.55 + warpVal * params.irregularity * shoulder * 0.24;
        lensNormal = localCoord * profile + internalWarp;
    } else {
        // Architectural
        presetStrength = 1.7;
        let architecturalDomain = alongAxis + coarse * params.irregularity * 0.028;
        let architecturalCell = fract(architecturalDomain * params.frequency);
        let localCoord = architecturalCell * 2.0 - 1.0;
        let absCoord = abs(localCoord);
        let curve = max(0.0, 1.0 - localCoord * localCoord);
        let profile = pow(curve, 1.08) * 0.7 * (1.0 - absCoord * params.irregularity * 0.18);
        lensNormal = localCoord * profile;
    }
    
    let perpX = -sin_a;
    let perpY = cos_a;
    let refractionStrength = params.amplitude * presetStrength;
    let baseDisp = lensNormal * refractionStrength + warpVal * params.warp * params.amplitude * 0.22;
    
    let refractedUv = clamp(uv + vec2<f32>(perpX * baseDisp, perpY * baseDisp), vec2<f32>(0.0), vec2<f32>(1.0));
    
    let sample_coords = vec2<i32>(
        i32(refractedUv.x * f32(engine.width - 1u)),
        i32(refractedUv.y * f32(engine.height - 1u))
    );
    
    let src_color = textureLoad(input_tex, coords, 0);
    let refr_color = textureLoad(input_tex, sample_coords, 0);
    
    let mix_factor = mix(0.14, 0.08, params.preset);
    let final_color = mix(refr_color.rgb, src_color.rgb, mix_factor * clamp(1.0 - params.amplitude * 8.0, 0.0, 0.85));
    
    textureStore(output_tex, coords, vec4<f32>(final_color, src_color.a));
}
