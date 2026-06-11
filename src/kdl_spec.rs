//! KDL front-end for render specs.
//!
//! Transpiles a `.kdl` document into the same `serde_json::Value` the engine
//! would otherwise read from a `.json` spec, so the rest of the pipeline
//! (overrides, keyframe sorting, deserialization) is untouched. This is a
//! one-way source format: KDL in, spec JSON out.
//!
//! ## Conventions
//!
//! - A node's **name** is the discriminator (`solid`/`text`/`fade`/`vstack`…),
//!   absorbing the JSON `"type"` field and layout `children` wrappers.
//! - The first positional **string** on a clip/transition is its `id`; the
//!   first positional **number** on a clip is its `duration` (or use `dur=`).
//! - `key=value` properties (scalars, strings, bools) map to that field.
//! - Multi-component values (vectors, padding, keyframes) are **child nodes**,
//!   because KDL properties cannot hold arrays.
//! - A `"#rgb"`/`"#rrggbb"`/`"#rrggbbaa"` string on a color field expands to
//!   `[r,g,b,a]` floats in 0..1.
//! - A `(expr)`-annotated value becomes `{ "expression": "…" }`.
//! - `def "name" { <node> }` defines a reusable block; any node with
//!   `use="name"` inherits it, with its own properties overriding.
//!
//! See `SPEC.md` §3 for the target JSON schema and `tests/fixtures/*.kdl` for
//! worked examples.

use std::collections::HashMap;

use kdl::{KdlDocument, KdlNode, KdlValue};
use serde_json::{Map, Value};

/// Parse a KDL spec document into the engine's spec JSON value.
pub fn kdl_to_spec_json(src: &str) -> Result<Value, String> {
    let doc = KdlDocument::parse(src).map_err(|e| format!("KDL parse error:\n{e}"))?;

    // First pass: collect reusable `def` blocks so they can be referenced from
    // anywhere, regardless of declaration order.
    let mut defs: HashMap<String, KdlNode> = HashMap::new();
    for node in doc.nodes() {
        if node.name().value() == "def" {
            let name = first_string_arg(node)
                .ok_or_else(|| "`def` requires a name: `def \"my_block\" { … }`".to_string())?;
            let children = node.children().map(|d| d.nodes()).unwrap_or(&[]);
            if children.len() != 1 {
                return Err(format!(
                    "`def \"{name}\"` must contain exactly one node, found {}",
                    children.len()
                ));
            }
            defs.insert(name.to_string(), children[0].clone());
        }
    }

    let mut root = Map::new();
    root.insert("version".into(), Value::String("1.0".into()));

    let mut assets = Map::new();
    let mut tracks: Vec<Value> = Vec::new();
    let mut audio_tracks: Vec<Value> = Vec::new();
    let mut presets: Vec<Value> = Vec::new();

    for node in doc.nodes() {
        match node.name().value() {
            "def" => {} // handled above
            "version" => {
                if let Some(s) = first_string_arg(node) {
                    root.insert("version".into(), Value::String(s.to_string()));
                }
            }
            "composition" | "comp" => {
                root.insert("composition".into(), build_composition(node)?);
            }
            "output" => {
                root.insert("output".into(), build_output(node)?);
            }
            "image" | "video" | "audio" | "shader" | "lut" | "font" | "asset" => {
                let (id, asset) = build_asset(node)?;
                assets.insert(id, asset);
            }
            "track" => tracks.push(build_track(node, &defs)?),
            "audio_track" => audio_tracks.push(build_audio_track(node)?),
            "preset" => presets.push(build_preset(node, &defs)?),
            other => {
                return Err(format!(
                    "unknown top-level node `{other}` (expected composition, output, \
                     image/video/audio/font/shader/lut, track, audio_track, preset, or def)"
                ))
            }
        }
    }

    if !root.contains_key("composition") {
        return Err("missing required `composition` node".into());
    }
    if !root.contains_key("output") {
        return Err("missing required `output` node".into());
    }

    root.insert("assets".into(), Value::Object(assets));
    root.insert("tracks".into(), Value::Array(tracks));
    if !audio_tracks.is_empty() {
        root.insert("audio_tracks".into(), Value::Array(audio_tracks));
    }
    if !presets.is_empty() {
        root.insert("presets".into(), Value::Array(presets));
    }

    Ok(Value::Object(root))
}

// ── Top-level builders ───────────────────────────────────────────────────

fn build_composition(node: &KdlNode) -> Result<Value, String> {
    let mut m = Map::new();
    // Allow either `composition width=800 height=800 fps=30 duration=8.0`
    // or positional `comp 800 800 30 8.0`.
    let args = positional_args(node);
    if args.len() >= 4 {
        m.insert("width".into(), value_to_json(args[0]));
        m.insert("height".into(), value_to_json(args[1]));
        m.insert("fps".into(), value_to_json(args[2]));
        m.insert("duration".into(), value_to_json(args[3]));
    }
    for (k, v) in properties(node) {
        m.insert(k.to_string(), value_to_json(v.value()));
    }
    for key in ["width", "height", "fps", "duration"] {
        if !m.contains_key(key) {
            return Err(format!("`composition` is missing `{key}`"));
        }
    }
    Ok(Value::Object(m))
}

