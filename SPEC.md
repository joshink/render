# Render Engine: Input JSON Specification Schema

This document defines the official input schema for the **Render** headless video rendering engine. The schema uses a declarative JSON structure to represent a complete video timeline, including global project settings, asset libraries, multi-track layering, keyframed property animations, scripting expressions, and custom WGSL shaders.

---

## 1. Core Architecture Principles

Based on our architectural decisions, the input format adheres to these constraints:

1. **JSON-Based:** The schema is written in standard JSON, enabling easy parsing with `serde_json` and direct compatibility with web-based editor frontends.
2. **Flat Composition:** The project consists of a single global timeline containing parallel, Z-ordered tracks (no nested compositions / pre-comps for simplicity).
3. **Explicit Overlaps:** Tracks utilize absolute timeline coordinates (seconds) for clip placement. If clips overlap, their compositing or transitions must be explicitly declared at those timestamps.
4. **Polymorphic Property Animation & Scripting:** Any property that can change over time (transforms, opacity, shader parameters, text parameters) can be declared in three ways:
   *   *Static constant* (e.g., `0.8`).
   *   *Keyframe array* (e.g., `[ { "time": 0.0, "value": 0.0 }, { "time": 2.0, "value": 1.0 } ]`).
   *   *Mathematical Expression / Script* (e.g., `{ "expression": "0.05 * comp.width + 10.0" }`).
5. **Expression Context (Relative Sizing):** Expressions have access to runtime environment variables, making it easy to define resolution-independent coordinates and sizes (e.g., `comp.width`, `comp.height`, `time`, `clip.time`, `clip.duration`).
6. **Hybrid Easing:** Keyframes support both named easing curves (e.g. `"linear"`, `"ease_out"`) and custom Cubic Bezier velocity curves (e.g. `[0.42, 0.0, 0.58, 1.0]`).
7. **Polymorphic Clips:** Clips on tracks can be media files (`"media"`), solid color layers (`"solid"`), or text overlays (`"text"`), each with its own specialized parameters.
8. **Font & Variable Font Extension:** Font assets can be loaded from file paths, system fonts, or URLs. Text clips can bind polymorphic values to variable font axes (like `"wght"`, `"wdth"`, `"slnt"`).
9. **Media Scaling Modes:** Media clips support custom scaling modes (`"fit"`, `"fill"`, `"natural"`, or `"stretch"`) to dictate how their aspect ratios are handled relative to the composition window.
10. **Automatic Shader Reflection:** Custom shaders are loaded as assets. The engine parses the WGSL source at runtime using `naga` to reflect uniform layouts, mapping JSON parameter keys directly to uniform struct fields.
11. **Standardized Transition API:** Custom transition shaders sample from standard bindings: outgoing clip texture at binding 0 (`tex_from`), incoming clip texture at binding 1 (`tex_to`), and the blended transition progress float at binding 2 (`progress`).

---

## 2. Complete Schema Example

Below is a complete, production-ready example of the input JSON specification:

```json
{
  "version": "1.0",
  "composition": {
    "width": 1920,
    "height": 1080,
    "fps": 30,
    "duration": 15.0
  },
  "assets": {
    "background_video": {
      "type": "video",
      "path": "assets/ocean.mp4"
    },
    "watermark_image": {
      "type": "image",
      "path": "assets/logo.png"
    },
    "variable_roboto_font": {
      "type": "font",
      "provider": "file",
      "path": "assets/RobotoFlex.ttf"
    },
    "custom_wave_shader": {
      "type": "shader",
      "path": "shaders/wave.wgsl"
    },
    "custom_transition_shader": {
      "type": "shader",
      "path": "shaders/page_curl.wgsl"
    }
  },
  "tracks": [
    {
      "id": "overlay_track",
      "clips": [
        {
          "id": "logo_overlay",
          "type": "media",
          "asset": "watermark_image",
          "start": 1.0,
          "duration": 10.0,
          "scale_mode": "fit",
          "transform": {
            // Polymorphic: Position is an expression relative to the viewport width/height
            "position": {
              "expression": "[comp.width * 0.85, comp.height * 0.15]"
            },
            // Polymorphic: Scale is static (shorthand format)
            "scale": [0.1, 0.1],
            // Polymorphic: Rotation is animated via keyframes using custom Bezier curves
            "rotation": [
              { "time": 1.0, "value": 0.0 },
              { "time": 11.0, "value": 360.0, "easing": [0.25, 0.1, 0.25, 1.0] }
            ],
            // Polymorphic: Opacity is animated via keyframes
            "opacity": [
              { "time": 1.0, "value": 0.0, "easing": "ease_out" },
              { "time": 2.0, "value": 0.8 },
              { "time": 10.0, "value": 0.8, "easing": "ease_in" },
              { "time": 11.0, "value": 0.0 }
            ]
          }
        },
        {
          "id": "title_text",
          "type": "text",
          "start": 2.0,
          "duration": 5.0,
          "text_params": {
            "text": "Ocean Voyage",
            "font": "variable_roboto_font",
            // Polymorphic Expression: Sizing is 6% of the composition height (vw/vh style)
            "font_size": {
              "expression": "comp.height * 0.06"
            },
            "color": [1.0, 1.0, 1.0, 1.0],
            // Variable Font axes
            "axes": {
              // Polymorphic: Weight animate over time (thin to heavy bold)
              "wght": [
                { "time": 2.0, "value": 100.0, "easing": "ease_in_out" },
                { "time": 4.0, "value": 900.0 }
              ],
              // Polymorphic: Width varies via a dynamic mathematical sin wave script
              "wdth": {
                "expression": "100.0 + 50.0 * sin(clip.time * 2.0 * pi())"
              }
            }
          },
          "transform": {
            "position": {
              "expression": "[comp.width * 0.5, comp.height * 0.5]"
            },
            "scale": 1.0,
            "rotation": 0.0,
            "opacity": 1.0
          }
        }
      ],
      "transitions": []
    },
    {
      "id": "main_track",
      "clips": [
        {
          "id": "video_clip_1",
          "type": "media",
          "asset": "background_video",
          "start": 0.0,
          "duration": 6.0,
          "scale_mode": "fill",
          "effects": [
            {
              "type": "custom_shader",
              "shader": "custom_wave_shader",
              "params": {
                "amplitude": [
                  { "time": 0.0, "value": 0.02, "easing": "linear" },
                  { "time": 6.0, "value": 0.08 }
                ],
                "frequency": 3.0
              }
            }
          ]
        },
        {
          "id": "solid_color_clip",
          "type": "solid",
          "start": 5.0,
          "duration": 5.0,
          "solid_params": {
            "color": [0.1, 0.1, 0.2, 1.0]
          }
        }
      ],
      "transitions": [
        {
          "id": "transition_1",
          "type": "custom_shader",
          "shader": "custom_transition_shader",
          "start": 5.0,
          "duration": 1.0,
          "from": "video_clip_1",
          "to": "solid_color_clip"
        }
      ]
    }
  ]
}
```

---

## 3. Detailed Field Schema

### 3.1. Project Metadata
*   `version` (string, required): Schema version (e.g. `"1.0"`).

### 3.2. Global Composition (`composition`)
Describes the output dimensions and frame metrics.
*   `width` (integer, required): Width of the rendered output in pixels.
*   `height` (integer, required): Height of the rendered output in pixels.
*   `fps` (integer, required): Frame rate (frames per second) for the output file.
*   `duration` (float, required): Total duration of the composition in seconds.

### 3.3. Asset Library (`assets`)
Declares external media files or script resources. The keys represent unique IDs used elsewhere in the timeline.
*   `type` (string, required): One of `"video"`, `"image"`, `"shader"`, or `"font"`.
*   `provider` (string, optional, required for `"font"`):
    *   `"file"`: Loaded from a local filesystem path.
    *   `"system"`: Loaded by name from local operating system font libraries.
    *   `"url"`: Fetched dynamically from a URL (e.g. Google Fonts WOFF2 link).
*   `path` (string, required): Absolute or relative path, font name, or URL.

