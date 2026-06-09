# Render Video Engine: External Interface Specification

This document provides the official external interface specification for the **Render** headless video rendering engine. It defines the command-line interface (CLI) options, the input JSON specification schema, the dynamic expression syntax, and the expected media outputs.

---

## 1. System Overview

The **Render** engine is a headless, GPU-accelerated utility designed to process declarative timeline configurations and compile them into static images or compressed video streams. 

```
┌─────────────────┐
│  CLI Invocation │──┐
└─────────────────┘  │
                     ▼
┌─────────────────┐     ┌──────────────────────┐     ┌────────────────┐
│ Input JSON Spec │────>│ Video Render Engine  │────>│  Compiled Video│
└─────────────────┘     │       (wgpu)         │     │   or Images    │
                     ▲  └──────────────────────┘     └────────────────┘
┌─────────────────┐  │
│  Media Assets   │──┘
└─────────────────┘
```

---

## 2. Command-Line Interface (CLI)

The rendering engine is executed as a command-line binary. The interface supports configuration parsing, include-path resolution, and diagnostic debugging.

### 2.1. Invocation Syntax
```bash
render-poc [OPTIONS] <SPEC_PATH>
```

### 2.2. Options and Arguments

| Flag / Option | Argument Type | Description |
| :--- | :--- | :--- |
| `<SPEC_PATH>` | Position / String | (Required) Path to the JSON file defining the render specification. |
| `-i`, `--input` | String | Alternative syntax to specify the path to the input JSON file. |
| `--debug` | Directory Path | (Optional) Enables debug mode and exports frames and execution logs to the specified directory. |
| `-I`, `--include` | Path | (Optional, repeatable) Directory or file path to scan for additional custom `.wgsl` shaders. |

### 2.3. Exit Codes

The binary exits with one of the following codes depending on the render outcome:

*   `0`: Render completed successfully; outputs generated.
*   `1`: Spec parse error (e.g. invalid JSON structure, mismatching types).
*   `2`: Resource resolution error (e.g. missing asset files, invalid font provider).
*   `3`: Shader compile error (e.g. custom WGSL syntax error, bind-group mismatch).
*   `4`: Runtime execution error (e.g. GPU out of memory, FFmpeg write pipe failure).

---

## 3. Render Specification Schema (JSON)

The render specification is a declarative JSON structure representing a single timeline composition.

### 3.1. Reference JSON Example

```json
{
  "version": "1.0",
  "output": "outputs/voyage.mp4",
  "composition": {
    "width": 1920,
    "height": 1080,
    "fps": 30,
    "duration": 15.0
  },
  "assets": {
    "ocean_video": {
      "type": "video",
      "path": "assets/ocean.mp4"
    },
    "watermark_logo": {
      "type": "image",
      "path": "assets/logo.png"
    },
    "custom_font": {
      "type": "font",
      "provider": "file",
      "path": "fonts/RobotoFlex.ttf"
    },
    "wave_shader": {
      "type": "shader",
      "path": "library/effects/wave.wgsl"
    }
  },
  "presets": [
    {
      "name": "Dreamy Glow",
      "inputs": [
        { "name": "blur_radius", "type": "float", "defaultValue": 5.0 },
        { "name": "glow_intensity", "type": "float", "defaultValue": 1.0 }
      ],
      "filters": [
        {
          "type": "blur",
          "params": {
            "radius": "$blur_radius"
          }
        },
        {
          "type": "glow",
          "params": {
            "intensity": "$glow_intensity",
            "radius": 4.0,
            "threshold": 0.4
          }
        }
      ]
    }
  ],
  "tracks": [
    {
      "id": "background_track",
      "clips": [
        {
          "id": "bg_clip",
          "type": "media",
          "asset": "ocean_video",
          "duration": 15.0,
          "scale_mode": "fill"
        }
      ]
    },
    {
      "id": "overlay_track",
      "start": 2.0,
      "clips": [
        {
          "id": "watermark_clip",
          "type": "media",
          "asset": "watermark_logo",
          "duration": 10.0,
          "offset": 1.0,
          "scale_mode": "fit",
          "blend_mode": "screen",
          "transform": {
            "position": {
              "expression": "[comp.width * 0.85, comp.height * 0.15]"
            },
            "scale": 0.15,
            "opacity": [
              { "time": 0.0, "value": 0.0, "easing": "ease_out" },
              { "time": 1.0, "value": 0.8 },
              { "time": 9.0, "value": 0.8, "easing": "ease_in" },
              { "time": 10.0, "value": 0.0 }
            ]
          }
        }
      ]
    },
    {
      "id": "text_track",
      "start": 3.0,
      "clips": [
        {
          "id": "title_text",
          "type": "text",
          "duration": 5.0,
          "text_params": {
            "kind": "layout",
            "body": {
              "type": "vstack",
              "spacing": 10.0,
              "children": [
                {
                  "type": "text",
                  "text": "Ocean Voyage",
                  "font": "custom_font",
                  "font_size": 48.0,
                  "color": [1.0, 1.0, 1.0, 1.0],
                  "axes": {
                    "wght": {
                      "expression": "300.0 + 400.0 * (0.5 + 0.5 * sin(clip_time * 2.0))"
                    }
                  }
                }
              ]
            }
          }
        }
      ]
    }
  ]
}
```