fn build_output(node: &KdlNode) -> Result<Value, String> {
    // `output "path"` — bare string form.
    if let Some(path) = first_string_arg(node) {
        if node.children().is_none() {
            return Ok(Value::String(path.to_string()));
        }
    }
    // `output { path "…"; credentials { key "…"; secret "…"; region "…" } }`
    let mut m = Map::new();
    if let Some(children) = node.children() {
        for c in children.nodes() {
            match c.name().value() {
                "path" => {
                    let p = first_string_arg(c)
                        .ok_or("`output > path` needs a string value")?;
                    m.insert("path".into(), Value::String(p.to_string()));
                }
                "credentials" => {
                    let mut cred = Map::new();
                    if let Some(cc) = c.children() {
                        for kv in cc.nodes() {
                            if let Some(v) = first_string_arg(kv) {
                                cred.insert(kv.name().value().to_string(), Value::String(v.to_string()));
                            }
                        }
                    }
                    for (k, v) in properties(c) {
                        cred.insert(k.to_string(), value_to_json(v.value()));
                    }
                    m.insert("credentials".into(), Value::Object(cred));
                }
                other => return Err(format!("unknown `output` child `{other}`")),
            }
        }
    }
    if !m.contains_key("path") {
        return Err("`output` needs a path (`output \"out.mp4\"`)".into());
    }
    Ok(Value::Object(m))
}

fn build_asset(node: &KdlNode) -> Result<(String, Value), String> {
    let kind = node.name().value();
    let id = first_string_arg(node)
        .ok_or_else(|| format!("`{kind}` asset needs an id: `{kind} \"my_id\" path=\"…\"`"))?
        .to_string();
    let mut m = Map::new();

    let asset_type = if kind == "asset" {
        property(node, "type")
            .and_then(|v| v.as_string())
            .ok_or("`asset` needs a `type=`")?
            .to_string()
    } else {
        kind.to_string()
    };
    m.insert("type".into(), Value::String(asset_type));

    for (k, v) in properties(node) {
        if k == "type" {
            continue;
        }
        m.insert(k.to_string(), value_to_json(v.value()));
    }
    if !m.contains_key("path") {
        return Err(format!("asset `{id}` is missing `path=`"));
    }
    Ok((id, Value::Object(m)))
}

const CLIP_TYPES: [&str; 4] = ["media", "solid", "text", "effect"];

fn build_track(node: &KdlNode, defs: &Defs) -> Result<Value, String> {
    let mut m = Map::new();
    let id = first_string_arg(node)
        .ok_or("`track` needs an id")?
        .to_string();
    m.insert("id".into(), Value::String(id.clone()));
    if let Some(start) = property(node, "start") {
        m.insert("start".into(), value_to_json(start));
    }

    let mut clips: Vec<Value> = Vec::new();
    let mut transitions: Vec<Value> = Vec::new();
    if let Some(children) = node.children() {
        let mut trans_idx = 0;
        for c in children.nodes() {
            if CLIP_TYPES.contains(&c.name().value()) {
                clips.push(build_clip(c, defs).map_err(|e| format!("track `{id}`: {e}"))?);
            } else {
                // Any non-clip node is treated as a shader-driven transition,
                // so transition types stay open-ended.
                transitions.push(
                    build_transition(c, trans_idx).map_err(|e| format!("track `{id}`: {e}"))?,
                );
                trans_idx += 1;
            }
        }
    }
    m.insert("clips".into(), Value::Array(clips));
    if !transitions.is_empty() {
        m.insert("transitions".into(), Value::Array(transitions));
    }
    Ok(Value::Object(m))
}

fn build_transition(node: &KdlNode, idx: usize) -> Result<Value, String> {
    let ttype = node.name().value().to_string();
    let mut m = Map::new();
    // Optional explicit id as the first string arg; otherwise auto-generate.
    let id = first_string_arg(node)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{ttype}_{idx}"));
    m.insert("id".into(), Value::String(id));
    m.insert("type".into(), Value::String(ttype.clone()));

    let mut params = Map::new();
    for (k, v) in properties(node) {
        match k {
            "from" | "to" | "shader" => {
                m.insert(k.to_string(), value_to_json(v.value()));
            }
            "at" | "start" => {
                m.insert("start".into(), value_to_json(v.value()));
            }
            "dur" | "duration" => {
                m.insert("duration".into(), value_to_json(v.value()));
            }
            _ => {
                params.insert(k.to_string(), dynamic_from_prop(v));
            }
        }
    }
    // Vector/keyframe params as child nodes.
    if let Some(children) = node.children() {
        for c in children.nodes() {
            if c.name().value() == "params" {
                for p in c.children().map(|d| d.nodes()).unwrap_or(&[]) {
                    params.insert(p.name().value().to_string(), node_to_dynamic(p)?);
                }
            } else {
                params.insert(c.name().value().to_string(), node_to_dynamic(c)?);
            }
        }
    }
    for req in ["from", "to"] {
        if !m.contains_key(req) {
            return Err(format!(
                "transition `{ttype}` (id `{}`) is missing `{req}=`; \
                 if `{ttype}` was meant to be a clip, valid clip types are {}",
                m["id"].as_str().unwrap_or("?"),
                CLIP_TYPES.join(", ")
            ));
        }
    }
    if !m.contains_key("duration") {
        return Err(format!(
            "transition `{ttype}` (id `{}`) is missing `dur=`",
            m["id"].as_str().unwrap_or("?")
        ));
    }
    if !params.is_empty() {
        m.insert("params".into(), Value::Object(params));
    }
    Ok(Value::Object(m))
}

