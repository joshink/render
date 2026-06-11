---
name: render
description: Use the Render engine — a headless GPU video compositor that turns one JSON/KDL spec into a PNG or MP4. Use when authoring render specs, producing videos/stills with render, or debugging render output. For modifying the engine itself, see the "Touching engine code?" section.
---

# Render (headless GPU video compositor)

One declarative spec (JSON or KDL, chosen by file extension) → a PNG still or H.264/AAC MP4. Everything (compositing, effects, text, transitions) runs on the GPU via wgpu/WGSL. Binary name: `render`.

```bash
cargo run --release -- spec.json                  # render
cargo run --release -- timeline.kdl --emit-json   # show what KDL compiles to, don't render
cargo test --release                              # progressive test suite
```

Normative schema: `SPEC.md`. KDL front-end: `KDL.md`. Example specs: `tests/fixtures/`.

## Minimal spec

```json
{
  "version": "1.0",
  "output": "out.mp4",
  "composition": { "width": 1280, "height": 720, "fps": 30, "duration": 4.0 },
  "assets": { "bg": { "type": "image", "path": "input.jpg" } },
  "tracks": [
    { "id": "bg", "clips": [
      { "id": "bg1", "type": "media", "asset": "bg", "duration": 4.0, "scale_mode": "fill",
        "effects": [ { "type": "glow", "params": { "intensity": 1.0, "radius": 4.0 } } ] }
    ]},
    { "id": "fg", "clips": [
      { "id": "title", "type": "text", "duration": 4.0,
        "text_params": { "text": "HELLO", "font": "title", "font_size": 72, "color": [1,1,1,1] },
        "transform": { "opacity": [ { "time": 0.0, "value": 0.0, "easing": "ease_out" },
                                    { "time": 1.0, "value": 1.0 } ] } }
    ]}
  ]
}
```

(A `text` clip needs a `font` asset, e.g. `"title": { "type": "font", "provider": "file", "path": "fonts/Inter-VF.ttf" }` — the bundled font lives in `library/fonts/`.)

Clip types: `media`, `solid`, `text`, `effect`. Any numeric property accepts a constant, a keyframe array, or `{ "expression": "..." }` with vars `time`, `clip_time`, `clip.duration`, `comp.width`, `comp.height` and fns `sin cos tan abs sqrt pow min max clamp`.

## Iterating

- **Render a still first**: any non-`.mp4` extension renders only the frame at `t = 0` — fast feedback, but it will NOT show anything that starts later. Use `--set composition.duration=…` / `--fps` / `-o` to vary without editing the spec.
- **`--debug ./debug`** writes `logs.txt`, `review.json`, and spot-check PNGs at clip boundaries/transitions — the main way to verify motion without watching the MP4.
- KDL is one-way (KDL→JSON only); when KDL output surprises you, run `--emit-json` and read the JSON.

## Gotchas

- **Run from the repo root** (or set `RENDER_LIBRARY_PATH` to the absolute path of `library/`). Otherwise the compositor shader/fonts aren't found and the process aborts.
- **MP4 needs `ffmpeg` on PATH.** A failed encode fails the render.
- **Clips on one track are strictly sequential** — start times are computed (`track.start + Σdurations + Σoffsets`), there is no per-clip `start`. To overlap content, use more tracks; track index 0 is the backmost.
- **Transitions only cross-blend adjacent clips on the same track** (`from`/`to` ids). `start` defaults to straddling the cut.
- **`video` assets render as a still image** on visual tracks — real video decoding isn't implemented. Their audio works fine via `audio_tracks`.
- **Bad expressions fail silently**: a property whose expression doesn't compile/evaluate falls back to its default with no error. Use `pow(x,y)` (no `^` operator). If something sits at canvas center / opacity 1, suspect a broken expression.
- **Keyframe `time` is relative to the clip start**, not the timeline.
- **Several effects default to off**: `glow`, `film_grain`, `blur`, `flow` have 0.0 default intensity/radius — adding the effect without `params` does nothing.
- **Colors are 0.0–1.0 floats** (`[r,g,b,a]`), not 0–255. Rotation is degrees. Position default is canvas center.
- **Custom WGSL shader params are packed alphabetically by key, positionally** — the shader's `CustomParams` struct fields must be declared in the same alphabetical order, or values land in the wrong fields with no warning. Strings/3+-element arrays in `params` are silently skipped. (Contract: SPEC.md §7.)
- **Audio mixes at unity gain** — no automatic attenuation; overlapping loud clips can clip.
- **Load errors panic** (spec parse, missing asset/font/shader, GPU setup) rather than exiting gracefully; exit code 1 means a render-loop or upload failure.

## Server mode

```bash
render serve --port 8080 --concurrency 1
curl -s -X POST localhost:8080/render -H 'Content-Type: application/json' --data @spec.json
# → {"id":…,"status_url":…,"events_url":…}; stream progress: curl -N localhost:8080/render/<id>/events
```

- Specs with **local output paths are rejected (422)** — server mode requires `s3://`, `gs://`, `mux://`, or a signed `https://` PUT URL.
- `POST /render` returns 202 immediately; poll `status_url` or stream SSE from `events_url` until `done`/`failed`. Job state is **evicted 15 minutes** after completion (then 404).
- `mux://` output reports the Mux asset ID and does not wait for processing.

## Touching engine code?

Two constraints break naively-written GPU code here: 256-byte row-pitch alignment when reading textures back to CPU (rows must be unpadded), and std140 16-byte uniform alignment (`#[repr(C)]` + explicit padding + bytemuck derives).
