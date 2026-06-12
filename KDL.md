# KDL Spec Front-End

`render` accepts two input formats interchangeably:

- **`*.json`** — the canonical spec described in [SPEC.md](./SPEC.md).
- **`*.kdl`** — a readable [KDL](https://kdl.dev) front-end that transpiles to
  the exact same spec before rendering.

The format is chosen by file extension. KDL is **one-way**: it compiles to spec
JSON; there is no JSON→KDL conversion. Everything in SPEC.md (overrides,
keyframes, expressions, the effect/transition catalogs) applies unchanged — KDL
only changes how you *write* the document.

```sh
render timeline.kdl                 # render a KDL spec
render timeline.kdl --emit-json     # print the transpiled spec and exit
```

`--emit-json` is the source of truth: when in doubt about how something maps,
emit it and read the JSON.

## Why KDL

A render spec is a tree of typed nodes with attributes — exactly what KDL
models. The mapping is mechanical:

| KDL | JSON |
| :--- | :--- |
| node **name** | the `type` discriminator (and removes layout `children` wrappers) |
| first positional **string** on a clip/transition | its `id` |
| first positional **number** on a clip | its `duration` (or use `dur=`) |
| `key=value` property (scalar/string/bool) | that field |
| **child node** with positional args | a multi-component value (vector / padding) |
| `(expr)`-annotated value | `{ "expression": "…" }` |
| `"#rgb"` / `"#rrggbb"` / `"#rrggbbaa"` on a color | `[r,g,b,a]` floats in 0..1 |
| `// …` | comment (dropped) |

> **Numbers are unitless.** KDL has no unit suffixes, so durations and times are
> plain seconds: `solid "a" 2.0`, `fade … at=1.5 dur=1.0`.

## Document structure

```kdl
composition width=800 height=800 fps=30 duration=8.0   // or: comp 800 800 30 8.0
output "out.mp4"                                        // or an { } block, below

image "bg"  path="bg.jpg"                               // asset; node name = type
font  "ui"  path="Inter.ttf"
// also: video, audio, shader, lut  — or  asset "x" type="image" path="…"

track "main" {
    // clips (sequential) and transitions live here
}
```

`version` defaults to `"1.0"`; add `version "1.0"` to override. `composition`
and `output` are required.

### Output with credentials

```kdl
output {
    path "s3://bucket/out.mp4"
    credentials key="AKIA…" secret="…" region="us-east-1"
}
```

### Output with encode settings

```kdl
output "out.mp4" {
    encode crf=28 preset="slow" max_bitrate="12M"
}
```

All three properties are optional and tune the H.264 encode (defaults: CRF 23,
preset `medium`, no bitrate cap); see SPEC.md §3.2 for the value ranges. Use
them to cap file size when per-frame noise (e.g. `film_grain`) would otherwise
balloon the output. A bare path argument and a `{ }` block combine, as shown;
`credentials` can live in the same block.

## Clips

Inside a `track`, the four clip node names are `media`, `solid`, `text`,
`effect`; any other node name in a track is a **transition** (its name is the
transition type).

```kdl
track "main" {
    media "bg" 10.0 asset="ocean" scale_mode="fill" blend_mode="screen"
    solid "card" 2.0 color="#1E1E2E"
    text  "title" 5.0 text="Hello" font="ui" size=72 color="#FFFFFF"
}
```

Common clip properties: `dur=`/`duration=`, `asset=`, `offset=`, `trim_start=`,
`scale_mode=`, `blend_mode=`, `shader=`, `preset=`. Unrecognized properties on a
clip become `params` entries (for custom shaders / presets).

### Transform, effects, params (clip children)

```kdl
media "logo" 10.0 asset="logo" {
    transform {
        position (expr)"[comp.width*0.85, comp.height*0.15]"
        scale 0.15
        opacity {
            key 0.0  0.0 ease="ease_out"
            key 1.0  0.8
            key 9.0  0.8 ease="ease_in"
            key 10.0 0.0
        }
    }
    fx {
        grayscale
        brightness factor=1.3
        glow intensity=1.0 radius=4.0 threshold=0.4
    }
}
```

- **`transform`** — each child node is a dynamic value (see below).
- **`fx`** (alias `effects`) — each child node is one effect; its name is the
  effect `type`, its properties are `params` (except `shader=` / `preset=`).
- **`params { … }`** — extra shader/preset params as child nodes (for vectors
  or keyframes).

## Transitions

The node name is the transition `type`. An explicit id is optional (a string
first arg); otherwise one is generated as `<type>_<index>`.

```kdl
track "main" {
    solid "a" 2.0 color="#CC3333"
    solid "b" 2.0 color="#33CC33"

    fade from="a" to="b" at=1.5 dur=1.0
    slide_switch "swap" from="a" to="b" at=3.5 dur=1.0 direction=1.0 gap_size=0.12
}
```

`at=` → `start`, `dur=`/`duration=` → `duration`. `from`/`to` are required.
Scalar params are inline properties; vector/keyframe params go in a child
`params { … }` block.

## Text

A `text` clip is **simple** unless it contains a stack child (`vstack` /
`hstack` / `zstack`), in which case it is **layout** mode (`kind:"layout"`).

```kdl
// simple
text "t" 5.0 text="Ocean Voyage" font="ui" size=48 color="#FFFFFF" {
    axes wght=600
}

// layout
text "t" 5.0 {
    enter use="rise"                 // or inline, see below
    vstack spacing=10 align="center" {
        padding 100 60 100 60        // vector → child node
        text "Title" font="ui" size=72 color="#F2E633"
        spacer 60                    // fixed extent; bare `spacer` is flexible
        text "Subtitle" font="ui" size=44 color="#FFFFFFE6"
    }
}
```

Layout node types: `vstack`, `hstack`, `zstack`, `text`, `spacer`. A layout
`text` node takes its content as the first string arg; `size=` is the font size.

### Entrance / exit

```kdl
enter "rise" granularity="letter" delay=0.05 dur=0.6 easing="ease_out" {
    from opacity=0.0 scale=0.8 rotation=10 {
        offset 0 40              // position_offset vector
    }
}
exit "fade" dur=0.4 { from opacity=0.0 }
```

`from` (alias `start_transform`): `opacity=`, `scale=`, `rotation=` as
properties; `offset` (alias `position_offset`) as a child node.

## Dynamic values

Anywhere SPEC.md allows a dynamic value (transform fields, font size, axes,
spacing, effect/transition params), KDL accepts three forms:

| Form | KDL | JSON |
| :--- | :--- | :--- |
| constant scalar | `scale=1.2` or `scale 1.2` | `1.2` |
| constant vector | `position 400 400` | `[400, 400]` |
| expression | `opacity=(expr)"clip_time*2"` / `position (expr)"[…, …]"` | `{"expression": …}` |
| keyframes | `opacity { key 0.0 0.0 ease="ease_out"; key 1.5 1.0 }` | `[{time,value,easing?}]` |

`key <time> <value…>` — a single value arg is a scalar, multiple are a vector;
`ease=` (alias `easing=`) sets the segment-start easing.

## Reuse: `def` / `use`

`def "name" { <one node> }` defines a reusable block. Any node with
`use="name"` inherits it; the node's own properties override (deep merge).

```kdl
def "reveal_in" {
    enter "reveal" granularity="letter" dur=0.2 easing="linear" {
        from opacity=0.0
    }
}

track "t" {
    text "a" 2.0 { enter use="reveal_in";              vstack { /* … */ } }
    text "b" 2.0 { enter use="reveal_in" delay=0.05;   vstack { /* … */ } }
}
```

## Audio & presets

```kdl
audio "music" path="track.mp3"          // asset

audio_track "bgm" start=0.0 {
    clip "m1" asset="music" dur=8.0 trim_start=2.0
}

preset "Dreamy Glow" {
    input "glow_intensity" type="float" default=1.0
    glow intensity="$glow_intensity" radius=4.0 threshold=0.4
}
```

## Worked examples

- [`tests/fixtures/17_dynamic_transitions_kdl.kdl`](./tests/fixtures/17_dynamic_transitions_kdl.kdl)
  — solids + transitions (port of `10_dynamic_transitions.json`).
- [`tests/fixtures/16_layout_kdl.kdl`](./tests/fixtures/16_layout_kdl.kdl)
  — nested layout, `def`/`use` reuse, and an expression axis.
