@group(0) @binding(0) var bg_tex: texture_2d<f32>;
@group(0) @binding(1) var media_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct CompositorParams {
    position: vec2<f32>,
    scale: vec2<f32>,
    rotation: f32,
    opacity: f32,
    clip_type: u32,
    blend_mode: u32,
    grayscale: u32,
    brightness: f32,
    padding1: u32,
    padding2: u32,
    solid_color: vec4<f32>,
}
@group(0) @binding(3) var<uniform> params: CompositorParams;

fn soft_light_channel(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
    } else {
        var d: f32;
        if (cb <= 0.25) {
            d = ((16.0 * cb - 12.0) * cb + 4.0) * cb;
        } else {
            d = sqrt(cb);
        }
        return cb + (2.0 * cs - 1.0) * (d - cb);
    }
}

fn soft_light(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        soft_light_channel(cb.r, cs.r),
        soft_light_channel(cb.g, cs.g),
        soft_light_channel(cb.b, cs.b)
    );
}

fn overlay_channel(cb: f32, cs: f32) -> f32 {
    if (cb <= 0.5) {
        return 2.0 * cb * cs;
    } else {
        return 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs);
    }
}

fn overlay(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        overlay_channel(cb.r, cs.r),
        overlay_channel(cb.g, cs.g),
        overlay_channel(cb.b, cs.b)
    );
}

fn hard_light_channel(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return 2.0 * cb * cs;
    } else {
        return 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs);
    }
}

fn hard_light(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        hard_light_channel(cb.r, cs.r),
        hard_light_channel(cb.g, cs.g),
        hard_light_channel(cb.b, cs.b)
    );
}

fn blend_colors(color_b: vec4<f32>, color_f: vec4<f32>, mode: u32) -> vec4<f32> {
    let src_raw = color_f.rgb;
    let dst_raw = color_b.rgb;
    
    var blend_raw: vec3<f32>;
    
    if (mode == 0u) { // normal
        blend_raw = src_raw;
    } else if (mode == 1u) { // multiply
        blend_raw = src_raw * dst_raw;
    } else if (mode == 2u) { // screen
        blend_raw = src_raw + dst_raw - (src_raw * dst_raw);
    } else if (mode == 3u) { // overlay
        blend_raw = overlay(dst_raw, src_raw);
    } else if (mode == 4u) { // darken
        blend_raw = min(src_raw, dst_raw);
    } else if (mode == 5u) { // lighten
        blend_raw = max(src_raw, dst_raw);
    } else if (mode == 6u) { // color_dodge
        blend_raw = vec3<f32>(
            select(dst_raw.r / max(1.0 - src_raw.r, 0.00001), 1.0, src_raw.r >= 1.0),
            select(dst_raw.g / max(1.0 - src_raw.g, 0.00001), 1.0, src_raw.g >= 1.0),
            select(dst_raw.b / max(1.0 - src_raw.b, 0.00001), 1.0, src_raw.b >= 1.0)
        );
    } else if (mode == 7u) { // color_burn
        blend_raw = vec3<f32>(
            select(1.0 - (1.0 - dst_raw.r) / max(src_raw.r, 0.00001), 0.0, src_raw.r <= 0.0),
            select(1.0 - (1.0 - dst_raw.g) / max(src_raw.g, 0.00001), 0.0, src_raw.g <= 0.0),
            select(1.0 - (1.0 - dst_raw.b) / max(src_raw.b, 0.00001), 0.0, src_raw.b <= 0.0)
        );
    } else if (mode == 8u) { // hard_light
        blend_raw = hard_light(dst_raw, src_raw);
    } else if (mode == 9u) { // soft_light
        blend_raw = soft_light(dst_raw, src_raw);
    } else if (mode == 10u) { // difference
        blend_raw = abs(dst_raw - src_raw);
    } else if (mode == 11u) { // exclusion
        blend_raw = dst_raw + src_raw - 2.0 * dst_raw * src_raw;
    } else {
        blend_raw = src_raw;
    }
    
    let src = vec4<f32>(src_raw * color_f.a, color_f.a);
    let dst = vec4<f32>(dst_raw * color_b.a, color_b.a);
    
    let out_a = src.a + dst.a * (1.0 - src.a);
    let out_rgb = src.rgb * (1.0 - dst.a) + dst.rgb * (1.0 - src.a) + src.a * dst.a * blend_raw;
    
    if (out_a > 0.0) {
        return vec4<f32>(out_rgb / out_a, out_a);
    } else {
        return vec4<f32>(0.0);
    }
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(bg_tex);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }

    let coords = vec2<i32>(id.xy);
    let bg_color = textureLoad(bg_tex, coords, 0);

    let tex_size = textureDimensions(media_tex);
    let clip_width = f32(tex_size.x);
    let clip_height = f32(tex_size.y);

    var p = vec2<f32>(id.xy) - params.position;

    let rad = -params.rotation * 3.14159265 / 180.0;
    let cos_r = cos(rad);
    let sin_r = sin(rad);
    p = vec2<f32>(
        p.x * cos_r - p.y * sin_r,
        p.x * sin_r + p.y * cos_r
    );

    p = p / params.scale;

    let local_coords = p + vec2<f32>(clip_width, clip_height) * 0.5;

    var fg_color = vec4<f32>(0.0);
    var in_bounds = false;

    if (local_coords.x >= 0.0 && local_coords.x < clip_width &&
        local_coords.y >= 0.0 && local_coords.y < clip_height) {
        in_bounds = true;
        if (params.clip_type == 1u) { // Solid
            fg_color = params.solid_color;
        } else { // Media
            let sample_pos = vec2<i32>(local_coords);
            fg_color = textureLoad(media_tex, sample_pos, 0);
        }
    }

    if (in_bounds) {
        if (params.grayscale == 1u) {
            let gray = dot(fg_color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
            fg_color = vec4<f32>(gray, gray, gray, fg_color.a);
        }

        fg_color = vec4<f32>(fg_color.rgb * params.brightness, fg_color.a * params.opacity);

        let blended_color = blend_colors(bg_color, fg_color, params.blend_mode);
        textureStore(output_tex, coords, blended_color);
    } else {
        textureStore(output_tex, coords, bg_color);
    }
}