fn build_clip(node: &KdlNode, defs: &Defs) -> Result<Value, String> {
    let clip_type = node.name().value().to_string();
    let mut m = Map::new();

    let id = first_string_arg(node)
        .ok_or_else(|| format!("`{clip_type}` clip needs an id"))?
        .to_string();
    m.insert("id".into(), Value::String(id.clone()));
    m.insert("type".into(), Value::String(clip_type.clone()));

    // duration: second positional number, or `dur=`/`duration=`.
    let num_args: Vec<&KdlValue> = positional_args(node)
        .into_iter()
        .filter(|v| v.as_integer().is_some() || v.as_float().is_some())
        .collect();
    if let Some(d) = num_args.first() {
        m.insert("duration".into(), value_to_json(d));
    }

    let mut params = Map::new();
    let mut color_prop: Option<Value> = None;
    for (k, v) in properties(node) {
        match k {
            "dur" | "duration" => {
                m.insert("duration".into(), value_to_json(v.value()));
            }
            "asset" | "scale_mode" | "blend_mode" | "shader" | "preset" => {
                m.insert(k.to_string(), value_to_json(v.value()));
            }
            "offset" | "trim_start" => {
                m.insert(k.to_string(), value_to_json(v.value()));
            }
            "color" if clip_type == "solid" => {
                color_prop = Some(color_to_json(v.value())?);
            }
            _ => {
                params.insert(k.to_string(), dynamic_from_prop(v));
            }
        }
    }

    if !m.contains_key("duration") {
        return Err(format!("clip `{id}` is missing a duration"));
    }

    // Text params built from props (simple) and/or children (layout/transitions).
    let mut text_params = Map::new();

    if let Some(children) = node.children() {
        for c in children.nodes() {
            match c.name().value() {
                "transform" => {
                    m.insert("transform".into(), build_transform(c, defs)?);
                }
                "fx" | "effects" => {
                    m.insert("effects".into(), build_effects(c, defs)?);
                }
                "params" => {
                    for p in c.children().map(|d| d.nodes()).unwrap_or(&[]) {
                        params.insert(p.name().value().to_string(), node_to_dynamic(p)?);
                    }
                }
                "enter" | "entrance" => {
                    text_params.insert("entrance".into(), build_text_transition(c, defs)?);
                }
                "exit" => {
                    text_params.insert("exit".into(), build_text_transition(c, defs)?);
                }
                "vstack" | "hstack" | "zstack" => {
                    text_params.insert("kind".into(), Value::String("layout".into()));
                    text_params.insert("body".into(), build_layout_node(c)?);
                }
                "axes" => {
                    text_params.insert("axes".into(), build_axes(c)?);
                }
                other => return Err(format!("unknown child `{other}` in clip `{id}`")),
            }
        }
    }

    // Solid color.
    if clip_type == "solid" {
        let color = color_prop
            .ok_or_else(|| format!("solid clip `{id}` needs a `color=`"))?;
        m.insert("solid_params".into(), serde_json::json!({ "color": color }));
    }

    // Simple-text props live alongside layout detection.
    if clip_type == "text" {
        for (k, v) in properties(node) {
            match k {
                "text" => {
                    text_params.insert("text".into(), value_to_json(v.value()));
                }
                "font" => {
                    text_params.insert("font".into(), value_to_json(v.value()));
                }
                "size" | "font_size" => {
                    text_params.insert("font_size".into(), dynamic_from_prop(v));
                }
                "color" => {
                    text_params.insert("color".into(), color_to_json(v.value())?);
                }
                _ => {}
            }
        }
        if !text_params.is_empty() {
            m.insert("text_params".into(), Value::Object(text_params));
        }
    }

    if !params.is_empty() {
        m.insert("params".into(), Value::Object(params));
    }
    Ok(Value::Object(m))
}

