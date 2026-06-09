//! Render spec deserialization and runtime expression / keyframe evaluation.
//!
//! This module defines the data structures that map to the JSON render
//! specification, and provides evaluation functions for dynamic properties
//! including literal values, `evalexpr` expressions, and keyframe arrays
//! with linear interpolation.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::RwLock;
use evalexpr::{ContextWithMutableVariables, ContextWithMutableFunctions, HashMapContext};

#[derive(Deserialize, Debug, Clone)]
pub struct EffectMetadata {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub params: Vec<EffectParamMetadata>,
    #[serde(default)]
    pub spot_checks: Vec<SpotCheckMetadata>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct EffectParamMetadata {
    pub name: String,
    #[serde(default)]
    pub target: String,
    #[serde(rename = "type")]
    pub param_type: String,
    pub default: f32,
}

impl EffectParamMetadata {
    pub fn target_name(&self) -> &str {
        if self.target.is_empty() {
            &self.name
        } else {
            &self.target
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct SpotCheckMetadata {
    pub time_start: f32,
    pub time_step: f32,
    pub param_name: Option<String>,
    pub format: String,
}

// NOTE: EFFECTS_REGISTRY and TRANSITIONS_REGISTRY share structurally identical patterns for
// registration and access. If a third registry type is introduced in the future, consider
// extracting a generic Registry<T> type or using a macro to reduce boilerplate.

static EFFECTS_REGISTRY: RwLock<Vec<EffectMetadata>> = RwLock::new(Vec::new());

pub fn register_effect_metadata(meta: EffectMetadata) {
    if let Ok(mut registry) = EFFECTS_REGISTRY.write() {
        if let Some(existing) = registry.iter_mut().find(|m| m.effect_type == meta.effect_type) {
            *existing = meta;
        } else {
            registry.push(meta);
        }
    }
}

pub fn get_effects_registry() -> Vec<EffectMetadata> {
    EFFECTS_REGISTRY.read().map(|r| r.clone()).unwrap_or_default()
}

#[derive(Deserialize, Debug, Clone)]
pub struct TransitionMetadata {
    #[serde(rename = "type")]
    pub transition_type: String,
    pub params: Vec<EffectParamMetadata>,
    #[serde(default)]
    pub spot_checks: Vec<SpotCheckMetadata>,
}

static TRANSITIONS_REGISTRY: RwLock<Vec<TransitionMetadata>> = RwLock::new(Vec::new());

pub fn register_transition_metadata(meta: TransitionMetadata) {
    if let Ok(mut registry) = TRANSITIONS_REGISTRY.write() {
        if let Some(existing) = registry.iter_mut().find(|m| m.transition_type == meta.transition_type) {
            *existing = meta;
        } else {
            registry.push(meta);
        }
    }
}

pub fn get_transitions_registry() -> Vec<TransitionMetadata> {
    TRANSITIONS_REGISTRY.read().map(|r| r.clone()).unwrap_or_default()
}

fn extract_tagged_block(wgsl: &str, tag: &str) -> Option<String> {
    let content_start = wgsl.find(tag)? + tag.len();
    let end = wgsl[content_start..].find("*/")?;
    Some(wgsl[content_start..content_start + end].trim().to_string())
}

pub fn extract_transition_metadata(wgsl: &str) -> Option<String> {
    extract_tagged_block(wgsl, "/* TRANSITION_METADATA:")
}

pub fn extract_metadata(wgsl: &str) -> Option<String> {
    extract_tagged_block(wgsl, "/* EFFECTS_METADATA:")
}

// ---------------------------------------------------------------------------
// Data model — deserialized from the JSON render spec
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum OutputConfig {
    Simple(String),
    Detailed {
        path: String,
        credentials: Option<OutputCredentials>,
    },
}

impl OutputConfig {
    pub fn path(&self) -> &str {
        match self {
            OutputConfig::Simple(s) => s,
            OutputConfig::Detailed { path, .. } => path,
        }
    }

    pub fn credentials(&self) -> Option<&OutputCredentials> {
        match self {
            OutputConfig::Simple(_) => None,
            OutputConfig::Detailed { credentials, .. } => credentials.as_ref(),
        }
    }

    /// Returns the path with any query parameters stripped.
    /// Useful for extension-based format detection (e.g. `.mp4` vs `.png`)
    /// when the path may contain signed-URL query strings.
    pub fn clean_path(&self) -> &str {
        let p = self.path();
        p.split('?').next().unwrap_or(p)
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct OutputCredentials {
    pub key: Option<String>,
    pub secret: Option<String>,
    pub region: Option<String>,
}

/// Root specification for a render job, containing composition settings,
/// assets, tracks, and optional audio tracks.
#[derive(Deserialize, Debug, Clone)]
pub struct RenderSpec {
    pub version: String,
    pub output: OutputConfig,
    pub composition: Composition,
    pub assets: HashMap<String, Asset>,
    #[serde(default)]
    pub presets: Option<Vec<PresetDef>>,
    pub tracks: Vec<Track>,
    pub audio_tracks: Option<Vec<AudioTrack>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct PresetInput {
    pub name: String,
    #[serde(rename = "type")]
    pub input_type: String,
    #[serde(rename = "defaultValue")]
    pub default_value: serde_json::Value,
}

#[derive(Deserialize, Debug, Clone)]
pub struct PresetDef {
    pub name: String,
    pub inputs: Vec<PresetInput>,
    pub filters: Vec<Effect>,
}

/// Output dimensions, frame rate, and total duration of the composition.
#[derive(Deserialize, Debug, Clone)]
pub struct Composition {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration: f32,
}

/// A named asset referenced by clips (video, image, audio, shader, or font).
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum Asset {
    #[serde(rename = "video")]
    Video { path: String },
    #[serde(rename = "image")]
    Image { path: String },
    #[serde(rename = "audio")]
    Audio { path: String },
    #[serde(rename = "shader")]
    Shader { path: String },
    #[serde(rename = "font")]
    Font { provider: String, path: String },
}

/// A visual track containing an ordered sequence of clips and optional transitions.
#[derive(Deserialize, Debug, Clone)]
pub struct Track {
    pub id: String,
    #[serde(default)]
    pub start: f32,
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
}

/// An audio track containing an ordered sequence of audio clips.
#[derive(Deserialize, Debug, Clone)]
pub struct AudioTrack {
    pub id: String,
    #[serde(default)]
    pub start: f32,
    pub clips: Vec<AudioClip>,
}

/// A single audio clip referencing an audio or video asset.
#[derive(Deserialize, Debug, Clone)]
pub struct AudioClip {
    pub id: String,
    pub asset: String,
    pub duration: f32,
    #[serde(default)]
    pub offset: f32,
    /// Source-audio in-point in seconds. The engine plays
    /// `[trim_start, trim_start + duration]` of the asset; defaults to 0
    /// (play from the start of the source).
    #[serde(default)]
    pub trim_start: f32,
}

/// Discriminant for the kind of content a clip renders.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClipType {
    Media,
    Solid,
    Text,
    Effect,
}

/// Porter-Duff / Photoshop-style blend mode applied when compositing a clip.
#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default] Normal,
    Multiply, Screen, Overlay, Darken, Lighten,
    ColorDodge, ColorBurn, HardLight, SoftLight,
    Difference, Exclusion,
}

impl BlendMode {
    pub fn as_u32(&self) -> u32 { *self as u32 }
}

/// A single visual clip on a track, carrying type-specific params, transform,
/// effects, and an optional custom shader.
#[derive(Deserialize, Debug, Clone)]
pub struct Clip {
    pub id: String,
    #[serde(rename = "type")]
    pub clip_type: ClipType,
    pub asset: Option<String>,
    pub duration: f32,
    #[serde(default)]
    pub offset: f32,
    #[serde(default)]
    pub trim_start: f32,
    #[serde(default)]
    pub scale_mode: Option<String>,
    #[serde(default)]
    pub solid_params: Option<SolidParams>,
    #[serde(default)]
    pub text_params: Option<TextParams>,
    #[serde(default)]
    pub transform: Option<Transform>,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub shader: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub blend_mode: Option<BlendMode>,
    #[serde(default)]
    pub params: Option<HashMap<String, serde_json::Value>>,
}

/// Parameters for a solid-color clip (RGBA, each component 0.0–1.0).
#[derive(Deserialize, Debug, Clone)]
pub struct SolidParams {
    pub color: [f32; 4],
}

/// Parameters for a text clip including font selection and styling.
#[derive(Deserialize, Debug, Clone)]
pub struct TextParams {
    // Traditional fields
    pub text: Option<String>,
    pub font: Option<String>,
    pub font_size: Option<serde_json::Value>,
    pub color: Option<[f32; 4]>,
    #[serde(default)]
    pub axes: Option<HashMap<String, serde_json::Value>>,

    // Layout fields
    pub kind: Option<String>,
    pub body: Option<LayoutNode>,

    // Transitions
    #[serde(default)]
    pub entrance: Option<TextTransition>,
    #[serde(default)]
    pub exit: Option<TextTransition>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TextTransition {
    #[serde(rename = "type")]
    pub transition_type: String,
    #[serde(default = "default_granularity")]
    pub granularity: std::borrow::Cow<'static, str>,
    #[serde(default)]
    pub delay: f32,
    pub duration: f32,
    #[serde(default = "default_easing")]
    pub easing: std::borrow::Cow<'static, str>,
    #[serde(default)]
    pub start_transform: Option<TextStartTransform>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct TextStartTransform {
    #[serde(default)]
    pub position_offset: Option<[f32; 2]>,
    #[serde(default)]
    pub scale: Option<serde_json::Value>,
    #[serde(default)]
    pub rotation: Option<f32>,
    #[serde(default)]
    pub opacity: Option<f32>,
}

fn default_granularity() -> std::borrow::Cow<'static, str> {
    std::borrow::Cow::Borrowed("letter")
}

fn default_easing() -> std::borrow::Cow<'static, str> {
    std::borrow::Cow::Borrowed("linear")
}


/// A node in the text layout tree.
#[derive(Deserialize, Debug, Clone)]
pub struct LayoutNode {
    #[serde(rename = "type", alias = "kind")]
    pub r#type: String, // "vstack", "hstack", "zstack", "spacer", "text"
    pub spacing: Option<serde_json::Value>,
    pub alignment: Option<String>,
    pub children: Option<Vec<LayoutNode>>,
    pub padding: Option<serde_json::Value>,
    pub size: Option<serde_json::Value>,
    pub text: Option<serde_json::Value>,
    pub font: Option<String>,
    pub font_size: Option<serde_json::Value>,
    pub color: Option<serde_json::Value>,
    pub axes: Option<HashMap<String, serde_json::Value>>,
}


/// Spatial transform properties (position, scale, rotation, opacity),
/// each of which may be a literal, expression, or keyframe array.
#[derive(Deserialize, Debug, Clone)]
pub struct Transform {
    pub position: Option<serde_json::Value>,
    pub scale: Option<serde_json::Value>,
    pub rotation: Option<serde_json::Value>,
    pub opacity: Option<serde_json::Value>,
}

/// A named post-processing effect applied to a clip, with optional parameters.
#[derive(Deserialize, Debug, Clone)]
pub struct Effect {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub shader: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub params: Option<HashMap<String, serde_json::Value>>,
}

/// A transition between two clips, driven by a shader.
#[derive(Deserialize, Debug, Clone)]
pub struct Transition {
    pub id: String,
    #[serde(rename = "type")]
    pub transition_type: String,
    pub shader: Option<String>,
    #[serde(default)]
    pub start: Option<f32>,
    pub duration: f32,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub params: Option<HashMap<String, serde_json::Value>>,
}

/// GPU-side uniform block for the built-in effect shader pipeline.
///
/// **Alignment contract**: this struct is `#[repr(C)]` and its total size
/// must be a multiple of 16 bytes (std140 / WGSL uniform layout). The
/// trailing `_padding` field ensures this invariant.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShaderParams {
    // Basic settings
    pub grayscale_enabled: u32,
    pub brightness_factor: f32,
    pub contrast_factor: f32,
    pub saturation_factor: f32,

    pub hue_rotate_angle: f32,
    pub blur_radius: f32,
    pub glow_intensity: f32,
    pub glow_radius: f32,

    pub glow_threshold: f32,
    pub film_grain_amount: f32,
    pub film_grain_speed: f32,
    pub film_flicker_amount: f32,

    pub film_flicker_speed: f32,
    pub depth_blur_focus_x: f32,
    pub depth_blur_focus_y: f32,
    pub depth_blur_focus_radius: f32,

    pub depth_blur_near_blur: f32,
    pub depth_blur_far_blur: f32,
    pub depth_blur_use_map: u32,
    pub flow_amount: f32,

    pub flow_speed: f32,
    pub flow_decay: f32,
    pub time: f32,
    pub clip_time: f32,

    pub width: u32,
    pub height: u32,
    pub _padding: [u32; 2],
}

impl ShaderParams {
    pub fn set_field(&mut self, target: &str, val_str_opt: Option<&str>, val_f32: f32) {
        match target {
            "grayscale_enabled" => self.grayscale_enabled = if val_f32 > 0.5 { 1 } else { 0 },
            "brightness_factor" => self.brightness_factor = val_f32,
            "contrast_factor" => self.contrast_factor = val_f32,
            "saturation_factor" => self.saturation_factor = val_f32,
            "hue_rotate_angle" => self.hue_rotate_angle = val_f32,
            "blur_radius" => self.blur_radius = val_f32,
            "glow_intensity" => self.glow_intensity = val_f32,
            "glow_radius" => self.glow_radius = val_f32,
            "glow_threshold" => self.glow_threshold = val_f32,
            "film_grain_amount" => self.film_grain_amount = val_f32,
            "film_grain_speed" => self.film_grain_speed = val_f32,
            "film_flicker_amount" => self.film_flicker_amount = val_f32,
            "film_flicker_speed" => self.film_flicker_speed = val_f32,
            "depth_blur_focus_x" => self.depth_blur_focus_x = val_f32,
            "depth_blur_focus_y" => self.depth_blur_focus_y = val_f32,
            "depth_blur_focus_radius" => self.depth_blur_focus_radius = val_f32,
            "depth_blur_near_blur" => self.depth_blur_near_blur = val_f32,
            "depth_blur_far_blur" => self.depth_blur_far_blur = val_f32,
            "depth_blur_use_map" => {
                if let Some(s) = val_str_opt {
                    if !s.is_empty() {
                        self.depth_blur_use_map = 1;
                    }
                } else if val_f32 > 0.5 {
                    self.depth_blur_use_map = 1;
                }
            }
            "flow_amount" => self.flow_amount = val_f32,
            "flow_speed" => self.flow_speed = val_f32,
            "flow_decay" => self.flow_decay = val_f32,
            _ => {
                log::warn!("Unknown ShaderParams target field: {}", target);
            }
        }
    }
}



/// GPU-side uniform block for the compositor shader.
///
/// **Alignment contract**: `#[repr(C)]` with `_padding` to maintain a
/// total size that is a multiple of 16 bytes (std140 / WGSL uniform layout).
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CompositorParams {
    pub position: [f32; 2],
    pub scale: [f32; 2],
    pub rotation: f32,
    pub opacity: f32,
    pub clip_type: u32,
    pub blend_mode: u32,
    pub grayscale: u32,
    pub brightness: f32,
    pub _padding: [u32; 2],
    pub solid_color: [f32; 4],
}

// ---------------------------------------------------------------------------
// Deduplicated clip start-time computation
// ---------------------------------------------------------------------------

/// Minimal trait exposing the two properties needed to compute absolute
/// start times for clips arranged sequentially on a track.
trait HasDurationAndOffset {
    fn duration(&self) -> f32;
    fn offset(&self) -> f32;
}

impl HasDurationAndOffset for Clip {
    fn duration(&self) -> f32 { self.duration }
    fn offset(&self) -> f32 { self.offset }
}

impl HasDurationAndOffset for AudioClip {
    fn duration(&self) -> f32 { self.duration }
    fn offset(&self) -> f32 { self.offset }
}

/// Computes absolute start times for a sequence of clips, accumulating
/// each clip's offset and duration from the track's `start` time.
fn compute_clip_start_times<T: HasDurationAndOffset>(start: f32, clips: &[T]) -> Vec<f32> {
    let mut start_times = Vec::with_capacity(clips.len());
    let mut current_time = start;
    for clip in clips {
        current_time += clip.offset().max(0.0);
        start_times.push(current_time);
        current_time += clip.duration();
    }
    start_times
}

impl Track {
    pub fn get_clip_start_times(&self) -> Vec<f32> {
        compute_clip_start_times(self.start, &self.clips)
    }

    pub fn resolve_transitions(&self) -> Vec<(Transition, f32)> {
        let clip_starts = self.get_clip_start_times();
        let mut resolved = Vec::new();
        for tr in &self.transitions {
            let start_time = tr.start.unwrap_or_else(|| {
                self.clips.iter()
                    .position(|c| c.id == tr.to)
                    .map(|idx| clip_starts[idx] - tr.duration * 0.5)
                    .unwrap_or(0.0)
            });
            resolved.push((tr.clone(), start_time));
        }
        resolved
    }
}

impl AudioTrack {
    pub fn get_clip_start_times(&self) -> Vec<f32> {
        compute_clip_start_times(self.start, &self.clips)
    }
}

// ---------------------------------------------------------------------------
// Effect parameter helper
// ---------------------------------------------------------------------------

/// Reads a named float parameter from an effect's params map, evaluating
/// expressions and keyframes.
///
/// # Parameters
/// * `effect` - The effect to query.
/// * `key` - The parameter name.
/// * `clip_time` - The current time relative to the clip start.
/// * `duration` - The total duration of the clip.
/// * `w` - The composition width.
/// * `h` - The composition height.
/// * `default` - The fallback value if parameter is missing.
fn read_effect_float(effect: &Effect, key: &str, clip_time: f32, duration: f32, w: u32, h: u32, default: f32) -> f32 {
    effect.params.as_ref()
        .and_then(|map| map.get(key))
        .map(|val| evaluate_float(val, clip_time, duration, w, h, default))
        .unwrap_or(default)
}

// ---------------------------------------------------------------------------
// RenderSpec helpers
// ---------------------------------------------------------------------------

impl RenderSpec {
    /// Recursively expands all presets in a list of effects into concrete (non-preset) effects.
    pub fn expand_effects(
        &self,
        effects: &[Effect],
        clip_time: f32,
        duration: f32,
        w: u32,
        h: u32,
        depth: usize,
    ) -> Vec<Effect> {
        if depth > 5 {
            return Vec::new();
        }
        let mut expanded = Vec::new();
        for effect in effects {
            if effect.effect_type == "preset" || effect.preset.is_some() {
                let preset_name = effect.preset.as_ref().unwrap_or(&effect.effect_type);
                let preset_def = self.presets.as_ref().and_then(|list| {
                    list.iter().find(|p| &p.name == preset_name)
                });
                if let Some(def) = preset_def {
                    let mut resolved_inputs = HashMap::new();
                    for input in &def.inputs {
                        let val = effect.params.as_ref()
                            .and_then(|p| p.get(&input.name))
                            .unwrap_or(&input.default_value);
                        
                        let resolved_val = match input.input_type.as_str() {
                            "float" => {
                                let f = evaluate_float(val, clip_time, duration, w, h, 0.0);
                                serde_json::Value::from(f)
                            }
                            "vec2" => {
                                let v = evaluate_vec2(val, clip_time, duration, w, h, [0.0, 0.0]);
                                serde_json::Value::from(v.to_vec())
                            }
                            _ => {
                                if let Some(f) = val.as_f64() {
                                    serde_json::Value::from(f)
                                } else {
                                    val.clone()
                                }
                            }
                        };
                        resolved_inputs.insert(input.name.clone(), resolved_val);
                    }

                    let mut sub_filters = Vec::new();
                    for filter in &def.filters {
                        let mut resolved_filter = filter.clone();
                        if let Some(ref params) = filter.params {
                            let mut resolved_params = HashMap::new();
                            for (key, val) in params {
                                let new_val = self.substitute_value(val, &resolved_inputs, clip_time, duration, w, h);
                                resolved_params.insert(key.clone(), new_val);
                            }
                            resolved_filter.params = Some(resolved_params);
                        }
                        sub_filters.push(resolved_filter);
                    }

                    let expanded_sub = self.expand_effects(&sub_filters, clip_time, duration, w, h, depth + 1);
                    expanded.extend(expanded_sub);
                } else {
                    log::error!("Preset '{}' not found in RenderSpec presets!", preset_name);
                }
            } else {
                expanded.push(effect.clone());
            }
        }
        expanded
    }

    fn substitute_value(
        &self,
        val: &serde_json::Value,
        resolved_inputs: &HashMap<String, serde_json::Value>,
        clip_time: f32,
        duration: f32,
        w: u32,
        h: u32,
    ) -> serde_json::Value {
        match val {
            serde_json::Value::String(s) => {
                if s.starts_with('$') {
                    let name = &s[1..];
                    if let Some(resolved) = resolved_inputs.get(name) {
                        return resolved.clone();
                    }
                }
                let mut replaced_str = s.clone();
                for (name, resolved) in resolved_inputs {
                    let pattern = format!("${}", name);
                    if replaced_str.contains(&pattern) {
                        let replacement = match resolved {
                            serde_json::Value::String(inner_s) => inner_s.clone(),
                            _ => resolved.to_string(),
                        };
                        replaced_str = replaced_str.replace(&pattern, &replacement);
                    }
                }
                serde_json::Value::String(replaced_str)
            }
            serde_json::Value::Object(obj) => {
                let mut new_obj = serde_json::Map::new();
                for (k, v) in obj {
                    new_obj.insert(k.clone(), self.substitute_value(v, resolved_inputs, clip_time, duration, w, h));
                }
                serde_json::Value::Object(new_obj)
            }
            serde_json::Value::Array(arr) => {
                let mut new_arr = Vec::new();
                for item in arr {
                    new_arr.push(self.substitute_value(item, resolved_inputs, clip_time, duration, w, h));
                }
                serde_json::Value::Array(new_arr)
            }
            _ => val.clone(),
        }
    }

    pub fn get_input_path(&self) -> Option<String> {
        for track in &self.tracks {
            for clip in &track.clips {
                if clip.clip_type == ClipType::Media {
                    if let Some(ref asset_id) = clip.asset {
                        if let Some(asset) = self.assets.get(asset_id) {
                            match asset {
                                Asset::Image { path } => return Some(path.clone()),
                                Asset::Video { path } => return Some(path.clone()),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        None
    }

    pub fn get_audio_clips(&self) -> Vec<(String, f32, f32, f32)> {
        let mut list = Vec::new();
        if let Some(ref tracks) = self.audio_tracks {
            for track in tracks {
                let start_times = track.get_clip_start_times();
                for (idx, clip) in track.clips.iter().enumerate() {
                    let absolute_start = start_times[idx];
                    if let Some(asset) = self.assets.get(&clip.asset) {
                        match asset {
                            Asset::Audio { path } => {
                                list.push((path.clone(), absolute_start, clip.duration, clip.trim_start));
                            }
                            Asset::Video { path } => {
                                list.push((path.clone(), absolute_start, clip.duration, clip.trim_start));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        list
    }

    pub fn get_spot_check_events(&self) -> Vec<(f32, String)> {
        let mut events = Vec::new();
        let is_movie = self.output.clean_path().ends_with(".mp4");

        events.push((0.0, "Composition start".to_string()));
        events.push((self.composition.duration, "Composition end".to_string()));

        let registry = get_effects_registry();
        let trans_registry = get_transitions_registry();

        for track in &self.tracks {
            let start_times = track.get_clip_start_times();
            for (idx, clip) in track.clips.iter().enumerate() {
                let clip_start = start_times[idx];
                let clip_end = clip_start + clip.duration;

                events.push((clip_start, format!("Track '{}' - Clip '{}' starts", track.id, clip.id)));
                events.push((clip_end, format!("Track '{}' - Clip '{}' ends", track.id, clip.id)));

                if let Some(ref text) = clip.text_params {
                    if text.entrance.is_some() || text.exit.is_some() {
                        events.push((
                            clip_start + clip.duration * 0.5,
                            format!("Track '{}' - Clip '{}' - Midpoint", track.id, clip.id),
                        ));
                    }
                }

                if is_movie {
                    for effect in &clip.effects {
                        if let Some(meta) = registry.iter().find(|m| m.effect_type == effect.effect_type) {
                            for check in &meta.spot_checks {
                                let mut t = check.time_start;
                                while t < clip.duration {
                                    let abs_time = clip_start + t;
                                    let mut expl = check.format.clone();
                                    if let Some(ref param_name) = check.param_name {
                                        let default_val = meta.params.iter()
                                            .find(|p| &p.name == param_name)
                                            .map(|p| p.default)
                                            .unwrap_or(1.0);
                                        let val = read_effect_float(effect, param_name, t, clip.duration, self.composition.width, self.composition.height, default_val);
                                        let replacement = if val > 0.5 { "ON" } else { "OFF" };
                                        expl = expl.replace("{}", replacement);
                                    }
                                    events.push((
                                        abs_time,
                                        format!("Track '{}' - Clip '{}' - {}", track.id, clip.id, expl),
                                    ));
                                    t += check.time_step;
                                }
                            }
                        }
                    }
                }
            }

            for (tr, start_time) in track.resolve_transitions() {
                if is_movie {
                    let tr_shader = tr.shader.as_deref().unwrap_or(&tr.transition_type);
                    if let Some(meta) = trans_registry.iter().find(|m| &m.transition_type == tr_shader) {
                        for check in &meta.spot_checks {
                            let mut t = check.time_start;
                            while t < tr.duration {
                                let abs_time = start_time + t;
                                let mut expl = check.format.clone();
                                if let Some(ref param_name) = check.param_name {
                                    let default_val = meta.params.iter()
                                        .find(|p| &p.name == param_name)
                                        .map(|p| p.default)
                                        .unwrap_or(1.0);
                                    let val = if let Some(ref map) = tr.params {
                                        if let Some(val) = map.get(param_name) {
                                            evaluate_float(val, t, tr.duration, self.composition.width, self.composition.height, default_val)
                                        } else {
                                            default_val
                                        }
                                    } else {
                                        default_val
                                    };
                                    let replacement = if val > 0.5 { "ON" } else { "OFF" };
                                    expl = expl.replace("{}", replacement);
                                }
                                events.push((
                                    abs_time,
                                    format!("Track '{}' - Transition '{}' - {}", track.id, tr.id, expl),
                                ));
                                t += check.time_step;
                            }
                        }
                    }
                }
            }
        }
        events.retain(|(t, _)| *t >= 0.0 && *t <= self.composition.duration);
        events
    }
}

// ---------------------------------------------------------------------------
// Clip evaluation methods
// ---------------------------------------------------------------------------

impl Clip {
    pub fn eval_position(&self, t: f32, w: u32, h: u32) -> [f32; 2] {
        self.transform.as_ref()
            .and_then(|tr| tr.position.as_ref())
            .map(|v| evaluate_vec2(v, t, self.duration, w, h, [w as f32 * 0.5, h as f32 * 0.5]))
            .unwrap_or([w as f32 * 0.5, h as f32 * 0.5])
    }

    pub fn eval_scale(&self, t: f32, w: u32, h: u32) -> [f32; 2] {
        self.transform.as_ref()
            .and_then(|tr| tr.scale.as_ref())
            .map(|v| evaluate_vec2(v, t, self.duration, w, h, [1.0, 1.0]))
            .unwrap_or([1.0, 1.0])
    }

    pub fn eval_rotation(&self, t: f32, w: u32, h: u32) -> f32 {
        self.transform.as_ref()
            .and_then(|tr| tr.rotation.as_ref())
            .map(|v| evaluate_float(v, t, self.duration, w, h, 0.0))
            .unwrap_or(0.0)
    }

    pub fn eval_opacity(&self, t: f32, w: u32, h: u32) -> f32 {
        self.transform.as_ref()
            .and_then(|tr| tr.opacity.as_ref())
            .map(|v| evaluate_float(v, t, self.duration, w, h, 1.0))
            .unwrap_or(1.0)
    }

    pub fn eval_built_in_effects(&self, clip_time: f32, w: u32, h: u32) -> (u32, f32) {
        eval_built_in_effects_from_effects(&self.effects, clip_time, self.duration, w, h)
    }

    pub fn get_depth_map_asset_id(&self) -> Option<String> {
        get_depth_map_asset_id_from_effects(&self.effects)
    }

    pub fn eval_shader_params(&self, clip_time: f32, comp_width: u32, comp_height: u32, time: f32) -> ShaderParams {
        eval_shader_params_from_effects(&self.effects, clip_time, self.duration, comp_width, comp_height, time)
    }

    pub fn eval_compositor_params(&self, clip_time: f32, spec: &RenderSpec) -> CompositorParams {
        let position = self.eval_position(clip_time, spec.composition.width, spec.composition.height);
        let scale = self.eval_scale(clip_time, spec.composition.width, spec.composition.height);
        let rotation = self.eval_rotation(clip_time, spec.composition.width, spec.composition.height);
        let opacity = self.eval_opacity(clip_time, spec.composition.width, spec.composition.height);
        let blend_mode_u32 = self.blend_mode.as_ref().map(|b| b.as_u32()).unwrap_or(0);
        let expanded_effects = spec.expand_effects(&self.effects, clip_time, self.duration, spec.composition.width, spec.composition.height, 0);
        let (grayscale, brightness) = eval_built_in_effects_from_effects(&expanded_effects, clip_time, self.duration, spec.composition.width, spec.composition.height);

        CompositorParams {
            position,
            scale,
            rotation,
            opacity,
            clip_type: 0,
            blend_mode: blend_mode_u32,
            grayscale,
            brightness,
            _padding: [0, 0],
            solid_color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

pub fn eval_built_in_effects_from_effects(effects: &[Effect], clip_time: f32, duration: f32, w: u32, h: u32) -> (u32, f32) {
    let mut grayscale = 0u32;
    let mut brightness = 1.0f32;
    for effect in effects {
        if effect.effect_type == "grayscale" {
            let val = read_effect_float(effect, "enabled", clip_time, duration, w, h, 1.0);
            if val > 0.5 {
                grayscale = 1;
            }
        } else if effect.effect_type == "brightness" {
            brightness = read_effect_float(effect, "factor", clip_time, duration, w, h, 1.0);
        }
    }
    (grayscale, brightness)
}

pub fn get_depth_map_asset_id_from_effects(effects: &[Effect]) -> Option<String> {
    let registry = get_effects_registry();
    for effect in effects {
        if let Some(meta) = registry.iter().find(|m| m.effect_type == effect.effect_type) {
            for param in &meta.params {
                if param.param_type == "depth_map" {
                    if let Some(ref params) = effect.params {
                        if let Some(val) = params.get(&param.name) {
                            if let Some(s) = val.as_str() {
                                return Some(s.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn eval_shader_params_from_effects(
    effects: &[Effect],
    clip_time: f32,
    duration: f32,
    comp_width: u32,
    comp_height: u32,
    time: f32,
) -> ShaderParams {
    let mut params = ShaderParams {
        grayscale_enabled: 0,
        brightness_factor: 1.0,
        contrast_factor: 1.0,
        saturation_factor: 1.0,
        hue_rotate_angle: 0.0,
        blur_radius: 0.0,
        glow_intensity: 0.0,
        glow_radius: 0.0,
        glow_threshold: 0.5,
        film_grain_amount: 0.0,
        film_grain_speed: 1.0,
        film_flicker_amount: 0.0,
        film_flicker_speed: 1.0,
        depth_blur_focus_x: 0.5,
        depth_blur_focus_y: 0.5,
        depth_blur_focus_radius: 0.2,
        depth_blur_near_blur: 0.0,
        depth_blur_far_blur: 0.0,
        depth_blur_use_map: 0,
        flow_amount: 0.0,
        flow_speed: 1.0,
        flow_decay: 0.95,
        time,
        clip_time,
        width: comp_width,
        height: comp_height,
        _padding: [0, 0],
    };

    let registry = get_effects_registry();
    for effect in effects {
        if let Some(meta) = registry.iter().find(|m| m.effect_type == effect.effect_type) {
            for param in &meta.params {
                if param.param_type == "depth_map" {
                    let mut depth_map_str = None;
                    if let Some(ref map) = effect.params {
                        if let Some(val) = map.get(&param.name) {
                            if let Some(s) = val.as_str() {
                                depth_map_str = Some(s);
                            }
                        }
                    }
                    params.set_field(&param.target, depth_map_str, 0.0);
                } else {
                    let val = read_effect_float(effect, &param.name, clip_time, duration, comp_width, comp_height, param.default);
                    params.set_field(&param.target, None, val);
                }
            }
        }
    }
    params
}


// ---------------------------------------------------------------------------
// Expression / keyframe evaluators
// ---------------------------------------------------------------------------

/// Evaluates a JSON value as a single `f32`. The value may be:
/// - A **literal number** — returned directly.
/// - An **expression object** `{ "expression": "..." }` — evaluated via `evalexpr`.
/// - A **keyframe array** `[{ "time": t, "value": v }, ...]` — linearly interpolated.
/// - `null` or unrecognised — returns `default`.
pub fn evaluate_float(value: &serde_json::Value, clip_time: f32, duration: f32, width: u32, height: u32, default: f32) -> f32 {
    // Null → default
    if value.is_null() {
        return default;
    }
    // Literal number
    if let Some(num) = value.as_f64() {
        return num as f32;
    }
    // Expression object: { "expression": "<expr>" }
    if let Some(obj) = value.as_object() {
        if let Some(expr_val) = obj.get("expression") {
            if let Some(expr_str) = expr_val.as_str() {
                return evaluate_simple_expression(expr_str, clip_time, duration, width, height, default);
            }
        }
    }
    // Keyframe array: [{ "time": <f32>, "value": <f32> }, ...]
    if let Some(arr) = value.as_array() {
        if arr.is_empty() {
            return default;
        }
        if arr[0].is_object() && arr[0].get("time").is_some() {
            let mut kfs: Vec<(f32, f32)> = Vec::new();
            for item in arr {
                if let (Some(t_val), Some(v_val)) = (item.get("time"), item.get("value")) {
                    let t = t_val.as_f64().unwrap_or(0.0) as f32;
                    let v = v_val.as_f64().unwrap_or(0.0) as f32;
                    kfs.push((t, v));
                }
            }
            if kfs.is_empty() {
                return default;
            }
            kfs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            // Clamp to first keyframe
            if clip_time <= kfs[0].0 {
                return kfs[0].1;
            }
            // Clamp to last keyframe
            if clip_time >= kfs[kfs.len() - 1].0 {
                return kfs[kfs.len() - 1].1;
            }
            // Linear interpolation between surrounding keyframes
            for window in kfs.windows(2) {
                let kf1 = window[0];
                let kf2 = window[1];
                if clip_time >= kf1.0 && clip_time <= kf2.0 {
                    let progress = (clip_time - kf1.0) / (kf2.0 - kf1.0);
                    return kf1.1 + progress * (kf2.1 - kf1.1);
                }
            }
        }
    }
    default
}

/// Evaluates a JSON value as an `[f32; 2]` vector. The value may be:
/// - A **literal 2-element array** `[x, y]` — returned directly.
/// - A **scalar number** — broadcast to `[n, n]`.
/// - A **keyframe array** `[{ "time": t, "value": [x, y] }, ...]` — linearly interpolated.
/// - An **expression object** with a bracketed pair `"[exprX, exprY]"` — each component evaluated.
/// - `null` or unrecognised — returns `default`.
pub fn evaluate_vec2(value: &serde_json::Value, clip_time: f32, duration: f32, width: u32, height: u32, default: [f32; 2]) -> [f32; 2] {
    // Null → default
    if value.is_null() {
        return default;
    }
    if let Some(arr) = value.as_array() {
        // Literal 2-element array: [x, y]
        if arr.len() == 2 {
            if let (Some(x), Some(y)) = (arr[0].as_f64(), arr[1].as_f64()) {
                return [x as f32, y as f32];
            }
        }
        // Keyframe array: [{ "time": t, "value": [x, y] | n }, ...]
        if !arr.is_empty() && arr[0].is_object() && arr[0].get("time").is_some() {
            let mut kfs: Vec<(f32, [f32; 2])> = Vec::new();
            for item in arr {
                if let (Some(t_val), Some(v_val)) = (item.get("time"), item.get("value")) {
                    let t = t_val.as_f64().unwrap_or(0.0) as f32;
                    if let Some(v_arr) = v_val.as_array() {
                        if v_arr.len() == 2 {
                            let vx = v_arr[0].as_f64().unwrap_or(0.0) as f32;
                            let vy = v_arr[1].as_f64().unwrap_or(0.0) as f32;
                            kfs.push((t, [vx, vy]));
                        }
                    } else if let Some(v_num) = v_val.as_f64() {
                        // Scalar value broadcast to both components
                        kfs.push((t, [v_num as f32, v_num as f32]));
                    }
                }
            }
            if kfs.is_empty() {
                return default;
            }
            kfs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            // Clamp to first keyframe
            if clip_time <= kfs[0].0 {
                return kfs[0].1;
            }
            // Clamp to last keyframe
            if clip_time >= kfs[kfs.len() - 1].0 {
                return kfs[kfs.len() - 1].1;
            }
            // Linear interpolation between surrounding keyframes
            for window in kfs.windows(2) {
                let kf1 = window[0];
                let kf2 = window[1];
                if clip_time >= kf1.0 && clip_time <= kf2.0 {
                    let progress = (clip_time - kf1.0) / (kf2.0 - kf1.0);
                    let rx = kf1.1[0] + progress * (kf2.1[0] - kf1.1[0]);
                    let ry = kf1.1[1] + progress * (kf2.1[1] - kf1.1[1]);
                    return [rx, ry];
                }
            }
        }
    }
    // Scalar number broadcast to both components
    if let Some(num) = value.as_f64() {
        return [num as f32, num as f32];
    }
    // Expression object with bracketed pair: { "expression": "[exprX, exprY]" }
    if let Some(obj) = value.as_object() {
        if let Some(expr_val) = obj.get("expression") {
            if let Some(expr_str) = expr_val.as_str() {
                if expr_str.starts_with('[') && expr_str.ends_with(']') {
                    let inner = &expr_str[1..expr_str.len() - 1];
                    let parts: Vec<&str> = inner.split(',').collect();
                    if parts.len() == 2 {
                        let x = evaluate_simple_expression(parts[0].trim(), clip_time, duration, width, height, default[0]);
                        let y = evaluate_simple_expression(parts[1].trim(), clip_time, duration, width, height, default[1]);
                        return [x, y];
                    }
                }
            }
        }
    }
    default
}

pub fn evaluate_scale_vec2(value: &Option<serde_json::Value>, default: [f32; 2]) -> [f32; 2] {
    if let Some(val) = value {
        if let Some(arr) = val.as_array() {
            if arr.len() == 2 {
                if let (Some(x), Some(y)) = (arr[0].as_f64(), arr[1].as_f64()) {
                    return [x as f32, y as f32];
                }
            }
        } else if let Some(num) = val.as_f64() {
            return [num as f32, num as f32];
        }
    }
    default
}


/// Evaluates a JSON value as an `[f32; 4]` color vector.
pub fn evaluate_vec4(value: &serde_json::Value, clip_time: f32, duration: f32, width: u32, height: u32, default: [f32; 4]) -> [f32; 4] {
    if value.is_null() {
        return default;
    }
    if let Some(arr) = value.as_array() {
        if arr.len() == 4 {
            let r = evaluate_float(&arr[0], clip_time, duration, width, height, default[0]);
            let g = evaluate_float(&arr[1], clip_time, duration, width, height, default[1]);
            let b = evaluate_float(&arr[2], clip_time, duration, width, height, default[2]);
            let a = evaluate_float(&arr[3], clip_time, duration, width, height, default[3]);
            return [r, g, b, a];
        }
        // Keyframes: [{ "time": t, "value": [r,g,b,a] | scalar }, ...]
        if !arr.is_empty() && arr[0].is_object() && arr[0].get("time").is_some() {
            let mut kfs: Vec<(f32, [f32; 4])> = Vec::new();
            for item in arr {
                if let (Some(t_val), Some(v_val)) = (item.get("time"), item.get("value")) {
                    let t = t_val.as_f64().unwrap_or(0.0) as f32;
                    let v = evaluate_vec4(v_val, t, duration, width, height, default);
                    kfs.push((t, v));
                }
            }
            if kfs.is_empty() {
                return default;
            }
            kfs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            if clip_time <= kfs[0].0 {
                return kfs[0].1;
            }
            if clip_time >= kfs[kfs.len() - 1].0 {
                return kfs[kfs.len() - 1].1;
            }
            for window in kfs.windows(2) {
                let kf1 = window[0];
                let kf2 = window[1];
                if clip_time >= kf1.0 && clip_time <= kf2.0 {
                    let progress = (clip_time - kf1.0) / (kf2.0 - kf1.0);
                    return [
                        kf1.1[0] + progress * (kf2.1[0] - kf1.1[0]),
                        kf1.1[1] + progress * (kf2.1[1] - kf1.1[1]),
                        kf1.1[2] + progress * (kf2.1[2] - kf1.1[2]),
                        kf1.1[3] + progress * (kf2.1[3] - kf1.1[3]),
                    ];
                }
            }
        }
    }
    if let Some(num) = value.as_f64() {
        let val_f = num as f32;
        return [val_f, val_f, val_f, val_f];
    }
    default
}

/// Evaluates a JSON value as padding: [top, right, bottom, left]
pub fn evaluate_padding(value: &serde_json::Value, clip_time: f32, duration: f32, width: u32, height: u32, default: [f32; 4]) -> [f32; 4] {
    if value.is_null() {
        return default;
    }
    if let Some(arr) = value.as_array() {
        if arr.len() == 4 {
            let t = evaluate_float(&arr[0], clip_time, duration, width, height, 0.0);
            let r = evaluate_float(&arr[1], clip_time, duration, width, height, 0.0);
            let b = evaluate_float(&arr[2], clip_time, duration, width, height, 0.0);
            let l = evaluate_float(&arr[3], clip_time, duration, width, height, 0.0);
            return [t, r, b, l];
        } else if arr.len() == 2 {
            let v = evaluate_float(&arr[0], clip_time, duration, width, height, 0.0);
            let h = evaluate_float(&arr[1], clip_time, duration, width, height, 0.0);
            return [v, h, v, h];
        }
    }
    let p = evaluate_float(value, clip_time, duration, width, height, default[0]);
    [p, p, p, p]
}

use std::sync::{OnceLock, Mutex};

static BASE_CONTEXT: OnceLock<HashMapContext> = OnceLock::new();
static EXPR_CACHE: OnceLock<Mutex<HashMap<String, evalexpr::Node>>> = OnceLock::new();

fn get_float_helper(val: &evalexpr::Value) -> Result<f64, evalexpr::EvalexprError> {
    if let Ok(f) = val.as_float() {
        Ok(f)
    } else if let Ok(i) = val.as_int() {
        Ok(i as f64)
    } else {
        Err(evalexpr::EvalexprError::expected_number(val.clone()))
    }
}

fn get_base_context() -> &'static HashMapContext {
    BASE_CONTEXT.get_or_init(|| {
        let mut context = HashMapContext::new();
        let _ = context.set_value("pi".into(), (std::f64::consts::PI).into());
        let _ = context.set_function("pi".into(), evalexpr::Function::new(|_argument| {
            Ok(evalexpr::Value::Float(std::f64::consts::PI))
        }));

        let _ = context.set_function("sin".into(), evalexpr::Function::new(|argument| {
            let val = get_float_helper(argument)?;
            Ok(evalexpr::Value::Float(val.sin()))
        }));

        let _ = context.set_function("cos".into(), evalexpr::Function::new(|argument| {
            let val = get_float_helper(argument)?;
            Ok(evalexpr::Value::Float(val.cos()))
        }));

        let _ = context.set_function("tan".into(), evalexpr::Function::new(|argument| {
            let val = get_float_helper(argument)?;
            Ok(evalexpr::Value::Float(val.tan()))
        }));

        let _ = context.set_function("abs".into(), evalexpr::Function::new(|argument| {
            let val = get_float_helper(argument)?;
            Ok(evalexpr::Value::Float(val.abs()))
        }));

        let _ = context.set_function("sqrt".into(), evalexpr::Function::new(|argument| {
            let val = get_float_helper(argument)?;
            Ok(evalexpr::Value::Float(val.sqrt()))
        }));

        let _ = context.set_function("pow".into(), evalexpr::Function::new(|argument| {
            let tuple = argument.as_tuple()?;
            if tuple.len() != 2 {
                return Err(evalexpr::EvalexprError::CustomMessage(format!(
                    "pow expects exactly 2 arguments, got {}",
                    tuple.len()
                )));
            }
            let base = get_float_helper(&tuple[0])?;
            let exponent = get_float_helper(&tuple[1])?;
            Ok(evalexpr::Value::Float(base.powf(exponent)))
        }));
        context
    })
}

fn build_eval_context(clip_time: f32, duration: f32, width: u32, height: u32) -> HashMapContext {
    let mut context = get_base_context().clone();
    let _ = context.set_value("time".into(), (clip_time as f64).into());
    let _ = context.set_value("clip_time".into(), (clip_time as f64).into());
    let _ = context.set_value("clip_duration".into(), (duration as f64).into());
    let _ = context.set_value("comp_width".into(), (width as i64).into());
    let _ = context.set_value("comp_height".into(), (height as i64).into());
    context
}

/// Evaluates a single math expression string via `evalexpr`
/// with `time`,
/// `clip_time`, `clip_duration`, `comp_width`, `comp_height`, and `pi` available as variables.
/// Falls back to a plain `f32::parse` if the expression engine can't handle it.
pub fn evaluate_simple_expression(expr: &str, clip_time: f32, duration: f32, width: u32, height: u32, default: f32) -> f32 {
    // Replace dots only in recognized variable names to avoid mangling decimal literals.
    let cleaned_expr = expr
        .replace("comp.width", "comp_width")
        .replace("comp.height", "comp_height")
        .replace("clip.time", "clip_time")
        .replace("clip.duration", "clip_duration");

    let cache = EXPR_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache_guard = cache.lock().unwrap();
    let compiled = if let Some(node) = cache_guard.get(&cleaned_expr) {
        Some(node.clone())
    } else {
        match evalexpr::build_operator_tree(&cleaned_expr) {
            Ok(node) => {
                cache_guard.insert(cleaned_expr.clone(), node.clone());
                Some(node)
            }
            Err(e) => {
                log::debug!("evalexpr::build_operator_tree failed for '{}': {:?}", cleaned_expr, e);
                None
            }
        }
    };
    drop(cache_guard);

    if let Some(node) = compiled {
        let context = build_eval_context(clip_time, duration, width, height);
        match node.eval_with_context(&context) {
            Ok(evalexpr::Value::Float(result)) => return result as f32,
            Ok(evalexpr::Value::Int(result)) => return result as f32,
            Ok(other) => log::warn!("evalexpr Node returned non-numeric value: {:?}", other),
            Err(e) => log::debug!("evalexpr Node eval failed for '{}': {:?}", cleaned_expr, e),
        }
    }

    // Fallback: try parsing the raw string as a number
    expr.parse::<f32>().unwrap_or(default)
}
