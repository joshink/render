//! Render spec deserialization and runtime expression / keyframe evaluation.
//!
//! This module defines the data structures that map to the JSON render
//! specification, and provides evaluation functions for dynamic properties
//! including literal values, `evalexpr` expressions, and keyframe arrays
//! with linear interpolation.

use serde::Deserialize;
use std::collections::HashMap;
use evalexpr::{eval_with_context, ContextWithMutableVariables, HashMapContext};

// ---------------------------------------------------------------------------
// Movie-mode oscillation constants
// ---------------------------------------------------------------------------

/// Amplitude of the sinusoidal brightness oscillation in movie mode.
const BRIGHTNESS_OSCILLATION_AMPLITUDE: f32 = 0.8;

/// Frequency multiplier (cycles per second, before the 2π factor) for
/// the brightness oscillation in movie mode.
const BRIGHTNESS_OSCILLATION_FREQ: f32 = 2.0;

// ---------------------------------------------------------------------------
// Data model — deserialized from the JSON render spec
// ---------------------------------------------------------------------------

/// Root specification for a render job, containing composition settings,
/// assets, tracks, and optional audio tracks.
#[derive(Deserialize, Debug, Clone)]
pub struct RenderSpec {
    pub version: String,
    pub output: String,
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
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default] Normal,
    Multiply, Screen, Overlay, Darken, Lighten,
    ColorDodge, ColorBurn, HardLight, SoftLight,
    Difference, Exclusion,
}

