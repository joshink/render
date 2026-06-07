@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct Params {
    grayscale: u32,
    brightness: f32,
}
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(input_tex);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }

    // Load pixel from the input texture
    let coords = vec2<i32>(id.xy);
    var color = textureLoad(input_tex, coords, 0);
    
    // Apply grayscale if enabled (using standard ITU-R BT.709 luma coefficients)
    if (params.grayscale == 1u) {
        let gray = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        color = vec4<f32>(gray, gray, gray, color.a);
    }

    // Apply brightness factor
    color = vec4<f32>(color.rgb * params.brightness, color.a);

    // Write back to the output storage texture
    textureStore(output_tex, coords, color);
}