### 3.4. Tracks (`tracks`)
An ordered array of parallel timelines. Lower indices are rendered first (background), while higher indices overlay on top (foreground).
*   `id` (string, required): Unique identifier for the track.
*   `clips` (array of Clip, required): List of clips scheduled on this track.
*   `transitions` (array of Transition, optional): List of transitions occurring between overlapping clips on this track.

### 3.5. Clips (`clips`)
Clips represent intervals of time on a track when content is rendered.
*   `id` (string, required): Unique identifier for the clip.
*   `type` (string, required): Specifies the content type:
    *   `"media"`: Renders an external video or image asset.
    *   `"solid"`: Renders a flat, solid-colored canvas.
    *   `"text"`: Renders a vector/rasterized text overlay.
*   `start` (float, required): Start time on the composition timeline in seconds.
*   `duration` (float, required): Playback duration of the clip in seconds.
*   `asset` (string, required for `"media"`): The ID of the asset (image/video) in the global library.
*   `scale_mode` (string, optional for `"media"`, defaults to `"fit"`):
    *   `"fit"`: Scales the asset to fit completely inside the composition viewport while maintaining its aspect ratio.
    *   `"fill"`: Scales the asset to completely cover the composition viewport, cropping any excess.
    *   `"natural"`: Keeps original asset resolution (1:1), centering it.
    *   `"stretch"`: Stretches the asset to match composition dimensions exactly.
*   `solid_params` (object, required for `"solid"`):
    *   `color` (array of 4 floats, required): RGBA values normalized to `[0.0, 1.0]`.
*   `text_params` (object, required for `"text"`):
    *   `text` (string, required): The text content to display.
    *   `font` (string, required): ID of the font asset in the global library.
    *   `font_size` (polymorphic number, required): Target height of the text (supports constant, keyframes, or expressions).
    *   `color` (array of 4 floats, required): Text RGBA color `[0.0, 1.0]`.
    *   `axes` (object, optional): Map of 4-character OpenType variation axis tags (e.g. `"wght"`, `"wdth"`, `"slnt"`, `"opsz"`) to polymorphic animatable values.
*   `transform` (object, optional): Geometric spatial configuration. All values are polymorphic (static constant OR keyframe array OR expression).
    *   `position`: Coordinates as `[x, y]` relative to composition size. Defaults to `[comp.width * 0.5, comp.height * 0.5]`.
    *   `scale`: Scale factors as `[sx, sy]` or a single float for uniform scaling. Defaults to `[1.0, 1.0]`.
    *   `rotation`: Rotation angle in degrees. Defaults to `0.0`.
    *   `opacity`: Alpha blending opacity in `[0.0, 1.0]`. Defaults to `1.0`.
*   `effects` (array of Effect, optional): Post-processing filters applied sequentially to the clip's output texture.

---

## 3.6. Polymorphic Animatable & Scripted Property Structure
Any numerical or vector property (e.g. transforms, font sizing, font variation axes, shader uniforms) supports three formats:

### A. Shorthand Static Value (Constant)
Specifies a constant value for the duration of the clip:
```json
"opacity": 0.8,
"position": [960.0, 540.0]
```

### B. Keyframe Array (Animated)
Specifies interpolation over time:
```json
"opacity": [
  { "time": 0.0, "value": 0.0, "easing": "ease_out" },
  { "time": 1.5, "value": 1.0 }
]
```
*   `time` (float, required): Absolute timeline timestamp in seconds.
*   `value` (number | array of numbers, required): Value at timestamp.
*   `easing` (string | array of 4 floats, optional): Curve to next keyframe (`"linear"`, `"ease_in"`, `"ease_out"`, `"ease_in_out"`, or `[x1, y1, x2, y2]`).

### C. Mathematical Expression (Scripted / Relative)
Specifies a dynamic mathematical expression evaluated per frame:
```json
"font_size": {
  "expression": "comp.height * 0.05 + 12.0"
}
```

---

## 4. GPU Shader Binding Interfaces

Shaders consume two types of uniform data:
1.  **Standard Engine Parameters (Built-in):** Auto-generated by the engine. Shaders bind this to a fixed structure containing global time, relative progress, and frame resolutions.
2.  **Custom Parameters (Reflected):** Declared in the WGSL code and matching properties supplied in the JSON. The engine uses Naga reflection to extract names, layouts, and offsets, packing values dynamically.