---

### 3.2. Root Spec Properties

| Field | Type | Description |
| :--- | :--- | :--- |
| `version` | String | (Required) Schema format version. Must be `"1.0"`. |
| `output` | String | (Required) Destination path for output. Extension determines format (`.mp4`, `.png`). |
| `composition` | Object | (Required) Structural and runtime properties of the render timeline. |
| `assets` | Map<String, Asset> | (Required) Registry of external files mapped by a unique identifier string. |
| `presets` | Array<Preset> | (Optional) Definitions for custom, reusable compound filters. |
| `tracks` | Array<Track> | (Required) Layered Z-indexed visual tracks. Index 0 is background. |
| `audio_tracks` | Array<AudioTrack> | (Optional) Audio mix timelines. |

---

### 3.3. Composition Settings

Describes the project's physical dimensions and playback attributes.

*   `width` (Integer, Required): Viewport width in pixels.
*   `height` (Integer, Required): Viewport height in pixels.
*   `fps` (Integer, Required): Frames per second.
*   `duration` (Float, Required): Playback runtime in seconds.

---

### 3.4. Assets Registry

The asset definitions point to external dependency resources.

```json
"assets": {
  "my_asset": {
    "type": "image",
    "path": "path/to/image.png"
  }
}
```

*   `type` (String, Required): One of `"video"`, `"image"`, `"audio"`, `"shader"`, or `"font"`.
*   `path` (String, Required): File path, system identifier, or remote URL.
*   `provider` (String, Optional): Source loader (required for `"font"`: `"file"`, `"system"`, or `"url"`).

---

### 3.5. Tracks & Clip Arrangements

Tracks represent Z-ordered layers composed onto the screen.

```json
"tracks": [
  {
    "id": "track_1",
    "start": 1.0,
    "clips": [ ... ]
  }
]
```

*   `id` (String, Required): Unique identifier for the track.
*   `start` (Float, Optional, Default `0.0`): The start offset of the track in seconds. The first clip inside begins at this offset.
*   `clips` (Array of Clips, Required): Sequential list of clips to play on this track.
    > [!IMPORTANT]
    > Clips inside a single track must be adjacent and sequential. They cannot overlap. To overlay clips, place them in separate Z-ordered tracks.

---

### 3.6. Visual Clips (`clips`)

Visual clips define content rendered onto a track at specific time windows.

*   `id` (String, Required): Unique identifier for the clip.
*   `type` (String, Required): Content model, must be one of:
    *   `"media"`: Video or image assets.
    *   `"solid"`: Solid background color.
    *   `"text"`: Rendered text overlay.
    *   `"effect"`: Adjustment layer applying filters to the layers below it.
*   `duration` (Float, Required): active screen duration in seconds.
*   `offset` (Float, Optional, Default `0.0`): A non-negative gap, in seconds, before this clip begins relative to the end of the previous clip on the track.
*   `trim_start` (Float, Optional, Default `0.0`): The start time within the source media, in seconds, from which playback begins.
*   `asset` (String, Optional): ID of the asset registry key (required for `"media"` type).
*   `scale_mode` (String, Optional, Default `"fit"`): Scaling strategy for media:
    *   `"fit"`: Uniform scale down to fit inside the composition bounds.
    *   `"fill"`: Scale up to cover composition bounds, cropping excess.
    *   `"natural"`: Retain source resolution without scaling (centered).
    *   `"stretch"`: Non-uniform scale to match composition dimensions exactly.
*   `solid_params` (Object, Optional): Defines solid color clip parameters.
    *   `color` (Array of 4 floats, Required): RGBA values normalized to `[0.0, 1.0]`.
