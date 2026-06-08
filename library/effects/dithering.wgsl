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
    algorithm: f32, // 0 = Bayer 2x2, 1 = Bayer 4x4, 2 = Bayer 8x8, 3 = Noise
    animate: f32,   // 0.0 or 1.0
    chromaticSplit: f32, // 0.0 or 1.0
    colorMode: f32, // 0 = source, 1 = monochrome, 2 = duo-tone
    dotScale: f32,
    highlightColorB: f32,
    highlightColorG: f32,
    highlightColorR: f32,
    monoColorB: f32,
    monoColorG: f32,
    monoColorR: f32,
    pixelSize: f32,
    shadowColorB: f32,
    shadowColorG: f32,
    shadowColorR: f32,
    speed: f32,
    spread: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

fn hash2D(p: vec2<f32>) -> f32 {
    let p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    let p3_added = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3_added.x + p3_added.y) * p3_added.z);
}

fn bayer2(coord: vec2<u32>) -> f32 {
    let x = coord.x % 2u;
    let y = coord.y % 2u;
    var m = array<f32, 4>(0.0, 2.0, 3.0, 1.0);
    return m[x + y * 2u] / 4.0;
}

fn bayer4(coord: vec2<u32>) -> f32 {
    let x = coord.x % 4u;
    let y = coord.y % 4u;
    var m = array<f32, 16>(
         0.0,  8.0,  2.0, 10.0,
        12.0,  4.0, 14.0,  6.0,
         3.0, 11.0,  1.0,  9.0,
        15.0,  7.0, 13.0,  5.0
    );
    return m[x + y * 4u] / 16.0;
}

fn bayer8(coord: vec2<u32>) -> f32 {
    let x = coord.x % 8u;
    let y = coord.y % 8u;
    let val4 = bayer4(vec2<u32>(x % 4u, y % 4u)) * 16.0;
    let val2 = bayer2(vec2<u32>((x / 4u) % 2u, (y / 4u) % 2u)) * 4.0;
    return (4.0 * val4 + val2) / 64.0;
}

fn get_threshold(coord: vec2<f32>, alg: f32, timeOffset: f32) -> f32 {
    let p = coord + timeOffset * 256.0;
    if (alg < 0.5) {
        return bayer2(vec2<u32>(abs(p)));
    } else if (alg < 1.5) {
        return bayer4(vec2<u32>(abs(p)));
    } else if (alg < 2.5) {
        return bayer8(vec2<u32>(abs(p)));
    } else {
        return hash2D(p);
    }
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let effectivePx = max(params.pixelSize, 1.0);
    let fragCoord = uv * vec2<f32>(f32(engine.width), f32(engine.height));
    let cellCoordinates = floor(fragCoord / effectivePx);
    let snappedUv = (cellCoordinates + 0.5) * effectivePx / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let timeOffset = engine.clip_time * params.speed * params.animate;
    
    // Matrix size: bayer2 is 2, bayer4 is 4, bayer8 is 8, noise is 64
    var matrixSize: f32 = 4.0;
    if (params.algorithm < 0.5) {
        matrixSize = 2.0;
    } else if (params.algorithm < 1.5) {
        matrixSize = 4.0;
    } else if (params.algorithm < 2.5) {
        matrixSize = 8.0;
    } else {
        matrixSize = 64.0;
    }
    
    let splitOffset = params.chromaticSplit / matrixSize;
    
    let snapped_coords = vec2<i32>(
        i32(clamp(snappedUv.x * f32(engine.width), 0.0, f32(engine.width) - 1.0)),
        i32(clamp(snappedUv.y * f32(engine.height), 0.0, f32(engine.height) - 1.0))
    );
    let src = textureLoad(input_tex, snapped_coords, 0);
    
    // Sample threshold
    let thR = get_threshold(cellCoordinates, params.algorithm, timeOffset) - 0.5;
    let thG = get_threshold(cellCoordinates + vec2<f32>(splitOffset * matrixSize, 0.0), params.algorithm, timeOffset) - 0.5;
    let thB = get_threshold(cellCoordinates + vec2<f32>(0.0, splitOffset * matrixSize), params.algorithm, timeOffset) - 0.5;
    
    // Per-channel quantization
    let adjustedR = src.r + thR * params.spread;
    let adjustedG = src.g + thG * params.spread;
    let adjustedB = src.b + thB * params.spread;
    
    // Simple 1-bit or 2-bit quantization based on threshold
    let quantR = clamp(floor(adjustedR + 0.5), 0.0, 1.0);
    let quantG = clamp(floor(adjustedG + 0.5), 0.0, 1.0);
    let quantB = clamp(floor(adjustedB + 0.5), 0.0, 1.0);
    
    let quantizedColor = vec3<f32>(quantR, quantG, quantB);
    
    let quantizedLuma = dot(quantizedColor, vec3<f32>(0.2126, 0.7152, 0.0722));
    
    var colorResult: vec3<f32>;
    if (params.colorMode > 1.5) {
        // Duo-tone
        let shadowTint = vec3<f32>(params.shadowColorR, params.shadowColorG, params.shadowColorB);
        let highlightTint = vec3<f32>(params.highlightColorR, params.highlightColorG, params.highlightColorB);
        colorResult = mix(shadowTint, highlightTint, quantizedLuma);
    } else if (params.colorMode > 0.5) {
        // Monochrome
        let monoTint = vec3<f32>(params.monoColorR, params.monoColorG, params.monoColorB);
        colorResult = vec3<f32>(quantizedLuma) * monoTint;
    } else {
        // Source
        colorResult = quantizedColor;
    }
    
    // Dot scale mask inside cell
    let cellFrac = fract(fragCoord / effectivePx);
    let centered = cellFrac - vec2<f32>(0.5);
    let dist = max(abs(centered.x), abs(centered.y));
    let halfSize = 0.5 * params.dotScale;
    let mask = smoothstep(halfSize, halfSize - 0.01, dist);
    
    textureStore(output_tex, coords, vec4<f32>(colorResult * mask, src.a));
}