fn build_transform(node: &KdlNode, defs: &Defs) -> Result<Value, String> {
    if let Some(base) = use_base(node, defs, build_transform)? {
        return Ok(base);
    }
    let mut m = Map::new();
    for (k, v) in properties(node) {
        if k == "use" {
            continue;
        }
        // scalar constant / expression as a property
        m.insert(k.to_string(), dynamic_from_prop(v));
    }
    if let Some(children) = node.children() {
        for c in children.nodes() {
            m.insert(c.name().value().to_string(), node_to_dynamic(c)?);
        }
    }
    Ok(Value::Object(m))
}

fn build_effects(node: &KdlNode, defs: &Defs) -> Result<Value, String> {
    let mut effects: Vec<Value> = Vec::new();
    if let Some(children) = node.children() {
        for c in children.nodes() {
            effects.push(build_effect(c, defs)?);
        }
    }
    Ok(Value::Array(effects))
}

fn build_effect(node: &KdlNode, defs: &Defs) -> Result<Value, String> {
    if let Some(base) = use_base(node, defs, build_effect)? {
        return Ok(base);
    }
    let mut m = Map::new();
    m.insert("type".into(), Value::String(node.name().value().to_string()));
    let mut params = Map::new();
    for (k, v) in properties(node) {
        match k {
            "use" => {}
            "shader" | "preset" => {
                m.insert(k.to_string(), value_to_json(v.value()));
            }
            _ => {
                params.insert(k.to_string(), dynamic_from_prop(v));
            }
        }
    }
    if let Some(children) = node.children() {
        for c in children.nodes() {
            params.insert(c.name().value().to_string(), node_to_dynamic(c)?);
        }
    }
    if !params.is_empty() {
        m.insert("params".into(), Value::Object(params));
    }
    Ok(Value::Object(m))
}

fn build_text_transition(node: &KdlNode, defs: &Defs) -> Result<Value, String> {
    if let Some(base) = use_base(node, defs, build_text_transition)? {
        return Ok(base);
    }
    let mut m = Map::new();
    if let Some(t) = first_string_arg(node) {
        m.insert("type".into(), Value::String(t.to_string()));
    }
    for (k, v) in properties(node) {
        match k {
            "use" => {}
            "type" => { m.insert("type".into(), value_to_json(v.value())); }
            "dur" | "duration" => { m.insert("duration".into(), value_to_json(v.value())); }
            "delay" => { m.insert("delay".into(), value_to_json(v.value())); }
            _ => { m.insert(k.to_string(), value_to_json(v.value())); }
        }
    }
    // start_transform child: `from { opacity=0; scale=0.5; rotation=10; offset 0 40 }`
    if let Some(children) = node.children() {
        for c in children.nodes() {
            match c.name().value() {
                "from" | "start_transform" => {
                    m.insert("start_transform".into(), build_start_transform(c)?);
                }
                other => return Err(format!("unknown child `{other}` in text transition")),
            }
        }
    }
    Ok(Value::Object(m))
}

fn build_start_transform(node: &KdlNode) -> Result<Value, String> {
    let mut m = Map::new();
    for (k, v) in properties(node) {
        match k {
            "opacity" | "rotation" => {
                m.insert(k.to_string(), value_to_json(v.value()));
            }
            "scale" => {
                m.insert("scale".into(), value_to_json(v.value()));
            }
            _ => return Err(format!("unknown start-transform property `{k}`")),
        }
    }
    if let Some(children) = node.children() {
        for c in children.nodes() {
            match c.name().value() {
                "offset" | "position_offset" => {
                    m.insert("position_offset".into(), numeric_array(c));
                }
                "scale" => {
                    m.insert("scale".into(), node_to_dynamic(c)?);
                }
                other => return Err(format!("unknown start-transform child `{other}`")),
            }
        }
    }
    Ok(Value::Object(m))
}

const LAYOUT_TYPES: [&str; 5] = ["vstack", "hstack", "zstack", "text", "spacer"];

