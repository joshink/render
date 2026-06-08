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
    colorMode: f32, // 0 = Source, 1 = Monochrome
    invert: f32,    // 0 = Normal, 1 = Inverted
    lineAngle: f32,
    linePitch: f32,
    lineThickness: f32,
    monoColorB: f32,
    monoColorG: f32,
    monoColorR: f32,
    noiseAmount: f32,
    noiseMode: f32, // 0 = Sine, 1 = Perlin, 2 = Turbulence
    presenceSoftness: f32,
    presenceThreshold: f32,
    scrollSpeed: f32,
    signalBlackPoint: f32,
    signalGamma: f32,
    signalWhitePoint: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn hash(p: vec2<f32>) -> f32 {
    let p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    let p3_added = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3_added.x + p3_added.y) * p3_added.z);
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
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    let px = vec2<f32>(id.xy);

    let angleRad = params.lineAngle * 3.14159265 / 180.0;
    let tDir = vec2<f32>(cos(angleRad), sin(angleRad));
    let nDir = vec2<f32>(-sin(angleRad), cos(angleRad));
    
    let nCoord = dot(px, nDir);
    let tCoord = dot(px, tDir);
    
    let pitch = max(params.linePitch, 2.0);
    let scroll = engine.clip_time * params.scrollSpeed * pitch * 2.0;
    let scrolledN = nCoord + scroll;
    let baseBand = floor(scrolledN / pitch);
    
    var tR = 0.0;
    var tG = 0.0;
    var tB = 0.0;
    
    // We sample a window of bands to draw lines smoothly
    for (var i = -3; i <= 3; i = i + 1) {
        let band = baseBand + f32(i);
        let ctrN = band * pitch + pitch * 0.5;
        
        let sampleN = ctrN - scroll;
        let cpx = tDir.x * tCoord + nDir.x * sampleN;
        let cpy = tDir.y * tCoord + nDir.y * sampleN;
        let cUv = vec2<f32>(cpx / f32(engine.width), cpy / f32(engine.height));
        let clampedUv = clamp(cUv, vec2<f32>(0.0), vec2<f32>(1.0));
        
        let sample_coords = vec2<i32>(
            i32(clampedUv.x * f32(engine.width - 1u)),
            i32(clampedUv.y * f32(engine.height - 1u))
        );
        let s = textureLoad(input_tex, sample_coords, 0);
        let sc = s.rgb;
        let luma = dot(sc, vec3<f32>(0.2126, 0.7152, 0.0722));
        
        let raw = mix(luma, 1.0 - luma, params.invert);
        let sigRange = max(params.signalWhitePoint - params.signalBlackPoint, 0.001);
        let sig = pow(clamp((raw - params.signalBlackPoint) / sigRange, 0.0, 1.0), 1.0 / max(params.signalGamma, 0.01));
        
        let hSoft = max(params.presenceSoftness * 0.5, 0.001);
        let invThresh = 1.0 - params.presenceThreshold;
        let pres = smoothstep(invThresh - hSoft, invThresh + hSoft, sig);
        
        // Noise generation
        var noiseVal = 0.0;
        let nx = tCoord * 0.012;
        let ny = band * 0.37 + 17.3;
        if (params.noiseMode > 1.5) {
            // Turbulence
            noiseVal = abs(noise(vec2<f32>(nx, ny)) * 2.0 - 1.0);
        } else if (params.noiseMode > 0.5) {
            // Perlin
            noiseVal = noise(vec2<f32>(nx, ny)) * 2.0 - 1.0;
        } else {
            // Sine
            noiseVal = sin(tCoord * 0.004 + band * 1.31);
        }
        
        let nDisp = noiseVal * pitch * params.noiseAmount * 2.0;
        let disp = sig * pitch * 5.0 + nDisp;
        let displaced = ctrN + disp;
        
        let dist = abs(scrolledN - displaced);
        let hw = params.lineThickness * 0.5 + sig * 0.5;
        
        let coreMask = 1.0 - smoothstep(max(0.0, hw - 1.0), hw + 1.0, dist);
        let glowMask = 1.0 - smoothstep(max(0.0, hw + sig * pitch * 0.08 - 2.0), hw + sig * pitch * 0.08 + 2.0, dist);
        
        let intensity = (coreMask + glowMask * 0.35) * pres * sig;
        
        let monoTint = vec3<f32>(params.monoColorR, params.monoColorG, params.monoColorB);
        let tint = mix(sc, monoTint, params.colorMode);
        
        tR = tR + tint.r * intensity;
        tG = tG + tint.g * intensity;
        tB = tB + tint.b * intensity;
    }
    
    textureStore(output_tex, coords, vec4<f32>(clamp(vec3<f32>(tR, tG, tB), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
}
