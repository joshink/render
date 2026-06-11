# Render — Input & Interface Specification

This document is the formal, normative specification for the **Render** engine's
external interface. It defines:

1. The command-line interface (arguments, overrides, exit behaviour).
2. The render specification — the JSON document that describes a composition.
3. The dynamic value system (literals, keyframes, and the expression DSL).
4. The built-in effect, transition, and shader catalogs.
5. The output formats and the debug-run layout.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **MAY**, and
**OPTIONAL** are used in the sense of RFC 2119.

Unless stated otherwise, every numeric field is a [Dynamic Value](#4-dynamic-values)
— it accepts a constant, a keyframe array, or an expression object
interchangeably.

---

## 1. System Overview

Render is a headless, GPU-accelerated engine. It reads a single declarative JSON
specification, resolves its assets, evaluates the timeline frame-by-frame on the
GPU (via WebGPU / `wgpu` compute pipelines), and writes a static image or an
H.264/AAC video.

```
  spec.json ─┐
             ├─▶ Render engine (wgpu) ─▶ output.png / output.mp4 ─▶ [local | S3 | GCS | signed URL]
  assets ────┘
```

---

## 2. Command-Line Interface

### 2.1 Synopsis

```
render [OPTIONS] <SPEC_PATH>
```

The binary is named `render`. Exactly one render specification is processed
per invocation.

### 2.2 Options

| Flag | Argument | Required | Description |
| :--- | :--- | :--- | :--- |
| `<SPEC_PATH>` | path | yes¹ | Positional path to the JSON spec. |
| `-i`, `--input` | path | yes¹ | Alternative to the positional argument. Takes precedence if both are given. |
| `--debug` | dir | no | Enables debug mode; writes a timestamped run folder under `dir` (see §6.2). |
| `-I`, `--include` | path | no | Extra directory or `.wgsl` file to scan for custom shaders. Repeatable. |
| `-o`, `--output` | string | no | Overrides the spec's `output` path. |
| `--width` | int | no | Overrides `composition.width`. |
| `--height` | int | no | Overrides `composition.height`. |
| `--fps` | int | no | Overrides `composition.fps`. |
| `--duration` | float | no | Overrides `composition.duration`. |
| `--set` | `key=value` | no | Overrides an arbitrary dotted spec path (e.g. `--set composition.fps=60`). Repeatable. |
| `--aws-key` | string | no | AWS access key id, used for `s3://` outputs. |
| `--aws-secret` | string | no | AWS secret access key. |
| `--gcs-key` | string | no | GCS HMAC access key, used for `gs://` outputs. |
| `--gcs-secret` | string | no | GCS HMAC secret. |

¹ A spec path **MUST** be supplied either positionally or via `-i/--input`.

### 2.3 Overrides

Overrides are applied to the raw JSON **before** deserialization, so they may
introduce or replace any field. `--set key=value` splits on the first `=`; the
key is a dot-separated path into the JSON object tree, and the value is coerced
to an integer, float, boolean, `null`, or string (in that order). Quoted values
(`"…"` or `'…'`) are always treated as strings. The convenience flags `-o`,
`--width`, `--height`, `--fps`, and `--duration` are shorthand for the
corresponding `--set` paths.

### 2.4 Exit Behaviour

| Code | Meaning |
| :--- | :--- |
| `0` | Render completed and all outputs (including any remote upload) succeeded. |
| `1` | A recoverable run error — most commonly a failed remote upload, or a missing spec path. |
| non-zero (abort) | An unrecoverable error during spec parsing, asset/font/shader loading, or GPU setup. These currently surface as a Rust panic rather than a graceful exit. |

> **Note.** Granular per-stage exit codes are not yet implemented. Resource and
> shader-compilation failures abort the process; only render-loop and upload
> failures return the clean `1` code.

### 2.5 Library & Asset Resolution

Render loads dynamic resources at runtime: the compositor shader
(`compositor.wgsl`), the built-in effect/transition shaders, and the bundled
fonts. It locates the `library/` directory by checking, in order:

1. **`RENDER_LIBRARY_PATH`** — if set and pointing at an existing directory, it
   is used directly.
2. **`./library`** — relative to the current working directory, if it contains
   `compositor.wgsl`.
3. **Executable-relative** — walking up parent directories from the binary's
   location until a `library/` containing `compositor.wgsl` is found.

If none match, the engine falls back to the literal path `library` and will
abort when the compositor shader cannot be read. Asset paths declared in the
spec are first tried as-is, then resolved relative to `library/` (with
`library/` and `fonts/` prefixes stripped as needed).

---

## 3. Render Specification (JSON)

### 3.1 Root Object

| Field | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `version` | string | yes | Schema version. Currently `"1.0"`. |
| `output` | string \| [Output](#32-output) | yes | Destination. A bare string is the path; an object additionally carries credentials. |
| `composition` | [Composition](#33-composition) | yes | Canvas dimensions and timing. |
| `assets` | map<string, [Asset](#34-assets)> | yes | Named external resources, keyed by id. |
| `tracks` | [Track](#35-tracks)[] | yes | Z-ordered visual layers (index `0` is the backmost). |
| `presets` | [Preset](#310-presets)[] | no | Reusable compound effects. |
| `audio_tracks` | [AudioTrack](#311-audio-tracks)[] | no | Parallel audio mix lanes. |

### 3.2 Output

`output` is either a string (the destination path) or an object:

| Field | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `path` | string | yes | Destination path or URL. |
| `credentials` | object | no | `{ "key", "secret", "region" }`, all optional. Used for `s3://` / `gs://` / `mux://` uploads; CLI credential flags act as fallbacks. For `mux://`, `key` is the Mux token ID and `secret` is the Mux token secret. |

The output **target** is chosen by the scheme of the path:

* No scheme — written to the local filesystem.
* `s3://bucket/key` — uploaded to S3 (credentials from `credentials`, then `--aws-*`).
* `gs://bucket/key` — uploaded to Google Cloud Storage (credentials from `credentials`, then `--gcs-*`).
* `http://…` / `https://…` — uploaded with an HTTP `PUT` (signed-URL style).
* `mux://` — uploaded to [Mux Video](https://mux.com) via the Direct Uploads API. The engine creates a one-time signed upload, `PUT`s the video to it, then prints the resulting Mux **asset ID** to stdout. It does not wait for the asset to finish processing — poll the asset (`GET /video/v1/assets/{id}`) yourself for `ready` status and playback IDs. Credentials come from `credentials` (`key` = token ID, `secret` = token secret), then `--mux-token-id` / `--mux-token-secret`, then `MUX_TOKEN_ID` / `MUX_TOKEN_SECRET`. The path after `mux://` is ignored — Mux assigns the asset ID.

The output **format** is chosen by the path's file extension, ignoring any query
string: `.mp4` produces a video; any other extension (`.png`, `.jpg`) produces a
single still rendered at timeline `t = 0`. The `mux://` scheme always implies a
video (Mux does not ingest stills).

### 3.3 Composition

| Field | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `width` | int | yes | Canvas width in pixels. |
| `height` | int | yes | Canvas height in pixels. |
| `fps` | int | yes | Frames per second (used for video output and frame indexing). |
| `duration` | float | yes | Total composition length in seconds. |

### 3.4 Assets

`assets` maps a unique id to an asset definition. The `type` field is the
discriminator.

| `type` | Fields | Notes |
| :--- | :--- | :--- |
| `image` | `path` | Still image. |
| `video` | `path` | Currently sampled as a still image for the visual track; its audio is usable via audio tracks. |
| `audio` | `path` | Audio source for audio tracks. |
| `shader` | `path` | A `.wgsl` file compiled into a custom effect/transition pipeline keyed by the asset id. |
| `font` | `provider`, `path` | `provider` is one of `"file"`, `"system"`, `"url"`. |

Any `path` beginning with `http://` or `https://` is downloaded and cached to a
temporary file before use, for every asset type.

### 3.5 Tracks

A track is a Z-ordered layer holding a sequence of clips and optional
transitions.

| Field | Type | Required | Default | Description |
| :--- | :--- | :--- | :--- | :--- |
| `id` | string | yes | — | Unique track id. |
| `start` | float | no | `0.0` | Time offset (seconds) at which the track's first clip begins. |
| `clips` | [Clip](#36-clips)[] | yes | — | Sequential clips. |
| `transitions` | [Transition](#39-transitions)[] | no | `[]` | Transitions between clips on this track. |

Clips on a single track are laid out **sequentially** and **MUST NOT** overlap.
Each clip's absolute start is `track.start + Σ(previous durations) + Σ(offsets)`.
To overlay content, place it on separate, higher-indexed tracks.

### 3.6 Clips

| Field | Type | Required | Default | Description |
| :--- | :--- | :--- | :--- | :--- |
| `id` | string | yes | — | Unique clip id. |
| `type` | enum | yes | — | `media`, `solid`, `text`, or `effect`. |
| `duration` | float | yes | — | On-screen duration in seconds. |
| `offset` | float | no | `0.0` | Non-negative gap before this clip starts, relative to the previous clip's end. |
| `trim_start` | float | no | `0.0` | In-point within the source media, in seconds. |
| `asset` | string | for `media` | — | Asset id to render. |
| `scale_mode` | enum | no | `"fit"` | `fit`, `fill`, `natural`, or `stretch` (see §3.7). |
| `solid_params` | object | for `solid` | — | `{ "color": [r,g,b,a] }`, each component `0.0–1.0`. |
| `text_params` | [TextParams](#312-text) | for `text` | — | Text content / layout. |
| `transform` | [Transform](#313-transform) | no | — | Position, scale, rotation, opacity. |
| `effects` | [Effect](#38-effects)[] | no | `[]` | Ordered post-processing chain. |
| `blend_mode` | enum | no | `"normal"` | Compositing mode (see §3.7). |
| `shader` | string | no | — | Custom shader asset id overriding the default renderer. |
| `preset` | string | no | — | Preset name (for `effect`-type adjustment clips). |
| `params` | map | no | — | Parameters for the custom `shader` / `preset`. |

### 3.7 Enumerations

**`scale_mode`** (media scaling):

| Value | Behaviour |
| :--- | :--- |
| `fit` | Uniform scale to fit inside the canvas (letterboxed). |
| `fill` | Uniform scale to cover the canvas (cropped). |
| `natural` | Source resolution, centered, no scaling. |
| `stretch` | Non-uniform scale to exactly fill the canvas. |

**`blend_mode`** (Photoshop-style): `normal`, `multiply`, `screen`, `overlay`,
`darken`, `lighten`, `color_dodge`, `color_burn`, `hard_light`, `soft_light`,
`difference`, `exclusion`.

### 3.8 Effects

Each entry in a clip's `effects` array applies a filter to the clip in order.

| Field | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `type` | string | yes | Effect id — a built-in (§5), a custom shader file stem, or `"preset"`. |
| `shader` | string | no | Shader asset id, when overriding by asset rather than `type`. |
| `preset` | string | no | Preset name, when `type` is `"preset"`. |
| `params` | map<string, [Dynamic Value](#4-dynamic-values)> | no | Effect parameters. |

There are two classes of effect:

* **Registered effects** (§5) carry an `EFFECTS_METADATA` block in their WGSL
  source. Their parameters are named, typed, and have documented defaults.
* **Custom-shader effects** (any other `.wgsl` in `library/effects/` or an
  `--include` path) receive their parameters through the
  [custom-parameter packing contract](#7-custom-shader-parameter-contract).

### 3.9 Transitions

A transition cross-blends two adjacent clips on a track using a shader.

| Field | Type | Required | Default | Description |
| :--- | :--- | :--- | :--- | :--- |
| `id` | string | yes | — | Unique transition id. |
| `type` | string | yes | — | Transition shader id (e.g. `"fade"`), or `"custom_shader"` paired with `shader`. |
| `shader` | string | no | — | Explicit shader asset id. |
| `from` | string | yes | — | Outgoing clip id. |
| `to` | string | yes | — | Incoming clip id. |
| `duration` | float | yes | — | Overlap length in seconds. |
| `start` | float | no | midpoint¹ | Absolute timeline start. |
| `params` | map<string, [Dynamic Value](#4-dynamic-values)> | no | — | Shader uniforms. |

¹ Defaults to `to`'s start minus half the transition duration, so the transition
straddles the cut. The built-in transition catalog is in §5.2.

### 3.10 Presets

A preset is a named, parameterised group of effects. Referencing a preset
expands into its filters with the caller's arguments substituted.

| Field | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `name` | string | yes | Preset id, referenced by an effect's `preset` field. |
| `inputs` | object[] | yes | Declared parameters: `{ "name", "type", "defaultValue" }`. `type` is `float`, `vec2`, or `string`. |
| `filters` | [Effect](#38-effects)[] | yes | Effects to expand. Use `$name` in any string/param value to substitute an input. |

Presets may reference other presets; expansion is bounded to a depth of 5.

### 3.11 Audio Tracks

| Field | Type | Required | Default | Description |
| :--- | :--- | :--- | :--- | :--- |
| `id` | string | yes | — | Unique track id. |
| `start` | float | no | `0.0` | Track start offset in seconds. |
| `clips` | object[] | yes | — | Sequential audio clips. |

Each audio clip:

| Field | Type | Required | Default | Description |
| :--- | :--- | :--- | :--- | :--- |
| `id` | string | yes | — | Unique clip id. |
| `asset` | string | yes | — | Id of an `audio` or `video` asset. |
| `duration` | float | yes | — | Playback length in seconds. |
| `offset` | float | no | `0.0` | Gap before this clip, relative to the previous clip's end. |
| `trim_start` | float | no | `0.0` | In-point within the source audio, in seconds. |

Audio clips are mixed at unity gain (no automatic attenuation) and muxed with
the video via FFmpeg.

### 3.12 Text

`text_params` supports two modes, selected by `kind`.

**Simple mode** (default — `kind` omitted):

| Field | Type | Description |
| :--- | :--- | :--- |
| `text` | string | The string to render. |
| `font` | string | Font asset id. |
| `font_size` | [Dynamic Value](#4-dynamic-values) | Type size in pixels. |
| `color` | `[r,g,b,a]` | Text colour, `0.0–1.0`. |
| `axes` | map<string, [Dynamic Value](#4-dynamic-values)> | OpenType variation axes (e.g. `"wght"`, `"wdth"`, `"slnt"`). |

**Layout mode** (`kind: "layout"`): set `body` to a [LayoutNode](#312a-layout-nodes)
tree.

Both modes accept optional `entrance` and `exit`
[text transitions](#312b-text-transitions).

#### 3.12a Layout Nodes

A layout node arranges text and nested nodes using stack semantics.

| Field | Type | Description |
| :--- | :--- | :--- |
| `type` (alias `kind`) | string | `vstack`, `hstack`, `zstack`, `spacer`, or `text`. |
| `children` | LayoutNode[] | Child nodes (for stacks). |
| `spacing` | Dynamic | Gap between children. |
| `alignment` | string | `left`, `center`, or `right`. |
| `padding` | Dynamic | `[top,right,bottom,left]`, `[v,h]`, or a scalar. |
| `size` | Dynamic | Fixed extent for a `spacer`. |
| `text` | Dynamic (string) | Content for a `text` node. |
| `font` | string | Font asset id. |
| `font_size` | Dynamic | Type size. |
| `color` | Dynamic | `[r,g,b,a]`. |
| `axes` | map<string, Dynamic> | Variation axes. |

#### 3.12b Text Transitions

`entrance` and `exit` animate glyphs in/out, staggered per element.

| Field | Type | Required | Default | Description |
| :--- | :--- | :--- | :--- | :--- |
| `type` | string | yes | — | Transition label. |
| `granularity` | string | no | `"letter"` | Stagger unit: `letter`/`character`, `word`, or `line`. |
| `delay` | float | no | `0.0` | Per-element stagger in seconds (element index × delay). |
| `duration` | float | yes | — | Per-element animation length. |
| `easing` | string | no | `"linear"` | Easing curve (§4.2). |
| `start_transform` | object | no | — | The offset state animated from (entrance) / to (exit). |

`start_transform` fields: `position_offset` `[dx,dy]`, `scale` (scalar or
`[sx,sy]`), `rotation` (degrees), `opacity` (`0.0–1.0`). At progress `0` a glyph
sits fully at `start_transform`; at progress `1` it sits at its natural layout
position with full opacity.

### 3.13 Transform

All transform fields are [Dynamic Values](#4-dynamic-values).

| Field | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `position` | vec2 | canvas center | `[x, y]` in pixels. |
| `scale` | vec2 \| scalar | `1.0` | `[sx, sy]` or a uniform multiplier. |
| `rotation` | float | `0.0` | Degrees. |
| `opacity` | float | `1.0` | `0.0–1.0`. |

---

## 4. Dynamic Values

Every scalar, vector, and colour property accepts three interchangeable forms.

### 4.1 Forms

**A — Constant**

```json
"scale": 1.2,
"position": [400.0, 400.0],
"color": [1.0, 1.0, 1.0, 1.0]
```

**B — Keyframe array** — interpolated by `clip_time`:

```json
"opacity": [
  { "time": 0.0, "value": 0.0, "easing": "ease_out" },
  { "time": 1.5, "value": 1.0 }
]
```

* `time` — seconds relative to the clip start.
* `value` — a scalar or a vector matching the property's arity.
* `easing` — optional, on the **segment-start** keyframe (§4.2).

Keyframes are sorted by `time` before evaluation. Before the first keyframe the
first value holds; after the last, the last value holds.

**C — Expression** — evaluated per frame:

```json
"position": { "expression": "[comp.width * 0.5 + 50.0 * sin(clip_time * 2.0), comp.height * 0.5]" }
```

For vector properties, the expression string is a bracketed, comma-separated
list `"[exprX, exprY]"`; each component is evaluated independently (commas inside
nested function calls are respected).

### 4.2 Easing

`easing` is either a named curve or a 4-element cubic-bézier control array
`[x1, y1, x2, y2]` (CSS `cubic-bezier` semantics, fixed endpoints `(0,0)`/`(1,1)`).

| Name | Curve |
| :--- | :--- |
| `linear` | identity (also the fallback for unknown names) |
| `ease_in` | `t²` |
| `ease_out` | `t·(2−t)` |
| `ease_in_out` | `t²·(3−2t)` (smoothstep) |

### 4.3 Expression DSL

Expressions are evaluated with [`evalexpr`](https://crates.io/crates/evalexpr).

**Operators:** `+`, `-`, `*`, `/`, `%`, plus the comparison/logical operators
provided by `evalexpr`.

**Constant:** `pi`.

**Registered functions:** `sin(x)`, `cos(x)`, `tan(x)`, `abs(x)`, `sqrt(x)`,
`pow(x, y)`, `min(x, y)`, `max(x, y)`, `clamp(x, lo, hi)`. Use `pow(x, y)` for
exponentiation.

**Context variables** (read-only):

| Variable | Aliases | Description |
| :--- | :--- | :--- |
| `time` | — | Absolute timeline position (seconds). |
| `clip_time` | `clip.time` | Position relative to the clip start (seconds). |
| `clip_duration` | `clip.duration` | Duration of the enclosing clip (seconds). |
| `comp_width` | `comp.width` | Canvas width (pixels). |
| `comp_height` | `comp.height` | Canvas height (pixels). |

Dotted aliases (`comp.width`, `clip.duration`, …) are accepted and normalised to
their underscore forms. If an expression fails to compile or evaluate, the
property falls back to its declared default.

---

## 5. Built-in Catalogs

### 5.1 Registered Effects

These ship with the engine and are referenced by `type` under a clip's
`effects`. Defaults apply when a parameter is omitted.

| `type` | Parameters (default) | Description |
| :--- | :--- | :--- |
| `grayscale` | `enabled` (1.0) | Luminance conversion; `>0.5` is on. |
| `brightness` | `factor` (1.0) | Exposure multiplier. |
| `contrast` | `factor` (1.0) | Contrast multiplier. |
| `saturation` | `factor` (1.0) | Saturation multiplier. |
| `hue_rotate` | `angle` (0.0) | Hue shift in degrees. |
| `blur` | `radius` (0.0) | Gaussian blur radius. |
| `glow` | `intensity` (0.0), `radius` (0.0), `threshold` (0.5) | Bloom from bright pixels above `threshold`. |
| `film_grain` | `amount` (0.0), `speed` (1.0) | Animated noise. |
| `film_flicker` | `amount` (0.0), `speed` (1.0) | Brightness flicker. |
| `depth_blur` | `focus_x` (0.5), `focus_y` (0.5), `focus_radius` (0.2), `near_blur` (0.0), `far_blur` (0.0), `depth_map` | Depth-of-field; `depth_map` is an optional asset id. |
| `flow` | `amount` (0.0), `speed` (1.0), `decay` (0.95) | Feedback-buffer flow displacement. |

`grayscale` and `brightness` are also evaluated in the compositor pass; the
remaining registered effects run as post-processing passes (and contribute
debug spot-checks only for video output).

### 5.2 Built-in Transitions

Referenced from a track's `transitions` by `type` (or `type: "custom_shader"`
with a matching `shader` id).

| `type` | Parameters (default) | Description |
| :--- | :--- | :--- |
| `fade` | — | Linear opacity cross-fade. |
| `wipe` | `direction` | Directional wipe. |
| `flash_burn` | `flash_intensity` (0.8), `burn_intensity` (1.0) | Exposure flash plus an expanding film burn-in revealing the incoming clip. |
| `focus_in` | `max_blur` (20.0) | Defocus-then-refocus with a breathing zoom. |
| `slide_switch` | `direction` (0.0), `gap_size` (0.1), `flicker_intensity` (0.15), `z_padding` | 35 mm projector slide swap with motion blur, a black mount gap, a spring snap, and lamp flicker. `direction` `0.0` = horizontal, `1.0` = vertical. |

### 5.3 Custom Shaders

Additional WGSL effects ship in `library/effects/` without an `EFFECTS_METADATA`
block (e.g. `bloom`, `chromatic_aberration`, `circuit_bent`, `crt`,
`directional_blur`, `displacement_map`, `dithering`, `edge_detect`,
`fluted_glass`, `halftone`, `ink`, `magnify_lens`, `pixelation`, `posterize`,
`slice`, `smear`, `threshold`, `voxel`). Reference them by file stem as an
effect `type` (or as a `shader` asset), and pass parameters per the contract in
§7. You may add your own via `--include`.

---

## 6. Output & Verification

### 6.1 Formats

* **Still** (`.png`, `.jpg`, …) — the single frame at `t = 0`, written with
  straight alpha.
* **Video** (`.mp4`) — every frame piped to FFmpeg and encoded as H.264
  (`yuv420p`) with AAC audio. Semi-transparent pixels are flattened onto black
  before encoding.

Remote destinations (§3.2) render to a temporary file first, then upload.

### 6.2 Debug Run Layout

With `--debug <dir>`, the engine creates a unique run folder
`<specname>_render_NNNN/`:

```
<specname>_render_0001/
├── logs.txt        # full execution trace (also echoed to stdout)
├── review.json     # spot-check frame index → {timestamp, file, explanation}
└── frames/
    ├── frame_0000.png
    ├── frame_0030.png
    └── …            # frames at clip boundaries, transitions, and effect spot-checks
```

`review.json` is an array of `{ "frame", "timestamp", "file", "explanation" }`
objects, one per spot-checked frame.

---

## 7. Custom-Shader Parameter Contract

Custom shaders (and any effect without registered metadata) receive their
`params` map packed into a single uniform buffer. The packing is **positional,
not name-matched**, so authors **MUST** observe this contract:

1. **Ordering.** Parameters are sorted **alphabetically by key**, then written
   in that order. A shader's `CustomParams` struct fields **MUST** be declared
   in the same alphabetical order.
2. **Type inference** from the JSON value:
   * number with a fractional part / expression / general number → `f32`;
   * boolean → `u32` (`0` or `1`);
   * integer → `i32`;
   * 2-element numeric array or bracketed `"[…, …]"` expression → `vec2<f32>`
     (8-byte aligned per std140).
   * Other types (strings, longer arrays, objects) are skipped.
3. **Alignment.** The buffer is zero-padded to a 16-byte multiple. An empty map
   yields a 16-byte zero buffer so the binding is never zero-length.

Every custom shader also receives a standard `EngineParams` uniform at
`@binding(2)`: `{ time, clip_time, progress, width, height }` (transitions add a
second input texture and use `TransitionEngineParams`).

---

## 8. Reference Example

```json
{
  "version": "1.0",
  "output": "outputs/voyage.mp4",
  "composition": { "width": 1920, "height": 1080, "fps": 30, "duration": 15.0 },
  "assets": {
    "ocean_video":    { "type": "video", "path": "assets/ocean.mp4" },
    "watermark_logo": { "type": "image", "path": "assets/logo.png" },
    "title_font":     { "type": "font", "provider": "file", "path": "library/fonts/Inter-VF.ttf" }
  },
  "presets": [
    {
      "name": "Dreamy Glow",
      "inputs": [
        { "name": "glow_intensity", "type": "float", "defaultValue": 1.0 }
      ],
      "filters": [
        { "type": "glow", "params": { "intensity": "$glow_intensity", "radius": 4.0, "threshold": 0.4 } }
      ]
    }
  ],
  "tracks": [
    {
      "id": "background",
      "clips": [
        {
          "id": "bg",
          "type": "media",
          "asset": "ocean_video",
          "duration": 15.0,
          "scale_mode": "fill",
          "effects": [ { "type": "Dreamy Glow", "preset": "Dreamy Glow" } ]
        }
      ]
    },
    {
      "id": "overlay",
      "start": 2.0,
      "clips": [
        {
          "id": "watermark",
          "type": "media",
          "asset": "watermark_logo",
          "duration": 10.0,
          "scale_mode": "fit",
          "blend_mode": "screen",
          "transform": {
            "position": { "expression": "[comp.width * 0.85, comp.height * 0.15]" },
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
      "id": "title",
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
                  "font": "title_font",
                  "font_size": 48.0,
                  "color": [1.0, 1.0, 1.0, 1.0],
                  "axes": { "wght": { "expression": "300.0 + 400.0 * (0.5 + 0.5 * sin(clip_time * 2.0))" } }
                }
              ]
            },
            "entrance": {
              "type": "rise",
              "granularity": "letter",
              "delay": 0.05,
              "duration": 0.6,
              "easing": "ease_out",
              "start_transform": { "position_offset": [0.0, 40.0], "opacity": 0.0 }
            }
          }
        }
      ]
    }
  ]
}
```