fn build_layout_node(node: &KdlNode) -> Result<Value, String> {
    let kind = node.name().value();
    let mut m = Map::new();
    m.insert("type".into(), Value::String(kind.to_string()));

    match kind {
        "text" => {
            if let Some(t) = first_string_arg(node) {
                m.insert("text".into(), Value::String(t.to_string()));
            }
        }
        "spacer" => {
            if let Some(size) = positional_args(node).into_iter().find(|v| {
                v.as_integer().is_some() || v.as_float().is_some()
            }) {
                m.insert("size".into(), value_to_json(size));
            }
        }
        _ => {}
    }

    for (k, v) in properties(node) {
        match k {
            "align" | "alignment" => { m.insert("alignment".into(), value_to_json(v.value())); }
            "spacing" => { m.insert("spacing".into(), dynamic_from_prop(v)); }
            "size" => { m.insert("size".into(), dynamic_from_prop(v)); }
            "text" => { m.insert("text".into(), value_to_json(v.value())); }
            "font" => { m.insert("font".into(), value_to_json(v.value())); }
            "size_px" | "font_size" => { m.insert("font_size".into(), dynamic_from_prop(v)); }
            "color" => { m.insert("color".into(), color_to_json(v.value())?); }
            _ => { m.insert(k.to_string(), dynamic_from_prop(v)); }
        }
    }
    // `size` on a text node means font size; keep `size` as fixed extent only
    // for spacers. Re-map a stray `size` prop for text into font_size.
    if kind == "text" {
        if let Some(sz) = property_entry(node, "size") {
            m.insert("font_size".into(), dynamic_from_prop(sz));
            m.remove("size");
        }
    }

    let mut children: Vec<Value> = Vec::new();
    if let Some(kids) = node.children() {
        for c in kids.nodes() {
            match c.name().value() {
                n if LAYOUT_TYPES.contains(&n) => children.push(build_layout_node(c)?),
                "padding" => {
                    m.insert("padding".into(), padding_value(c));
                }
                "axes" => {
                    m.insert("axes".into(), build_axes(c)?);
                }
                other => return Err(format!("unknown layout child `{other}`")),
            }
        }
    }
    if !children.is_empty() {
        m.insert("children".into(), Value::Array(children));
    }
    Ok(Value::Object(m))
}

fn build_axes(node: &KdlNode) -> Result<Value, String> {
    let mut m = Map::new();
    for (k, v) in properties(node) {
        m.insert(k.to_string(), dynamic_from_prop(v));
    }
    // Expression axes as children: `axes { wght (expr)"…" }`
    if let Some(children) = node.children() {
        for c in children.nodes() {
            m.insert(c.name().value().to_string(), node_to_dynamic(c)?);
        }
    }
    Ok(Value::Object(m))
}

fn build_audio_track(node: &KdlNode) -> Result<Value, String> {
    let mut m = Map::new();
    m.insert("id".into(), Value::String(
        first_string_arg(node).ok_or("`audio_track` needs an id")?.to_string(),
    ));
    if let Some(start) = property(node, "start") {
        m.insert("start".into(), value_to_json(start));
    }
    let mut clips: Vec<Value> = Vec::new();
    if let Some(children) = node.children() {
        for c in children.nodes() {
            let mut cm = Map::new();
            cm.insert("id".into(), Value::String(
                first_string_arg(c).ok_or("audio clip needs an id")?.to_string(),
            ));
            for (k, v) in properties(c) {
                let key = if k == "dur" { "duration" } else { k };
                cm.insert(key.to_string(), value_to_json(v.value()));
            }
            clips.push(Value::Object(cm));
        }
    }
    m.insert("clips".into(), Value::Array(clips));
    Ok(Value::Object(m))
}

fn build_preset(node: &KdlNode, defs: &Defs) -> Result<Value, String> {
    let mut m = Map::new();
    m.insert("name".into(), Value::String(
        first_string_arg(node).ok_or("`preset` needs a name")?.to_string(),
    ));
    let mut inputs: Vec<Value> = Vec::new();
    let mut filters: Vec<Value> = Vec::new();
    if let Some(children) = node.children() {
        for c in children.nodes() {
            if c.name().value() == "input" {
                let mut im = Map::new();
                im.insert("name".into(), Value::String(
                    first_string_arg(c).ok_or("preset `input` needs a name")?.to_string(),
                ));
                for (k, v) in properties(c) {
                    let key = if k == "default" { "defaultValue" } else { k };
                    im.insert(key.to_string(), value_to_json(v.value()));
                }
                inputs.push(Value::Object(im));
            } else {
                filters.push(build_effect(c, defs)?);
            }
        }
    }
    m.insert("inputs".into(), Value::Array(inputs));
    m.insert("filters".into(), Value::Array(filters));
    Ok(Value::Object(m))
}

// ── Dynamic value handling ─────────────────────────────────────────────────

/// A property used where a dynamic value is expected: a constant scalar, or an
/// `(expr)`-annotated expression string → `{ "expression": "…" }`.
fn dynamic_from_prop(e: &kdl::KdlEntry) -> Value {
    if e.ty().map(|t| t.value()) == Some("expr") {
        if let Some(s) = e.value().as_string() {
            return serde_json::json!({ "expression": s });
        }
    }
    value_to_json(e.value())
}

/// Convert a child node standing in for a dynamic value into its JSON form:
/// keyframe array, expression object, vector, or scalar.
fn node_to_dynamic(node: &KdlNode) -> Result<Value, String> {
    // Keyframes: child `key` nodes.
    let has_keys = node
        .children()
        .map(|d| d.nodes().iter().any(|n| n.name().value() == "key"))
        .unwrap_or(false);
    if has_keys {
        let mut frames: Vec<Value> = Vec::new();
        for k in node.children().unwrap().nodes() {
            if k.name().value() != "key" {
                return Err("only `key` nodes are allowed inside a keyframed value".into());
            }
            frames.push(build_keyframe(k)?);
        }
        return Ok(Value::Array(frames));
    }

    // Expression: single `(expr)`-annotated arg.
    let pos = positional_entries(node);
    if let [entry] = pos.as_slice() {
        if entry.ty().map(|t| t.value()) == Some("expr") {
            let s = entry
                .value()
                .as_string()
                .ok_or("`(expr)` value must be a string")?;
            return Ok(serde_json::json!({ "expression": s }));
        }
    }

    // Color string.
    if let [entry] = pos.as_slice() {
        if let Some(s) = entry.value().as_string() {
            if s.starts_with('#') {
                return color_to_json(entry.value());
            }
            return Ok(Value::String(s.to_string()));
        }
    }

    // Vector / scalar constant from numeric args.
    Ok(numeric_array(node))
}