*   `text_params` (Object, Optional): Configurations for text rendering.
    *   `text` (String): Content string for traditional text.
    *   `font` (String): Asset ID of the font.
    *   `font_size` (Polymorphic Float): Base height of the typeface.
    *   `color` (Array of 4 floats): RGBA text color.
    *   `axes` (Map<String, Polymorphic Float>): OpenType font axis overrides (e.g. `"wght"`, `"wdth"`, `"slnt"`).
    *   `kind` (String): If set to `"layout"`, activates the layout engine.
    *   `body` (LayoutNode): Root layout node structure.
*   `transform` (Object, Optional): Animatable spatial transforms:
    *   `position`: Target layout coordinate `[x, y]`. Defaults to composition center.
    *   `scale`: Scale factors `[sx, sy]` or a single uniform multiplier. Defaults to `1.0`.
    *   `rotation`: Rotation in degrees. Defaults to `0.0`.
    *   `opacity`: Opacity in `[0.0, 1.0]`. Defaults to `1.0`.
*   `blend_mode` (String, Optional, Default `"normal"`): Photoshop-style blend mode. Standard values:
    *   `"normal"`, `"multiply"`, `"screen"`, `"overlay"`, `"darken"`, `"lighten"`, `"color_dodge"`, `"color_burn"`, `"hard_light"`, `"soft_light"`, `"difference"`, `"exclusion"`.
*   `effects` (Array of Effects, Optional): Ordered array of post-processing filters.
*   `shader` (String, Optional): Shader asset ID to override default clip renderer.
*   `preset` (String, Optional): Preset definition ID to use for `"effect"` clips.
*   `params` (Map<String, Value>, Optional): Argument parameters passed to the custom shader or preset.

---

### 3.7. Text Layout Nodes

Used when `text_params.kind` is set to `"layout"` to construct complex structured typographic overlays.

```json
"body": {
  "type": "vstack",
  "spacing": 15.0,
  "children": [
    {
      "type": "text",
      "text": "Heading Line",
      "font": "header_font",
      "font_size": 32.0
    }
  ]
}
```

*   `type` (String, Required): Structural tag: `"vstack"`, `"hstack"`, `"zstack"`, `"spacer"`, or `"text"`.
*   `spacing` (Float, Optional): Gap between children in stacks.
*   `alignment` (String, Optional): Horizontal alignment (`"left"`, `"center"`, `"right"`).
*   `padding` (Array of 4 floats, Optional): Inner spacing `[top, right, bottom, left]`.
*   `size` (Float, Optional): Size of spacers.
*   `text` (String, Optional): Rendered content (for `"text"` nodes).
*   `font` (String, Optional): Font asset reference.
*   `font_size` (Polymorphic Float, Optional): Typographic scale height.
*   `color` (Array of 4 floats, Optional): Typographic RGBA color.
*   `axes` (Map<String, Polymorphic Float>, Optional): Variation axis mappings.

---

### 3.8. Transitions

Transitions cross-fade or wipe adjacent clips in a track over a specified overlay window.

```json
"transitions": [
  {
    "id": "cross_wipe",
    "type": "custom_shader",
    "shader": "wipe_shader",
    "start": 4.5,
    "duration": 1.0,
    "from": "clip_a",
    "to": "clip_b",
    "params": {
      "direction": 1.0
    }
  }
]
```

*   `id` (String, Required): Unique transition identifier.
*   `type` (String, Required): Transition pipeline type (e.g. `"custom_shader"`).
*   `shader` (String, Required): Custom shader asset ID.
*   `start` (Float, Optional): Absolute composition timeline start. Defaults to the midpoint between the outgoing and incoming clip boundaries.
*   `duration` (Float, Required): Runtime overlap duration in seconds.
*   `from` (String, Required): Outgoing clip ID.
*   `to` (String, Required): Incoming clip ID.
*   `params` (Map<String, Value>, Optional): Uniform parameters passed to transition shader.

### 3.8.1. Built-in Transition Shaders Library

The engine provides a collection of built-in transitions in the `library/transitions/` directory. These can be referenced in the transition spec using `type: "custom_shader"` and their corresponding shader asset ID pointing to the built-in WGSL file:

#### A. Flash / Burn In (`"shader": "flash_burn_shader"`)
Simulates a bright camera exposure flash peaking at the midpoint of the transition, combined with an organic film burn-in hot-spot that expands from the center to reveal the incoming clip.
*   `flash_intensity` (Float, Default `0.8`): Peak brightness offset of the camera exposure spike.
*   `burn_intensity` (Float, Default `1.0`): Noise-based boundary perturbation intensity of the expanding burn hotspot.

