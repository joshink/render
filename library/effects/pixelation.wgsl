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
    aspectRatio: f32,
    cellSize: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let cellW = params.cellSize / f32(engine.width);
    let cellH = (params.cellSize * params.aspectRatio) / f32(engine.height);

    let cellX = floor(uv.x / max(cellW, 0.0001));
    let cellY = floor(uv.y / max(cellH, 0.0001));
    
    let cellCenterUv = vec2<f32>(
        (cellX + 0.5) * cellW,
        (cellY + 0.5) * cellH
    );
    
    let sample_coords = vec2<i32>(
        i32(clamp(cellCenterUv.x * f32(engine.width), 0.0, f32(engine.width) - 1.0)),
        i32(clamp(cellCenterUv.y * f32(engine.height), 0.0, f32(engine.height) - 1.0))
    );
    
    let color = textureLoad(input_tex, sample_coords, 0);
    textureStore(output_tex, vec2<i32>(id.xy), color);
}