fn build_keyframe(node: &KdlNode) -> Result<Value, String> {
    let pos = positional_entries(node);
    if pos.is_empty() {
        return Err("`key` needs a time and a value: `key 0.0 1.0`".into());
    }
    let mut m = Map::new();
    m.insert("time".into(), value_to_json(pos[0].value()));

    // Value: remaining positional args. One → scalar (or expr/color); many → array.
    let rest = &pos[1..];
    let value = match rest {
        [] => return Err("`key` needs a value after the time".into()),
        [e] if e.ty().map(|t| t.value()) == Some("expr") => {
            serde_json::json!({ "expression": e.value().as_string().unwrap_or("") })
        }
        [e] => {
            if let Some(s) = e.value().as_string() {
                if s.starts_with('#') {
                    color_to_json(e.value())?
                } else {
                    Value::String(s.to_string())
                }
            } else {
                value_to_json(e.value())
            }
        }
        many => Value::Array(many.iter().map(|e| value_to_json(e.value())).collect()),
    };
    m.insert("value".into(), value);

    if let Some(e) = property(node, "ease").or_else(|| property(node, "easing")) {
        m.insert("easing".into(), value_to_json(e));
    }
    Ok(Value::Object(m))
}

/// `padding 100 60 100 60` → `[…]`; a single arg stays a scalar.
fn padding_value(node: &KdlNode) -> Value {
    numeric_array(node)
}

/// Collect a node's positional numeric args: one → scalar number, many → array.
fn numeric_array(node: &KdlNode) -> Value {
    let vals: Vec<Value> = positional_args(node).into_iter().map(value_to_json).collect();
    match vals.len() {
        1 => vals.into_iter().next().unwrap(),
        _ => Value::Array(vals),
    }
}

// ── Colors ──────────────────────────────────────────────────────────────

fn color_to_json(v: &KdlValue) -> Result<Value, String> {
    if let Some(s) = v.as_string() {
        let rgba = hex_to_rgba(s)?;
        return Ok(Value::Array(
            rgba.iter().map(|f| serde_json::json!(*f)).collect(),
        ));
    }
    Err("color must be a hex string like \"#F2E633\" or \"#FFFFFFE6\"".into())
}

fn hex_to_rgba(s: &str) -> Result<[f64; 4], String> {
    let h = s.strip_prefix('#').unwrap_or(s);
    let parse = |slice: &str| -> Result<f64, String> {
        u8::from_str_radix(slice, 16)
            .map(|b| (b as f64) / 255.0)
            .map_err(|_| format!("invalid hex color `{s}`"))
    };
    let comp = |a: char, b: char| -> Result<f64, String> {
        parse(&format!("{a}{b}"))
    };
    let chars: Vec<char> = h.chars().collect();
    match chars.len() {
        3 => Ok([
            comp(chars[0], chars[0])?,
            comp(chars[1], chars[1])?,
            comp(chars[2], chars[2])?,
            1.0,
        ]),
        6 => Ok([
            comp(chars[0], chars[1])?,
            comp(chars[2], chars[3])?,
            comp(chars[4], chars[5])?,
            1.0,
        ]),
        8 => Ok([
            comp(chars[0], chars[1])?,
            comp(chars[2], chars[3])?,
            comp(chars[4], chars[5])?,
            comp(chars[6], chars[7])?,
        ]),
        _ => Err(format!("hex color `{s}` must have 3, 6, or 8 digits")),
    }
}

// ── `def` / `use` reuse ────────────────────────────────────────────────────

type Defs = HashMap<String, KdlNode>;

/// If `node` carries `use="name"`, build the referenced def in this context and
/// merge the node's own fields over it. Returns `None` when there is no `use`.
fn use_base(
    node: &KdlNode,
    defs: &Defs,
    builder: fn(&KdlNode, &Defs) -> Result<Value, String>,
) -> Result<Option<Value>, String> {
    let Some(name) = property(node, "use").and_then(|v| v.as_string()) else {
        return Ok(None);
    };
    let def = defs
        .get(name)
        .ok_or_else(|| format!("`use=\"{name}\"` references an undefined `def`"))?;
    let mut base = builder(def, defs)?;
    // Build the node's own fields with `use` removed, so the builder doesn't
    // recurse back into `use_base` on the same node.
    let mut stripped = node.clone();
    stripped
        .entries_mut()
        .retain(|e| e.name().map(|n| n.value()) != Some("use"));
    let overrides = builder(&stripped, defs)?;
    merge(&mut base, overrides);
    Ok(Some(base))
}

