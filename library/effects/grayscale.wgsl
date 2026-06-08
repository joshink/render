/* EFFECTS_METADATA:
{
  "type": "grayscale",
  "params": [
    { "name": "enabled", "type": "bool", "default": 1.0 }
  ],
  "spot_checks": [
    {
      "time_start": 0.5,
      "time_step": 1.0,
      "param_name": "enabled",
      "format": "Grayscale is {}"
    }
  ]
}
*/

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
    enabled: i32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }
    let coords = vec2<i32>(id.xy);
    var color = textureLoad(input_tex, coords, 0);
    if (params.enabled == 1) {
        let gray = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        color = vec4<f32>(gray, gray, gray, color.a);
    }
    textureStore(output_tex, coords, color);
}