#### B. Focus Blur / Focus In (`"shader": "focus_in_shader"`)
Defocuses the outgoing clip and brings the incoming clip into focus, accompanied by a subtle focus breathing zoom effect that mimics real camera optics.
*   `max_blur` (Float, Default `20.0`): Maximum blur radius in pixels at the peak of defocus.

#### C. 35mm Slide Projector Switching (`"shader": "slide_switch_shader"`)
Emulates the mechanical slide swap in a physical 35mm projector. Features motion blur along the axis of movement, a black separation frame/gap, a damped spring-like snap landing bounce, random lamp flicker, and warm orange light leaks near the gap.
*   `direction` (Float, Default `0.0`): Movement direction (`0.0` for horizontal slide switch, `1.0` for vertical).
*   `gap_size` (Float, Default `0.1`): Thickness of the black slide mount frame border relative to screen height/width.
*   `flicker_intensity` (Float, Default `0.15`): Projector lamp brightness vibration intensity.

---

### 3.9. Presets

Presets define reusable compound visual filters that map custom arguments to child filters.

```json
"presets": [
  {
    "name": "Vaporwave",
    "inputs": [
      { "name": "intensity", "type": "float", "defaultValue": 0.5 }
    ],
    "filters": [
      {
        "type": "saturation",
        "params": { "factor": "1.0 + $intensity" }
      }
    ]
  }
]
```

*   `name` (String, Required): Global identifier for the preset.
*   `inputs` (Array of Objects, Required): Parameters declared by the preset.
    *   `name` (String, Required): Parameter key.
    *   `type` (String, Required): Data format (`"float"`, `"vec2"`, or `"string"`).
    *   `defaultValue` (Value, Required): Fallback parameter value.
*   `filters` (Array of Effects, Required): Array of filters evaluated when the preset is called. Variables are substituted using the `$<name>` syntax.

---

### 3.10. Audio Specs (`audio_tracks`)

Exposes audio mixing lanes processed in parallel with video layers.

*   `id` (String, Required): Unique track key.
*   `start` (Float, Optional, Default `0.0`): Track start offset in seconds.
*   `clips` (Array, Required): Audio clips on the track.
    *   `id` (String, Required): Unique clip key.
    *   `asset` (String, Required): Asset ID pointing to an audio file or a video containing audio.
    *   `duration` (Float, Required): Playback runtime in seconds.
    *   `offset` (Float, Optional, Default `0.0`): A non-negative gap, in seconds, before this audio clip begins relative to the end of the previous audio clip on this track.

---

## 4. Dynamic Values and Expression DSL

Every float or vector value (scale, position, shader parameters, opacities, font variation axes) supports dynamic resolution.

### 4.1. Supported Value Formats

#### Format A: Static Constant (Shorthand)
Specifies a constant value.
```json
"scale": 1.2,
"position": [400.0, 400.0]
```

#### Format B: Keyframe Array (Animated)
Allows keyframed interpolation.
```json
"opacity": [
  { "time": 0.0, "value": 0.0, "easing": "ease_out" },
  { "time": 2.0, "value": 1.0 }
]
```
*   `time` (Float, Required): Offset relative to the parent clip start (in seconds).
*   `value` (Float or Array, Required): Frame target value.
*   `easing` (String or Array, Optional, Default `"linear"`): Easing transition. Supported strings: `"linear"`, `"ease_in"`, `"ease_out"`, `"ease_in_out"`. Custom Bezier curves are supported as a 4-float coordinate array `[x1, y1, x2, y2]`.

#### Format C: Mathematical Expression (Expression DSL)
Evaluates dynamic math formulas per frame.
```json
"position": {
  "expression": "[comp.width * 0.5 + 50.0 * sin(clip_time * 2.0), comp.height * 0.5]"
}
```

---

### 4.2. Expression Syntax

The mathematical engine parses statements using a standard mathematical syntax:

*   **Operators**: `+`, `-`, `*`, `/`, `%`, `^` (exponentiation).
*   **Constants**: `pi`, `e`.
*   **Functions**: `sin(x)`, `cos(x)`, `tan(x)`, `abs(x)`, `sqrt(x)`, `min(x, y)`, `max(x, y)`, `clamp(x, min, max)`, `pow(x, y)`.
*   **Array Literals**: `[x, y]` or `[r, g, b, a]` for multi-dimensional vector properties.

### 4.3. Runtime Context Variables

Expressions have read-only access to the following environment scope:

| Variable | Type | Description |
| :--- | :--- | :--- |
| `comp.width` | Float | Canvas width in pixels. |
| `comp.height` | Float | Canvas height in pixels. |
| `time` | Float | Absolute timeline position in seconds. |
| `clip_time` | Float | Relative play position from the clip's start in seconds. |
| `clip.duration` | Float | Duration of the enclosing clip in seconds. |

---

## 5. Built-in Effects Catalog

These standard visual filters are packaged directly inside the engine and can be called under the `effects` block of any visual clip.

### 5.1. Grayscale (`"type": "grayscale"`)
Converts the RGB color channels to monochrome luminance.
*   `enabled` (Float, Default `1.0`): Active intensity (`0.0` is off, `1.0` is fully grayscale).

### 5.2. Brightness (`"type": "brightness"`)
Adjusts the exposure of the source image.
*   `factor` (Float, Default `1.0`): Luminance scale multiplier.

### 5.3. Contrast (`"type": "contrast"`)
Adjusts color contrast.
*   `factor` (Float, Default `1.0`): Contrast scale multiplier.

### 5.4. Saturation (`"type": "saturation"`)
Adjusts color saturation.
*   `factor` (Float, Default `1.0`): Color saturation multiplier.

### 5.5. Hue Rotate (`"type": "hue_rotate"`)
Shifts color hues.
*   `angle` (Float, Default `0.0`): Hue rotation angle in degrees `[0.0, 360.0]`.

### 5.6. Blur (`"type": "blur"`)
Applies a blur pass.
*   `radius` (Float, Default `0.0`): Filter sample radius size.

### 5.7. Glow (`"type": "glow"`)
Isolates bright pixels and smears them back over the source.
*   `intensity` (Float, Default `1.0`): Glow addition multiplier.
*   `radius` (Float, Default `4.0`): Smear blur radius.
*   `threshold` (Float, Default `0.4`): Lower luminance gate. Pixels darker than this are excluded from glow.

### 5.8. Film Grain (`"type": "film_grain"`)
Applies dynamic noise to the image.
*   `amount` (Float, Default `0.0`): Grain noise opacity.
*   `speed` (Float, Default `1.0`): Noise oscillation frame frequency.

### 5.9. Film Flicker (`"type": "film_flicker"`)
Applies brightness flicker.
*   `amount` (Float, Default `0.0`): Maximum brightness deviation scale.
*   `speed` (Float, Default `1.0`): Flicker frequency.

### 5.10. Depth Blur (`"type": "depth_blur"`)
Simulates shallow depth-of-field.
*   `focus_x` (Float, Default `0.5`): Focus center coordinate horizontal axis `[0.0, 1.0]`.
*   `focus_y` (Float, Default `0.5`): Focus center coordinate vertical axis `[0.0, 1.0]`.
*   `focus_radius` (Float, Default `0.25`): Bounds of unblurred region.
*   `near_blur` (Float, Default `0.0`): Blur radius inside focus area.
*   `far_blur` (Float, Default `10.0`): Blur radius outside focus area.
*   `use_map` (Float/String, Optional): Enables depth map asset file sampling if set.

### 5.11. Flow (`"type": "flow"`)
Applies feedback flow displacement.
*   `amount` (Float, Default `0.0`): Displacement strength.
*   `speed` (Float, Default `1.0`): Displacement wave speed.
*   `decay` (Float, Default `0.96`): Feedback buffer persistent decay.

---

## 6. Execution Output & Verification

The engine generates outputs based on the destination file extension specified in the `output` field:

### 6.1. Media Output Formats
*   **Static Image (`.png` / `.jpg`)**: Renders the composition frame at timeline point `0.0` and exports a lossless image file.
*   **Video / Audio (`.mp4`)**: Mixes all audio and visual tracks, pipes frame-by-frame raw video frames into an internal FFmpeg subprocess, and exports a H.264/AAC-compressed video stream.

### 6.2. Debug Run Directory Structure
When run with the `--debug <debug_dir>` flag, the engine creates a unique timestamped subfolder representing the run.

```
<debug_dir>/
├── logs.txt
├── review.json
└── frames/
    ├── frame_0000.png
    ├── frame_0030.png
    └── frame_0060.png
```

*   `logs.txt`: Execution trace tracking thread speeds, shader compiles, and pipe writes.
*   `frames/`: Diagnostic spot-checks containing keyframe outputs and track transition moments.
*   `review.json`: A machine-readable list mapping spotcheck frames to timestamps and event explanations.

```json
[
  {
    "frame": 30,
    "timestamp": 1.0,
    "file": "frames/frame_0030.png",
    "explanation": "Track 'overlay_track' - Clip 'watermark_clip' starts"
  }
]
```