/// Shallow-into-deep merge: object keys from `over` overwrite `base`; nested
/// objects merge recursively; everything else replaces.
fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

// ── KDL primitives ──────────────────────────────────────────────────────

fn value_to_json(v: &KdlValue) -> Value {
    match v {
        KdlValue::String(s) => Value::String(s.clone()),
        KdlValue::Integer(i) => Value::Number((*i as i64).into()),
        KdlValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        KdlValue::Bool(b) => Value::Bool(*b),
        KdlValue::Null => Value::Null,
    }
}

fn positional_entries(node: &KdlNode) -> Vec<&kdl::KdlEntry> {
    node.entries().iter().filter(|e| e.name().is_none()).collect()
}

fn positional_args(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect()
}

fn properties(node: &KdlNode) -> Vec<(&str, &kdl::KdlEntry)> {
    node.entries()
        .iter()
        .filter_map(|e| e.name().map(|n| (n.value(), e)))
        .collect()
}

fn property_entry<'a>(node: &'a KdlNode, key: &str) -> Option<&'a kdl::KdlEntry> {
    node.entries()
        .iter()
        .filter(|e| e.name().map(|n| n.value()) == Some(key))
        .last()
}

fn property<'a>(node: &'a KdlNode, key: &str) -> Option<&'a KdlValue> {
    node.entries()
        .iter()
        .filter(|e| e.name().map(|n| n.value()) == Some(key))
        .map(|e| e.value())
        .last()
}