---

### 4.1. Custom Effect Shaders

Effect shaders are compute shaders that receive a single frame's input texture and write the filtered result to an output storage texture.

#### Standard Bindings for Effects:
*   `@group(0) @binding(0) var input_tex: texture_2d<f32>;` (Input source texture)
*   `@group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;` (Output destination texture)
*   `@group(0) @binding(2) var<uniform> engine: EngineParams;` (Built-in engine parameters)
*   `@group(0) @binding(3) var<uniform> params: CustomParams;` (Custom reflected parameters mapped from JSON)

#### Standard Effect Uniform Struct (`EngineParams`):
```wgsl
struct EngineParams {
    time: f32,         // Current composition timeline time in seconds
    clip_time: f32,    // Relative time in seconds since the current clip started
    progress: f32,     // Progress ratio through the current clip [0.0 - 1.0]
    width: u32,        // Composition resolution width in pixels
    height: u32,       // Composition resolution height in pixels
}
```

#### Complete WGSL Custom Effect Example:
```wgsl
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

// Custom fields. Naga dynamically reflects this and maps properties from JSON `params`
struct CustomParams {
    amplitude: f32,
    frequency: f32,
}
@group(0) @binding(3) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let uv = vec2<f32>(id.xy) / vec2<f32>(engine.width, engine.height);
    
    // Wave offset calculation using a mix of standard time and custom reflected inputs
    let offset = sin(uv.y * params.frequency + engine.clip_time * 5.0) * params.amplitude;
    let sample_coords = vec2<i32>(i32(f32(coords.x) + offset * f32(engine.width)), coords.y);
    
    let color = textureLoad(input_tex, sample_coords, 0);
    textureStore(output_tex, coords, color);
}
```

---

### 4.2. Custom Transition Shaders

Transition shaders execute compute operations to blend outgoing frames (`tex_from`) and incoming frames (`tex_to`).

#### Standard Bindings for Transitions:
*   `@group(0) @binding(0) var tex_from: texture_2d<f32>;` (Outgoing source frame)
*   `@group(0) @binding(1) var tex_to: texture_2d<f32>;` (Incoming destination frame)
*   `@group(0) @binding(2) var output_tex: texture_storage_2d<rgba8unorm, write>;` (Output buffer)
*   `@group(0) @binding(3) var<uniform> engine: TransitionEngineParams;` (Built-in engine parameters)
*   `@group(0) @binding(4) var<uniform> params: CustomParams;` (Custom transition parameters)

#### Standard Transition Uniform Struct (`TransitionEngineParams`):
```wgsl
struct TransitionEngineParams {
    progress: f32,     // Normalized transition progress [0.0 - 1.0] (computed using easing)
    duration: f32,     // Total transition duration in seconds
    width: u32,        // Resolution width in pixels
    height: u32,       // Resolution height in pixels
}
```

#### Complete WGSL Custom Transition Example:
```wgsl
@group(0) @binding(0) var tex_from: texture_2d<f32>;
@group(0) @binding(1) var tex_to: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba8unorm, write>;

struct TransitionEngineParams {
    progress: f32,
    duration: f32,
    width: u32,
    height: u32,
}
@group(0) @binding(3) var<uniform> engine: TransitionEngineParams;

// Custom user parameters (e.g. wipe smoothness)
struct CustomParams {
    smoothness: f32,
}
@group(0) @binding(4) var<uniform> params: CustomParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= engine.width || id.y >= engine.height) { return; }

    let coords = vec2<i32>(id.xy);
    let color_from = textureLoad(tex_from, coords, 0);
    let color_to = textureLoad(tex_to, coords, 0);

    // Simple horizontal wipe transition using custom smoothness parameter
    let uv = vec2<f32>(id.xy) / vec2<f32>(engine.width, engine.height);
    let bound = engine.progress * (1.0 + params.smoothness);
    let alpha = smoothstep(bound - params.smoothness, bound, uv.x);
    
    let blended_color = mix(color_from, color_to, alpha);
    textureStore(output_tex, coords, blended_color);
}
```