impl BlendMode {
    pub fn as_u32(self) -> u32 { self as u32 }
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
    pub text: String,
    pub font: String,
    pub font_size: serde_json::Value,
    pub color: [f32; 4],
    #[serde(default)]
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
    pub shader: String,
    pub start: f32,
    pub duration: f32,
    pub from: String,
    pub to: String,
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
/// expressions and keyframes. Returns `default` if the key is absent.
fn read_effect_float(effect: &Effect, key: &str, clip_time: f32, w: u32, h: u32, default: f32) -> f32 {
    effect.params.as_ref()
        .and_then(|map| map.get(key))
        .map(|val| evaluate_float(val, clip_time, w, h, default))
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
                                let f = evaluate_float(val, clip_time, w, h, 0.0);
                                serde_json::Value::from(f)
                            }
                            "vec2" => {
                                let v = evaluate_vec2(val, clip_time, w, h, [0.0, 0.0]);
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
                                let new_val = self.substitute_value(val, &resolved_inputs, clip_time, w, h);
                                resolved_params.insert(key.clone(), new_val);
                            }
                            resolved_filter.params = Some(resolved_params);
                        }
                        sub_filters.push(resolved_filter);
                    }

                    let expanded_sub = self.expand_effects(&sub_filters, clip_time, w, h, depth + 1);
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
                            serde_json::Value::Number(num) => num.to_string(),
                            serde_json::Value::Bool(b) => b.to_string(),
                            serde_json::Value::Array(arr) => {
                                format!("{:?}", arr)
                            }
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
                    new_obj.insert(k.clone(), self.substitute_value(v, resolved_inputs, clip_time, w, h));
                }
                serde_json::Value::Object(new_obj)
            }
            serde_json::Value::Array(arr) => {
                let mut new_arr = Vec::new();
                for item in arr {
                    new_arr.push(self.substitute_value(item, resolved_inputs, clip_time, w, h));
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

    pub fn get_audio_clips(&self) -> Vec<(String, f32, f32)> {
        let mut list = Vec::new();
        if let Some(ref tracks) = self.audio_tracks {
            for track in tracks {
                let start_times = track.get_clip_start_times();
                for (idx, clip) in track.clips.iter().enumerate() {
                    let absolute_start = start_times[idx];
                    if let Some(asset) = self.assets.get(&clip.asset) {
                        match asset {
                            Asset::Audio { path } => {
                                list.push((path.clone(), absolute_start, clip.duration));
                            }
                            Asset::Video { path } => {
                                list.push((path.clone(), absolute_start, clip.duration));
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
        let is_movie = self.output.ends_with(".mp4");

        events.push((0.0, "Composition start".to_string()));
        events.push((self.composition.duration, "Composition end".to_string()));

        for track in &self.tracks {
            let start_times = track.get_clip_start_times();
            for (idx, clip) in track.clips.iter().enumerate() {
                let clip_start = start_times[idx];
                let clip_end = clip_start + clip.duration;

                events.push((clip_start, format!("Track '{}' - Clip '{}' starts", track.id, clip.id)));
                events.push((clip_end, format!("Track '{}' - Clip '{}' ends", track.id, clip.id)));

                if is_movie {
                    for effect in &clip.effects {
                        match effect.effect_type.as_str() {
                            "grayscale" => {
                                let mut t = 0.5;
                                while t < clip.duration {
                                    let abs_time = clip_start + t;
                                    let is_on = (t * std::f32::consts::PI).sin() > 0.0;
                                    events.push((
                                        abs_time,
                                        format!(
                                            "Track '{}' - Clip '{}' - Grayscale is {}",
                                            track.id, clip.id, if is_on { "ON" } else { "OFF" }
                                        ),
                                    ));
                                    t += 1.0;
                                }
                            }
                            "brightness" => {
                                let mut t = 0.25;
                                while t < clip.duration {
                                    events.push((
                                        clip_start + t,
                                        format!("Track '{}' - Clip '{}' - Brightness at peak (factor oscillation)", track.id, clip.id),
                                    ));
                                    t += 1.0;
                                }
                                let mut t = 0.75;
                                while t < clip.duration {
                                    events.push((
                                        clip_start + t,
                                        format!("Track '{}' - Clip '{}' - Brightness at trough (factor oscillation)", track.id, clip.id),
                                    ));
                                    t += 1.0;
                                }
                            }
                            _ => {}
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
            .map(|v| evaluate_vec2(v, t, w, h, [w as f32 * 0.5, h as f32 * 0.5]))
            .unwrap_or([w as f32 * 0.5, h as f32 * 0.5])
    }

    pub fn eval_scale(&self, t: f32, w: u32, h: u32) -> [f32; 2] {
        self.transform.as_ref()
            .and_then(|tr| tr.scale.as_ref())
            .map(|v| evaluate_vec2(v, t, w, h, [1.0, 1.0]))
            .unwrap_or([1.0, 1.0])
    }

    pub fn eval_rotation(&self, t: f32, w: u32, h: u32) -> f32 {
        self.transform.as_ref()
            .and_then(|tr| tr.rotation.as_ref())
            .map(|v| evaluate_float(v, t, w, h, 0.0))
            .unwrap_or(0.0)
    }

    pub fn eval_opacity(&self, t: f32, w: u32, h: u32) -> f32 {
        self.transform.as_ref()
            .and_then(|tr| tr.opacity.as_ref())
            .map(|v| evaluate_float(v, t, w, h, 1.0))
            .unwrap_or(1.0)
    }

    pub fn eval_built_in_effects(&self, clip_time: f32, is_movie: bool) -> (u32, f32) {
        eval_built_in_effects_from_effects(&self.effects, clip_time, is_movie)
    }

    pub fn get_depth_map_asset_id(&self) -> Option<String> {
        get_depth_map_asset_id_from_effects(&self.effects)
    }

    pub fn eval_shader_params(&self, clip_time: f32, comp_width: u32, comp_height: u32, time: f32, is_movie: bool) -> ShaderParams {
        eval_shader_params_from_effects(&self.effects, clip_time, comp_width, comp_height, time, is_movie)
    }
}

pub fn eval_built_in_effects_from_effects(effects: &[Effect], clip_time: f32, is_movie: bool) -> (u32, f32) {
    let mut grayscale = 0u32;
    let mut brightness = 1.0f32;
    for effect in effects {
        match effect.effect_type.as_str() {
            "grayscale" => {
                grayscale = 1;
                if is_movie {
                    grayscale = if (clip_time * std::f32::consts::PI).sin() > 0.0 { 1 } else { 0 };
                }
            }
            "brightness" => {
                if let Some(ref params) = effect.params {
                    if let Some(factor_val) = params.get("factor") {
                        if let Some(factor) = factor_val.as_f64() {
                            brightness = factor as f32;
                        }
                    }
                }
                if is_movie {
                    brightness = brightness
                        * (1.0 + BRIGHTNESS_OSCILLATION_AMPLITUDE
                            * (clip_time * BRIGHTNESS_OSCILLATION_FREQ * std::f32::consts::PI).sin());
                }
            }
            _ => {}
        }
    }
    (grayscale, brightness)
}

pub fn get_depth_map_asset_id_from_effects(effects: &[Effect]) -> Option<String> {
    for effect in effects {
        if effect.effect_type == "depth_blur" {
            if let Some(ref params) = effect.params {
                if let Some(val) = params.get("depth_map") {
                    if let Some(s) = val.as_str() {
                        return Some(s.to_string());
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
    comp_width: u32,
    comp_height: u32,
    time: f32,
    is_movie: bool,
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

    for effect in effects {
        match effect.effect_type.as_str() {
            "grayscale" => {
                let mut enabled = read_effect_float(effect, "enabled", clip_time, comp_width, comp_height, 1.0) > 0.5;
                if is_movie {
                    enabled = enabled && (clip_time * std::f32::consts::PI).sin() > 0.0;
                }
                params.grayscale_enabled = if enabled { 1 } else { 0 };
            }
            "brightness" => {
                let mut factor = read_effect_float(effect, "factor", clip_time, comp_width, comp_height, 1.0);
                if is_movie {
                    factor = factor
                        * (1.0 + BRIGHTNESS_OSCILLATION_AMPLITUDE
                            * (clip_time * BRIGHTNESS_OSCILLATION_FREQ * std::f32::consts::PI).sin());
                }
                params.brightness_factor = factor;
            }
            "contrast" => {
                params.contrast_factor = read_effect_float(effect, "factor", clip_time, comp_width, comp_height, 1.0);
            }
            "saturation" => {
                params.saturation_factor = read_effect_float(effect, "factor", clip_time, comp_width, comp_height, 1.0);
            }
            "hue_rotate" => {
                params.hue_rotate_angle = read_effect_float(effect, "angle", clip_time, comp_width, comp_height, 0.0);
            }
            "blur" => {
                params.blur_radius = read_effect_float(effect, "radius", clip_time, comp_width, comp_height, 0.0);
            }
            "glow" => {
                params.glow_intensity = read_effect_float(effect, "intensity", clip_time, comp_width, comp_height, 0.0);
                params.glow_radius = read_effect_float(effect, "radius", clip_time, comp_width, comp_height, 0.0);
                params.glow_threshold = read_effect_float(effect, "threshold", clip_time, comp_width, comp_height, 0.5);
            }
            "film_grain" => {
                params.film_grain_amount = read_effect_float(effect, "amount", clip_time, comp_width, comp_height, 0.0);
                params.film_grain_speed = read_effect_float(effect, "speed", clip_time, comp_width, comp_height, 1.0);
            }
            "film_flicker" => {
                params.film_flicker_amount = read_effect_float(effect, "amount", clip_time, comp_width, comp_height, 0.0);
                params.film_flicker_speed = read_effect_float(effect, "speed", clip_time, comp_width, comp_height, 1.0);
            }
            "depth_blur" => {
                params.depth_blur_focus_x = read_effect_float(effect, "focus_x", clip_time, comp_width, comp_height, 0.5);
                params.depth_blur_focus_y = read_effect_float(effect, "focus_y", clip_time, comp_width, comp_height, 0.5);
                params.depth_blur_focus_radius = read_effect_float(effect, "focus_radius", clip_time, comp_width, comp_height, 0.2);
                params.depth_blur_near_blur = read_effect_float(effect, "near_blur", clip_time, comp_width, comp_height, 0.0);
                params.depth_blur_far_blur = read_effect_float(effect, "far_blur", clip_time, comp_width, comp_height, 0.0);
                if let Some(ref map) = effect.params {
                    if let Some(val) = map.get("depth_map") {
                        if val.as_str().is_some() {
                            params.depth_blur_use_map = 1;
                        }
                    }
                }
            }
            "flow" => {
                params.flow_amount = read_effect_float(effect, "amount", clip_time, comp_width, comp_height, 0.0);
                params.flow_speed = read_effect_float(effect, "speed", clip_time, comp_width, comp_height, 1.0);
                params.flow_decay = read_effect_float(effect, "decay", clip_time, comp_width, comp_height, 0.95);
            }
            _ => {}
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
pub fn evaluate_float(value: &serde_json::Value, clip_time: f32, width: u32, height: u32, default: f32) -> f32 {
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
                return evaluate_simple_expression(expr_str, clip_time, width, height, default);
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
pub fn evaluate_vec2(value: &serde_json::Value, clip_time: f32, width: u32, height: u32, default: [f32; 2]) -> [f32; 2] {
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
                        let x = evaluate_simple_expression(parts[0].trim(), clip_time, width, height, default[0]);
                        let y = evaluate_simple_expression(parts[1].trim(), clip_time, width, height, default[1]);
                        return [x, y];
                    }
                }
            }
        }
    }
    default
}

/// Evaluates a single math expression string via `evalexpr`, with `time`,
/// `clip_time`, `comp_width`, `comp_height`, and `pi` available as variables.
/// Falls back to a plain `f32::parse` if the expression engine can't handle it.
pub fn evaluate_simple_expression(expr: &str, clip_time: f32, width: u32, height: u32, default: f32) -> f32 {
    // evalexpr uses underscores for member access; replace dots so that
    // decimal literals like "0.5" become "0_5" (handled by the engine).
    let cleaned_expr = expr.replace(".", "_");
    let mut context = HashMapContext::new();
    let _ = context.set_value("time".into(), (clip_time as f64).into());
    let _ = context.set_value("clip_time".into(), (clip_time as f64).into());
    let _ = context.set_value("comp_width".into(), (width as i64).into());
    let _ = context.set_value("comp_height".into(), (height as i64).into());
    let _ = context.set_value("pi".into(), (std::f64::consts::PI).into());

    // Try evaluating as a float expression first
    if let Ok(evalexpr::Value::Float(result)) = eval_with_context(&cleaned_expr, &context) {
        return result as f32;
    }
    // Integer expressions (e.g. "comp_width / 2") yield an Int
    if let Ok(evalexpr::Value::Int(result)) = eval_with_context(&cleaned_expr, &context) {
        return result as f32;
    }

    // Fallback: try parsing the raw string as a number
    expr.parse::<f32>().unwrap_or(default)
}