fn first_string_arg(node: &KdlNode) -> Option<&str> {
    positional_args(node).into_iter().find_map(|v| v.as_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RenderSpec;

    fn transpile(src: &str) -> Value {
        kdl_to_spec_json(src).expect("transpile failed")
    }

    /// Every transpiled spec must deserialize into the engine's RenderSpec.
    fn assert_valid(src: &str) -> RenderSpec {
        let v = transpile(src);
        serde_json::from_value(v).expect("spec did not deserialize into RenderSpec")
    }

    #[test]
    fn hex_colors() {
        assert_eq!(hex_to_rgba("#FFFFFF").unwrap(), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(hex_to_rgba("#000000").unwrap(), [0.0, 0.0, 0.0, 1.0]);
        let a = hex_to_rgba("#FFFFFF80").unwrap();
        assert!((a[3] - 0.5019607843).abs() < 1e-6);
        assert_eq!(hex_to_rgba("#f00").unwrap(), [1.0, 0.0, 0.0, 1.0]);
        assert!(hex_to_rgba("#xyz").is_err());
    }

    #[test]
    fn solid_clips_and_transitions() {
        let src = r##"
            composition width=800 height=800 fps=30 duration=8.0
            output "out.mp4"
            track "main" {
                solid "red"   2.0 color="#CC3333"
                solid "green" 2.0 color="#33CC33"
                fade from="red" to="green" at=1.5 dur=1.0
                slide_switch from="green" to="red" at=3.5 dur=1.0 direction=1.0 gap_size=0.12
            }
        "##;
        let v = transpile(src);
        let track = &v["tracks"][0];
        assert_eq!(track["clips"][0]["type"], "solid");
        assert_eq!(track["clips"][0]["solid_params"]["color"][0], 0.8);
        let t0 = &track["transitions"][0];
        assert_eq!(t0["type"], "fade");
        assert_eq!(t0["id"], "fade_0");
        assert_eq!(t0["start"], 1.5);
        assert_eq!(t0["from"], "red");
        let t1 = &track["transitions"][1];
        assert_eq!(t1["params"]["gap_size"], 0.12);
        assert_valid(src);
    }

    #[test]
    fn keyframes_and_expressions() {
        let src = r##"
            composition width=1920 height=1080 fps=30 duration=10.0
            output "out.mp4"
            image "logo" path="logo.png"
            track "t" {
                media "m" 10.0 asset="logo" {
                    transform {
                        position (expr)"[comp.width*0.85, comp.height*0.15]"
                        scale 0.15
                        opacity {
                            key 0.0 0.0 ease="ease_out"
                            key 1.0 0.8
                            key 9.0 0.8 ease="ease_in"
                            key 10.0 0.0
                        }
                    }
                }
            }
        "##;
        let v = transpile(src);
        let tr = &v["tracks"][0]["clips"][0]["transform"];
        assert_eq!(tr["position"]["expression"], "[comp.width*0.85, comp.height*0.15]");
        assert_eq!(tr["scale"], 0.15);
        let kf = tr["opacity"].as_array().unwrap();
        assert_eq!(kf.len(), 4);
        assert_eq!(kf[0]["time"], 0.0);
        assert_eq!(kf[0]["value"], 0.0);
        assert_eq!(kf[0]["easing"], "ease_out");
        assert_eq!(kf[3]["value"], 0.0);
        assert_valid(src);
    }

    #[test]
    fn expr_as_property() {
        // `(expr)` on a `key=value` property (not just a child node) must expand
        // to an expression object — covers transform fields and layout axes.
        let src = r##"
            composition width=1920 height=1080 fps=30 duration=5.0
            output "out.mp4"
            font "f" path="f.ttf" provider="local"
            image "logo" path="logo.png"
            track "t" {
                media "m" 5.0 asset="logo" {
                    transform opacity=(expr)"clip_time*2"
                    fx { brightness factor=(expr)"1.0 + clip_time" }
                }
                text "h" 5.0 {
                    vstack {
                        text "X" font="f" size=64 {
                            axes wght=(expr)"300.0 + 400.0 * sin(clip_time)"
                        }
                    }
                }
            }
        "##;
        let v = transpile(src);
        let m = &v["tracks"][0]["clips"][0];
        assert_eq!(m["transform"]["opacity"]["expression"], "clip_time*2");
        assert_eq!(m["effects"][0]["params"]["factor"]["expression"], "1.0 + clip_time");
        let axes = &v["tracks"][0]["clips"][1]["text_params"]["body"]["children"][0]["axes"];
        assert_eq!(axes["wght"]["expression"], "300.0 + 400.0 * sin(clip_time)");
        assert_valid(src);
    }

    #[test]
    fn def_reuse_for_entrance() {
        let src = r##"
            composition width=1080 height=1920 fps=30 duration=4.0
            output "out.mp4"
            font "inter" path="Inter.ttf" provider="local"
            def "reveal_in" {
                enter "reveal" granularity="letter" delay=0.0 dur=0.2 easing="linear" {
                    from opacity=0.0
                }
            }
            track "t" {
                text "c1" 2.0 {
                    enter use="reveal_in"
                    vstack spacing=50 align="center" {
                        padding 100 60 100 60
                        text "Title" font="inter" size=72 color="#F2E633"
                        spacer 60
                        text "Body copy" font="inter" size=44 color="#FFFFFFE6"
                    }
                }
                text "c2" 2.0 {
                    enter use="reveal_in" delay=0.05
                    vstack { text "Second" font="inter" size=64 color="#FFFFFF" }
                }
            }
        "##;
        let v = transpile(src);
        let clips = v["tracks"][0]["clips"].as_array().unwrap();

        // c1 inherits the def verbatim.
        let e1 = &clips[0]["text_params"]["entrance"];
        assert_eq!(e1["type"], "reveal");
        assert_eq!(e1["granularity"], "letter");
        assert_eq!(e1["duration"], 0.2);
        assert_eq!(e1["start_transform"]["opacity"], 0.0);

        // c2 overrides delay while inheriting the rest.
        let e2 = &clips[1]["text_params"]["entrance"];
        assert_eq!(e2["delay"], 0.05);
        assert_eq!(e2["type"], "reveal");

        // Layout tree shape.
        let body = &clips[0]["text_params"]["body"];
        assert_eq!(body["type"], "vstack");
        assert_eq!(body["padding"], serde_json::json!([100, 60, 100, 60]));
        let kids = body["children"].as_array().unwrap();
        assert_eq!(kids[0]["type"], "text");
        assert_eq!(kids[0]["text"], "Title");
        assert_eq!(kids[0]["font_size"], 72);
        assert_eq!(kids[1]["type"], "spacer");
        assert_eq!(kids[1]["size"], 60);

        assert_valid(src);
    }

    #[test]
    fn effects_chain() {
        let src = r##"
            composition width=800 height=800 fps=30 duration=2.0
            output "out.png"
            image "img" path="in.jpg"
            track "t" {
                media "c" 2.0 asset="img" {
                    fx {
                        grayscale
                        brightness factor=1.3
                        glow intensity=1.0 radius=4.0 threshold=0.4
                    }
                }
            }
        "##;
        let v = transpile(src);
        let fx = v["tracks"][0]["clips"][0]["effects"].as_array().unwrap();
        assert_eq!(fx[0]["type"], "grayscale");
        assert!(fx[0].get("params").is_none());
        assert_eq!(fx[1]["type"], "brightness");
        assert_eq!(fx[1]["params"]["factor"], 1.3);
        assert_eq!(fx[2]["params"]["threshold"], 0.4);
        assert_valid(src);
    }

    #[test]
    fn missing_required_fields_error() {
        // No composition.
        assert!(kdl_to_spec_json("output \"x.mp4\"").is_err());
        // Transition without from/to.
        let bad = r##"
            composition width=1 height=1 fps=1 duration=1.0
            output "x.mp4"
            track "t" { fade dur=1.0 }
        "##;
        assert!(kdl_to_spec_json(bad).is_err());
    }
}
