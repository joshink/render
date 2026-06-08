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
    voxelSize: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(f32(engine.width), f32(engine.height));
    
    let size = max(params.voxelSize, 4.0);
    
    // Scale by aspect ratio
    let scale = vec2<f32>(f32(engine.width), f32(engine.height) * params.aspectRatio) / size;
    let cell = floor(uv * scale);
    let local = fract(uv * scale) - vec2<f32>(0.5); // local cell coordinates from [-0.5, 0.5]
    
    // Sample color from center of cell
    let cell_center_uv = (cell + 0.5) / scale;
    let sample_coords = vec2<i32>(
        i32(clamp(cell_center_uv.x * f32(engine.width), 0.0, f32(engine.width) - 1.0)),
        i32(clamp(cell_center_uv.y * f32(engine.height), 0.0, f32(engine.height) - 1.0))
    );
    let cell_color = textureLoad(input_tex, sample_coords, 0);
    
    // Determine which face of the isometric cube we are on
    var face_shade = 1.0;
    var is_outline = false;
    
    let abs_x = abs(local.x);
    
    // Top face (rhombus top)
    if (local.y < -0.15 && abs_x < (local.y + 0.5) * 1.0) {
        face_shade = 1.0;
        // Outline check for top face
        if (local.y > -0.18 || abs_x > (local.y + 0.48) * 1.0) {
            is_outline = true;
        }
    }
    // Left face
    else if (local.x < 0.0 && local.y >= -0.15 && local.y < 0.5 + local.x * 0.7) {
        face_shade = 0.6;
        // Outline check
        if (local.x > -0.02 || local.y > 0.47 + local.x * 0.7) {
            is_outline = true;
        }
    }
    // Right face
    else if (local.x >= 0.0 && local.y >= -0.15 && local.y < 0.5 - local.x * 0.7) {
        face_shade = 0.8;
        // Outline check
        if (local.x < 0.02 || local.y > 0.47 - local.x * 0.7) {
            is_outline = true;
        }
    }
    // Background/Empty space around the isometric cube
    else {
        textureStore(output_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
    }
    
    var final_color = cell_color.rgb * face_shade;
    if (is_outline) {
        final_color = final_color * 0.3; // dark border outline
    }
    
    textureStore(output_tex, coords, vec4<f32>(final_color, cell_color.a));
}
