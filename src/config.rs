//! Render spec deserialization and runtime expression / keyframe evaluation.
//!
//! This module defines the data structures that map to the JSON render
//! specification, and provides evaluation functions for dynamic properties
//! including literal values, `evalexpr` expressions, and keyframe arrays
//! with linear interpolation.

use serde::Deserialize;
use std::collections::HashMap;
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

#[derive(Deserialize, Debug, Clone)]
pub struct TransitionMetadata {
    #[serde(rename = "type")]
    pub transition_type: String,
    pub params: Vec<EffectParamMetadata>,
    #[serde(default)]
    pub spot_checks: Vec<SpotCheckMetadata>,
}

/// Effect/transition metadata extracted from the WGSL shaders compiled for a
/// single render. Owned by the render's pipeline state (not a process-wide
/// static) so concurrent jobs — which may declare same-named custom shaders
/// with different parameter layouts — can never observe each other's metadata.
#[derive(Debug, Clone, Default)]
pub struct ShaderRegistry {
    pub effects: Vec<EffectMetadata>,
    pub transitions: Vec<TransitionMetadata>,
}

impl ShaderRegistry {
    /// Registers effect metadata, replacing any existing entry of the same type.
    pub fn register_effect(&mut self, meta: EffectMetadata) {
        if let Some(existing) = self.effects.iter_mut().find(|m| m.effect_type == meta.effect_type) {
            *existing = meta;
        } else {
            self.effects.push(meta);
        }
    }

    /// Registers transition metadata, replacing any existing entry of the same type.
    pub fn register_transition(&mut self, meta: TransitionMetadata) {
        if let Some(existing) = self.transitions.iter_mut().find(|m| m.transition_type == meta.transition_type) {
            *existing = meta;
        } else {
            self.transitions.push(meta);
        }
    }

    pub fn find_effect(&self, effect_type: &str) -> Option<&EffectMetadata> {
        self.effects.iter().find(|m| m.effect_type == effect_type)
    }

    pub fn find_transition(&self, transition_type: &str) -> Option<&TransitionMetadata> {
        self.transitions.iter().find(|m| m.transition_type == transition_type)
    }
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

/// Remote output destination schemes accepted by the pipeline. The single
/// source of truth shared by the upload dispatch, the serve-mode destination
/// validation, and the CLI's mux handling.
pub const REMOTE_SCHEMES: [&str; 5] = ["s3://", "gs://", "mux://", "http://", "https://"];

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

    /// True when the output lands somewhere remote (upload required) rather
    /// than on the local filesystem.
    pub fn is_remote(&self) -> bool {
        let p = self.path();
        REMOTE_SCHEMES.iter().any(|scheme| p.starts_with(scheme))
    }

    pub fn is_mux(&self) -> bool {
        self.path().starts_with("mux://")
    }

    /// True when the render produces a video. Mux only ingests video, so a
    /// `mux://` destination always implies an `.mp4` render regardless of the
    /// (path-less) scheme.
    pub fn is_movie(&self) -> bool {
        self.is_mux() || self.clean_path().ends_with(".mp4")
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
    #[serde(rename = "lut")]
    Lut { path: String },
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
            let implicit_start = self.clips.iter()
                .position(|c| c.id == tr.to)
                .map(|idx| clip_starts[idx] - tr.duration * 0.5);
            // Unreachable for specs that passed `validate_references`; guards
            // against directly constructed Tracks.
            let Some(start_time) = tr.start.or(implicit_start) else {
                log::error!(
                    "Transition '{}' in track '{}' references unknown clip '{}'; skipping it",
                    tr.id, self.id, tr.to
                );
                continue;
            };
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
    /// Validates every cross-reference in the spec — clip → asset, transition
    /// → clip, effect/clip → preset, text → font asset — and reports all
    /// broken references at once, so a bad spec fails fast with a complete
    /// list instead of silently rendering wrong output (missing media becomes
    /// transparent, missing presets/fonts/audio are dropped).
    pub fn validate_references(&self) -> Result<(), String> {
        let mut errors = Vec::new();

        for track in &self.tracks {
            let clip_ids: std::collections::HashSet<&str> =
                track.clips.iter().map(|c| c.id.as_str()).collect();

            for clip in &track.clips {
                let context = format!("track '{}', clip '{}'", track.id, clip.id);
                if let Some(asset_id) = &clip.asset {
                    match self.assets.get(asset_id) {
                        None => errors.push(format!(
                            "{context}: references unknown asset '{asset_id}'"
                        )),
                        Some(asset) => {
                            if clip.clip_type == ClipType::Media
                                && !matches!(asset, Asset::Video { .. } | Asset::Image { .. })
                            {
                                errors.push(format!(
                                    "{context}: media clip references asset '{asset_id}', which is not a video or image"
                                ));
                            }
                        }
                    }
                }
                if let Some(preset) = &clip.preset {
                    self.check_preset_ref(preset, &context, &mut errors, &mut Vec::new());
                }
                self.check_effect_presets(&clip.effects, &context, &mut errors, &mut Vec::new());
                if let Some(text) = &clip.text_params {
                    if let Some(font) = &text.font {
                        self.check_font_ref(font, &context, &mut errors);
                    }
                    if let Some(body) = &text.body {
                        self.check_layout_fonts(body, &context, &mut errors);
                    }
                }
            }

            for tr in &track.transitions {
                for (field, target) in [("from", &tr.from), ("to", &tr.to)] {
                    if !clip_ids.contains(target.as_str()) {
                        let listed: Vec<&str> =
                            track.clips.iter().map(|c| c.id.as_str()).collect();
                        errors.push(format!(
                            "track '{}': transition '{}' `{field}` references unknown clip '{target}' (clips in track: {})",
                            track.id,
                            tr.id,
                            if listed.is_empty() { "<none>".to_string() } else { listed.join(", ") }
                        ));
                    }
                }
            }
        }

        if let Some(audio_tracks) = &self.audio_tracks {
            for track in audio_tracks {
                for clip in &track.clips {
                    match self.assets.get(&clip.asset) {
                        None => errors.push(format!(
                            "audio track '{}', clip '{}': references unknown asset '{}'",
                            track.id, clip.id, clip.asset
                        )),
                        Some(Asset::Audio { .. }) | Some(Asset::Video { .. }) => {}
                        Some(_) => errors.push(format!(
                            "audio track '{}', clip '{}': asset '{}' is not an audio or video asset",
                            track.id, clip.id, clip.asset
                        )),
                    }
                }
            }
        }

        // Presets nest: validate references inside every definition too, so a
        // broken inner reference is caught even before some clip uses it.
        if let Some(presets) = &self.presets {
            for def in presets {
                self.check_effect_presets(
                    &def.filters,
                    &format!("preset '{}'", def.name),
                    &mut errors,
                    &mut vec![def.name.clone()],
                );
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Spec validation failed with {} broken reference{}:\n  - {}",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" },
                errors.join("\n  - ")
            ))
        }
    }

    fn check_font_ref(&self, font_id: &str, context: &str, errors: &mut Vec<String>) {
        // An empty/omitted font falls back to the built-in approximation.
        if font_id.is_empty() {
            return;
        }
        match self.assets.get(font_id) {
            None => errors.push(format!(
                "{context}: references unknown font asset '{font_id}'"
            )),
            Some(Asset::Font { .. }) => {}
            Some(_) => errors.push(format!(
                "{context}: asset '{font_id}' is used as a font but is not a font asset"
            )),
        }
    }

    fn check_layout_fonts(&self, node: &LayoutNode, context: &str, errors: &mut Vec<String>) {
        if let Some(font) = &node.font {
            self.check_font_ref(font, context, errors);
        }
        if let Some(children) = &node.children {
            for child in children {
                self.check_layout_fonts(child, context, errors);
            }
        }
    }

    fn check_effect_presets(
        &self,
        effects: &[Effect],
        context: &str,
        errors: &mut Vec<String>,
        visiting: &mut Vec<String>,
    ) {
        for effect in effects {
            if effect.effect_type == "preset" || effect.preset.is_some() {
                let name = effect.preset.as_ref().unwrap_or(&effect.effect_type);
                self.check_preset_ref(name, context, errors, visiting);
            }
        }
    }

    fn check_preset_ref(
        &self,
        name: &str,
        context: &str,
        errors: &mut Vec<String>,
        visiting: &mut Vec<String>,
    ) {
        // Both cases below are hard errors because expand_effects truncates
        // at depth > 5 at render time, silently dropping the chain's effects.
        if visiting.iter().any(|n| n == name) {
            errors.push(format!(
                "{context}: preset cycle detected ({} → {name})",
                visiting.join(" → ")
            ));
            return;
        }
        if visiting.len() > 5 {
            errors.push(format!(
                "{context}: preset nesting deeper than 5 levels ({} → {name})",
                visiting.join(" → ")
            ));
            return;
        }
        let def = self
            .presets
            .as_ref()
            .and_then(|list| list.iter().find(|p| p.name == name));
        match def {
            None => {
                let known: Vec<&str> = self
                    .presets
                    .as_ref()
                    .map(|l| l.iter().map(|p| p.name.as_str()).collect())
                    .unwrap_or_default();
                errors.push(format!(
                    "{context}: references unknown preset '{name}'{}",
                    if known.is_empty() {
                        String::new()
                    } else {
                        format!(" (known presets: {})", known.join(", "))
                    }
                ));
            }
            Some(def) => {
                visiting.push(name.to_string());
                self.check_effect_presets(
                    &def.filters,
                    &format!("preset '{}'", def.name),
                    errors,
                    visiting,
                );
                visiting.pop();
            }
        }
    }

    /// Collects every shader pipeline id this spec can dispatch at render
    /// time — effect shaders, Effect-clip shaders, and transition shaders,
    /// with presets expanded statically — paired with a context string for
    /// error messages. Used after pipeline compilation to verify the compiled
    /// set covers the spec before the frame loop starts.
    pub fn collect_required_shader_ids(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for track in &self.tracks {
            for clip in &track.clips {
                let context = format!("track '{}', clip '{}'", track.id, clip.id);
                if clip.clip_type == ClipType::Effect {
                    if let Some(shader) = &clip.shader {
                        out.push((context.clone(), shader.clone()));
                    } else if let Some(preset) = &clip.preset {
                        self.collect_preset_shader_ids(preset, &context, &mut out, &mut Vec::new());
                    }
                }
                self.collect_effect_shader_ids(&clip.effects, &context, &mut out, &mut Vec::new());
            }
            for tr in &track.transitions {
                let id = tr.shader.clone().unwrap_or_else(|| tr.transition_type.clone());
                out.push((format!("track '{}', transition '{}'", track.id, tr.id), id));
            }
        }
        out
    }

    fn collect_effect_shader_ids(
        &self,
        effects: &[Effect],
        context: &str,
        out: &mut Vec<(String, String)>,
        visiting: &mut Vec<String>,
    ) {
        for effect in effects {
            if effect.effect_type == "preset" || effect.preset.is_some() {
                let name = effect.preset.as_ref().unwrap_or(&effect.effect_type);
                self.collect_preset_shader_ids(name, context, out, visiting);
            } else {
                let id = effect.shader.clone().unwrap_or_else(|| effect.effect_type.clone());
                out.push((context.to_string(), id));
            }
        }
    }

    fn collect_preset_shader_ids(
        &self,
        name: &str,
        context: &str,
        out: &mut Vec<(String, String)>,
        visiting: &mut Vec<String>,
    ) {
        // Unknown presets, cycles, and over-deep nesting are already reported
        // by `validate_references`; just stop descending here.
        if visiting.iter().any(|n| n == name) || visiting.len() > 5 {
            return;
        }
        let Some(def) = self
            .presets
            .as_ref()
            .and_then(|list| list.iter().find(|p| p.name == name))
        else {
            return;
        };
        visiting.push(name.to_string());
        self.collect_effect_shader_ids(&def.filters, context, out, visiting);
        visiting.pop();
    }

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
            // Unreachable for specs that passed `validate_references`, which
            // rejects cycles and >5-level nesting; guards direct construction.
            log::error!("Preset nesting exceeded depth 5; dropping the remaining effects");
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
                    match self.assets.get(&clip.asset) {
                        Some(Asset::Audio { path }) | Some(Asset::Video { path }) => {
                            list.push((path.clone(), absolute_start, clip.duration, clip.trim_start));
                        }
                        Some(_) => log::warn!(
                            "Audio clip '{}' references asset '{}', which is not audio or video; skipping it",
                            clip.id, clip.asset
                        ),
                        None => log::warn!(
                            "Audio clip '{}' references unknown asset '{}'; skipping it",
                            clip.id, clip.asset
                        ),
                    }
                }
            }
        }
        list
    }

    pub fn get_spot_check_events(&self, registry: &ShaderRegistry) -> Vec<(f32, String)> {
        let mut events = Vec::new();
        let is_movie = self.output.is_movie();

        events.push((0.0, "Composition start".to_string()));
        events.push((self.composition.duration, "Composition end".to_string()));

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
                        if let Some(meta) = registry.find_effect(&effect.effect_type) {
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
                    if let Some(meta) = registry.find_transition(tr_shader) {
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

/// Resolves the LUT asset id referenced by an effect whose metadata declares a
/// `lut`-typed parameter. Mirrors [`get_depth_map_asset_id_from_effects`].
pub fn get_lut_asset_id_from_effects(registry: &ShaderRegistry, effects: &[Effect]) -> Option<String> {
    for effect in effects {
        if let Some(meta) = registry.find_effect(&effect.effect_type) {
            for param in &meta.params {
                if param.param_type == "lut" {
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

pub fn get_depth_map_asset_id_from_effects(registry: &ShaderRegistry, effects: &[Effect]) -> Option<String> {
    for effect in effects {
        if let Some(meta) = registry.find_effect(&effect.effect_type) {
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

/// Recursively scans any JSON value (whether in a Transform, Effect, or LayoutNode)
/// and pre-sorts keyframe arrays by the "time" field.
pub fn sort_value_keyframes(val: &mut serde_json::Value) {
    match val {
        serde_json::Value::Array(arr) => {
            if !arr.is_empty() && arr[0].is_object() && arr[0].get("time").is_some() {
                arr.sort_by(|a, b| {
                    let t_a = a.get("time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    let t_b = b.get("time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    t_a.partial_cmp(&t_b).unwrap_or(std::cmp::Ordering::Equal)
                });
            } else {
                for item in arr {
                    sort_value_keyframes(item);
                }
            }
        }
        serde_json::Value::Object(obj) => {
            for (_, v) in obj {
                sort_value_keyframes(v);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Expression / keyframe evaluators
// ---------------------------------------------------------------------------


/// Maps a normalised progress `t` ∈ [0,1] through a named easing curve.
/// Recognises the documented set (`linear`, `ease_in`, `ease_out`,
/// `ease_in_out`); unknown names fall back to linear.
fn apply_named_easing(t: f32, easing: &str) -> f32 {
    match easing.to_ascii_lowercase().as_str() {
        "ease_in" | "ease-in" => t * t,
        "ease_out" | "ease-out" => t * (2.0 - t),
        "ease_in_out" | "ease-in-out" => t * t * (3.0 - 2.0 * t),
        _ => t, // "linear" or unknown
    }
}

/// Evaluates a CSS-style `cubic-bezier(x1, y1, x2, y2)` easing curve with fixed
/// endpoints P0=(0,0) and P3=(1,1). Given an input progress `t` (the x value),
/// solves for the curve parameter `s` such that x(s)=t via Newton–Raphson, then
/// returns the corresponding y(s).
fn cubic_bezier_ease(t: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    // Cubic Bézier component with p0=0, p3=1 and the two given control points.
    let bezier = |s: f32, c1: f32, c2: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * c1 + 3.0 * u * s * s * c2 + s * s * s
    };
    let bezier_dx = |s: f32, c1: f32, c2: f32| {
        let u = 1.0 - s;
        3.0 * u * u * c1 + 6.0 * u * s * (c2 - c1) + 3.0 * s * s * (1.0 - c2)
    };
    // Newton–Raphson, seeded at s=t; the x-curve is monotonic for valid control
    // points so a handful of iterations converges tightly.
    let mut s = t;
    for _ in 0..8 {
        let x = bezier(s, x1, x2) - t;
        if x.abs() < 1e-5 {
            break;
        }
        let dx = bezier_dx(s, x1, x2);
        if dx.abs() < 1e-6 {
            break;
        }
        s = (s - x / dx).clamp(0.0, 1.0);
    }
    bezier(s, y1, y2)
}

/// Resolves the eased progress for a keyframe segment. `easing` is the optional
/// `"easing"` field on the *segment-start* keyframe and may be a string
/// (`"ease_out"`) or a 4-element cubic-bezier array `[x1, y1, x2, y2]`.
fn apply_keyframe_easing(progress: f32, easing: Option<&serde_json::Value>) -> f32 {
    match easing {
        Some(serde_json::Value::String(s)) => apply_named_easing(progress, s),
        Some(serde_json::Value::Array(arr)) if arr.len() == 4 => {
            let c: Vec<f32> = arr.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
            cubic_bezier_ease(progress, c[0], c[1], c[2], c[3])
        }
        _ => progress,
    }
}

/// Minimum keyframe segment span used to avoid division by zero when two
/// keyframes share (or nearly share) the same timestamp.
const MIN_KEYFRAME_SPAN: f32 = 0.0001;

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + t * (b - a)
}

/// Shared keyframe-interpolation core behind [`evaluate_float`],
/// [`evaluate_vec2`], and [`evaluate_vec4`].
///
/// Expects `arr` to be a keyframe array (`[{ "time": t, "value": v, "easing"?: e }, ...]`)
/// and returns `None` if it isn't shaped like one — callers fall back to their
/// other interpretations (literal, scalar broadcast, expression) or default.
/// `parse_value` decodes a keyframe's `"value"` field into `T`; `lerp_t`
/// interpolates between two decoded values.
///
/// Semantics: a single keyframe is a constant; `clip_time` before the first /
/// after the last keyframe clamps to that keyframe's value; otherwise the
/// surrounding pair is found and interpolated, with the easing taken from the
/// segment-start keyframe (see [`apply_keyframe_easing`]). Malformed
/// keyframes are skipped, matching the historical lenient behaviour.
fn evaluate_keyframes<T: Copy>(
    arr: &[serde_json::Value],
    clip_time: f32,
    parse_value: impl Fn(&serde_json::Value) -> Option<T>,
    lerp_t: impl Fn(T, T, f32) -> T,
) -> Option<T> {
    if arr.is_empty() || !arr[0].is_object() || arr[0].get("time").is_none() {
        return None;
    }
    let get_kf = |item: &serde_json::Value| -> Option<(f32, T)> {
        let t = item.get("time")?.as_f64()? as f32;
        let v = parse_value(item.get("value")?)?;
        Some((t, v))
    };

    if arr.len() == 1 {
        return get_kf(&arr[0]).map(|(_, v)| v);
    }

    // Clamp outside the keyframed range.
    if let Some((t0, v0)) = get_kf(&arr[0]) {
        if clip_time <= t0 {
            return Some(v0);
        }
    }
    if let Some((tn, vn)) = get_kf(&arr[arr.len() - 1]) {
        if clip_time >= tn {
            return Some(vn);
        }
    }

    // Find the segment containing clip_time and interpolate within it.
    for i in 0..arr.len() - 1 {
        if let (Some((t1, v1)), Some((t2, v2))) = (get_kf(&arr[i]), get_kf(&arr[i + 1])) {
            if clip_time >= t1 && clip_time <= t2 {
                let raw = (clip_time - t1) / (t2 - t1).max(MIN_KEYFRAME_SPAN);
                let progress = apply_keyframe_easing(raw, arr[i].get("easing"));
                return Some(lerp_t(v1, v2, progress));
            }
        }
    }
    None
}

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
    // Literal bool (packed as 0/1 by bool-typed shader params)
    if let Some(b) = value.as_bool() {
        return if b { 1.0 } else { 0.0 };
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
        let parse = |v: &serde_json::Value| v.as_f64().map(|n| n as f32);
        if let Some(v) = evaluate_keyframes(arr, clip_time, parse, lerp) {
            return v;
        }
    }
    // Unrecognized shape (bare string, object without `expression`, malformed
    // keyframes): warn once instead of silently becoming the default.
    let key = value.to_string();
    warn_expression_once(&key, || {
        format!("Could not evaluate {} as a number; using default {}", key, default)
    });
    default
}

/// Splits a string on commas that are not nested inside parentheses or
/// brackets, so a vec2 expression like `clamp(x, 0, 1), y` separates into its
/// two components rather than fragmenting the inner function arguments.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
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
        // A keyframe value may be a 2-element array or a scalar broadcast.
        let parse = |v_val: &serde_json::Value| -> Option<[f32; 2]> {
            if let Some(v_arr) = v_val.as_array() {
                if v_arr.len() != 2 {
                    return None;
                }
                Some([v_arr[0].as_f64()? as f32, v_arr[1].as_f64()? as f32])
            } else {
                v_val.as_f64().map(|n| [n as f32; 2])
            }
        };
        let lerp2 = |a: [f32; 2], b: [f32; 2], t: f32| std::array::from_fn(|i| lerp(a[i], b[i], t));
        if let Some(v) = evaluate_keyframes(arr, clip_time, parse, lerp2) {
            return v;
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
                    let parts = split_top_level_commas(inner);
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
        // A keyframe value may be a 4-element array or a scalar broadcast.
        let parse = |v_val: &serde_json::Value| -> Option<[f32; 4]> {
            if let Some(v_arr) = v_val.as_array() {
                if v_arr.len() != 4 {
                    return None;
                }
                Some([
                    v_arr[0].as_f64()? as f32,
                    v_arr[1].as_f64()? as f32,
                    v_arr[2].as_f64()? as f32,
                    v_arr[3].as_f64()? as f32,
                ])
            } else {
                v_val.as_f64().map(|n| [n as f32; 4])
            }
        };
        let lerp4 = |a: [f32; 4], b: [f32; 4], t: f32| std::array::from_fn(|i| lerp(a[i], b[i], t));
        if let Some(v) = evaluate_keyframes(arr, clip_time, parse, lerp4) {
            return v;
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
use std::cell::Cell;

static BASE_CONTEXT: OnceLock<HashMapContext> = OnceLock::new();
static EXPR_CACHE: OnceLock<Mutex<HashMap<String, evalexpr::Node>>> = OnceLock::new();

thread_local! {
    /// Expressions already warned about on this render thread, so per-frame
    /// evaluation reports each broken expression once instead of flooding the
    /// log. Thread-local (renders are single-threaded) and cleared by
    /// [`reset_expression_warnings`] at the start of each render, so one
    /// serve-mode job's warnings are never suppressed by an earlier job's.
    static WARNED_EXPRS: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Clears this thread's warned-expressions set. Called at the start of each
/// render so every job reports its own broken expressions.
pub fn reset_expression_warnings() {
    WARNED_EXPRS.with(|w| w.borrow_mut().clear());
}

/// Logs at warn level the first time `expr` fails on this render; later
/// failures of the same expression are silent (they would repeat every
/// frame). The message is built lazily so the steady-state per-frame cost of
/// an already-warned expression is a single set lookup.
fn warn_expression_once(expr: &str, message: impl FnOnce() -> String) {
    WARNED_EXPRS.with(|w| {
        let mut warned = w.borrow_mut();
        if !warned.contains(expr) {
            warned.insert(expr.to_string());
            log::warn!("{}", message());
        }
    });
}

thread_local! {
    /// The absolute timeline position (seconds) of the frame currently being
    /// rendered. SPEC §4.3 distinguishes `time` (absolute) from `clip_time`
    /// (relative to the clip start). Absolute time is constant across every
    /// expression evaluated within a single frame, so rather than threading it
    /// through every `evaluate_*` signature we publish it once per frame here
    /// and `build_eval_context` reads it for the `time` variable.
    static CURRENT_ABSOLUTE_TIME: Cell<Option<f32>> = const { Cell::new(None) };
}

/// Publishes the absolute timeline time for the frame about to be rendered.
/// Must be called once at the start of each frame (before any expression is
/// evaluated) so the `time` variable resolves to the global timeline position.
pub fn set_current_absolute_time(time: f32) {
    CURRENT_ABSOLUTE_TIME.with(|c| c.set(Some(time)));
}

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

        // `min`/`max`/`clamp` are documented in SPEC §4.2. evalexpr ships
        // built-in min/max, but we register float-coercing versions here so
        // mixed int/float arguments behave consistently, and `clamp` (which
        // has no built-in) is available at all.
        let _ = context.set_function("min".into(), evalexpr::Function::new(|argument| {
            let tuple = argument.as_tuple()?;
            if tuple.len() != 2 {
                return Err(evalexpr::EvalexprError::CustomMessage(format!(
                    "min expects exactly 2 arguments, got {}",
                    tuple.len()
                )));
            }
            let a = get_float_helper(&tuple[0])?;
            let b = get_float_helper(&tuple[1])?;
            Ok(evalexpr::Value::Float(a.min(b)))
        }));

        let _ = context.set_function("max".into(), evalexpr::Function::new(|argument| {
            let tuple = argument.as_tuple()?;
            if tuple.len() != 2 {
                return Err(evalexpr::EvalexprError::CustomMessage(format!(
                    "max expects exactly 2 arguments, got {}",
                    tuple.len()
                )));
            }
            let a = get_float_helper(&tuple[0])?;
            let b = get_float_helper(&tuple[1])?;
            Ok(evalexpr::Value::Float(a.max(b)))
        }));

        let _ = context.set_function("clamp".into(), evalexpr::Function::new(|argument| {
            let tuple = argument.as_tuple()?;
            if tuple.len() != 3 {
                return Err(evalexpr::EvalexprError::CustomMessage(format!(
                    "clamp expects exactly 3 arguments (x, min, max), got {}",
                    tuple.len()
                )));
            }
            let x = get_float_helper(&tuple[0])?;
            let lo = get_float_helper(&tuple[1])?;
            let hi = get_float_helper(&tuple[2])?;
            // Guard against inverted bounds so the result stays within [lo, hi].
            Ok(evalexpr::Value::Float(x.max(lo).min(hi)))
        }));

        context
    })
}

fn build_eval_context(clip_time: f32, duration: f32, width: u32, height: u32) -> HashMapContext {
    let mut context = get_base_context().clone();
    // `time` is the absolute timeline position; fall back to clip_time when no
    // frame is in progress (e.g. CPU-only spot-check label evaluation).
    let abs_time = CURRENT_ABSOLUTE_TIME.with(|c| c.get()).unwrap_or(clip_time);
    let _ = context.set_value("time".into(), (abs_time as f64).into());
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
                warn_expression_once(&cleaned_expr, || format!(
                    "Expression '{}' failed to parse: {}. Falling back to plain-number parsing / the default value",
                    expr, e
                ));
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
            Ok(other) => warn_expression_once(&cleaned_expr, || format!(
                "Expression '{}' returned non-numeric value {:?}; using the default value",
                expr, other
            )),
            Err(e) => warn_expression_once(&cleaned_expr, || format!(
                "Expression '{}' failed to evaluate: {}. Using the default value",
                expr, e
            )),
        }
    }

    // Fallback: try parsing the raw string as a number
    expr.parse::<f32>().unwrap_or(default)
}

#[cfg(test)]
mod expr_keyframe_tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool { (a - b).abs() < 1e-3 }

    #[test]
    fn clamp_min_max_are_available() {
        // clamp(x, lo, hi)
        assert!(approx(evaluate_simple_expression("clamp(5.0, 0.0, 1.0)", 0.0, 1.0, 100, 100, -999.0), 1.0));
        assert!(approx(evaluate_simple_expression("clamp(-5.0, 0.0, 1.0)", 0.0, 1.0, 100, 100, -999.0), 0.0));
        assert!(approx(evaluate_simple_expression("clamp(0.3, 0.0, 1.0)", 0.0, 1.0, 100, 100, -999.0), 0.3));
        // min / max with mixed int/float
        assert!(approx(evaluate_simple_expression("min(3, 2.0)", 0.0, 1.0, 100, 100, -999.0), 2.0));
        assert!(approx(evaluate_simple_expression("max(3, 2.0)", 0.0, 1.0, 100, 100, -999.0), 3.0));
    }

    #[test]
    fn time_variable_uses_absolute_timeline() {
        // Absolute time = 4.0, clip-relative time passed in = 1.0
        set_current_absolute_time(4.0);
        let v = serde_json::json!({ "expression": "time" });
        assert!(approx(evaluate_float(&v, 1.0, 5.0, 100, 100, 0.0), 4.0), "time should be absolute");
        let v2 = serde_json::json!({ "expression": "clip_time" });
        assert!(approx(evaluate_float(&v2, 1.0, 5.0, 100, 100, 0.0), 1.0), "clip_time should be relative");
    }

    #[test]
    fn keyframe_easing_is_applied() {
        // Two keyframes 0->1 over [0,1] with ease_in (t*t). At t=0.5 -> 0.25, not 0.5.
        let kf = serde_json::json!([
            { "time": 0.0, "value": 0.0, "easing": "ease_in" },
            { "time": 1.0, "value": 1.0 }
        ]);
        let v = evaluate_float(&kf, 0.5, 1.0, 100, 100, 0.0);
        assert!(approx(v, 0.25), "ease_in at midpoint should be 0.25, got {}", v);

        // Linear (no easing) -> 0.5
        let kf_lin = serde_json::json!([
            { "time": 0.0, "value": 0.0 },
            { "time": 1.0, "value": 1.0 }
        ]);
        assert!(approx(evaluate_float(&kf_lin, 0.5, 1.0, 100, 100, 0.0), 0.5));
    }

    #[test]
    fn keyframe_cubic_bezier_easing() {
        // Linear bezier control points reproduce the identity curve.
        let kf = serde_json::json!([
            { "time": 0.0, "value": 0.0, "easing": [0.0, 0.0, 1.0, 1.0] },
            { "time": 1.0, "value": 10.0 }
        ]);
        let v = evaluate_float(&kf, 0.5, 1.0, 100, 100, 0.0);
        assert!(approx(v, 5.0), "linear bezier at midpoint should be 5.0, got {}", v);
    }

    #[test]
    fn vec2_expression_handles_inner_commas() {
        // Each component is a function call whose own arguments contain commas;
        // the top-level split must only break between the two components.
        let v = serde_json::json!({ "expression": "[min(2, 3), max(1, 4)]" });
        let out = evaluate_vec2(&v, 0.0, 1.0, 100, 100, [-1.0, -1.0]);
        assert!(approx(out[0], 2.0) && approx(out[1], 4.0), "got {:?}", out);
    }

    #[test]
    fn split_top_level_commas_ignores_nested() {
        assert_eq!(split_top_level_commas("a, b"), vec!["a", " b"]);
        assert_eq!(split_top_level_commas("clamp(x, 0, 1), y"), vec!["clamp(x, 0, 1)", " y"]);
        assert_eq!(split_top_level_commas("f(g(1,2), 3), h"), vec!["f(g(1,2), 3)", " h"]);
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn spec_from(json: serde_json::Value) -> RenderSpec {
        serde_json::from_value(json).expect("test spec must deserialize")
    }

    fn base_spec(tracks: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "version": "1.0",
            "output": "out.png",
            "composition": { "width": 16, "height": 16, "fps": 30, "duration": 1.0 },
            "assets": {
                "img": { "type": "image", "path": "input.jpg" },
                "song": { "type": "audio", "path": "song.mp3" },
                "ttf": { "type": "font", "provider": "file", "path": "font.ttf" }
            },
            "tracks": tracks
        })
    }

    #[test]
    fn valid_spec_passes() {
        let spec = spec_from(base_spec(serde_json::json!([{
            "id": "main",
            "clips": [
                { "id": "a", "type": "media", "asset": "img", "duration": 1.0 },
                { "id": "b", "type": "media", "asset": "img", "duration": 1.0 }
            ],
            "transitions": [
                { "id": "t1", "type": "fade", "duration": 0.5, "from": "a", "to": "b" }
            ]
        }])));
        assert!(spec.validate_references().is_ok());
    }

    #[test]
    fn unknown_clip_asset_fails() {
        let spec = spec_from(base_spec(serde_json::json!([{
            "id": "main",
            "clips": [{ "id": "a", "type": "media", "asset": "nope", "duration": 1.0 }]
        }])));
        let err = spec.validate_references().unwrap_err();
        assert!(err.contains("clip 'a'") && err.contains("unknown asset 'nope'"), "{err}");
    }

    #[test]
    fn transition_to_unknown_clip_fails() {
        let spec = spec_from(base_spec(serde_json::json!([{
            "id": "main",
            "clips": [{ "id": "a", "type": "media", "asset": "img", "duration": 1.0 }],
            "transitions": [
                { "id": "t1", "type": "fade", "duration": 0.5, "from": "a", "to": "ghost" }
            ]
        }])));
        let err = spec.validate_references().unwrap_err();
        assert!(err.contains("transition 't1'") && err.contains("unknown clip 'ghost'"), "{err}");
    }

    #[test]
    fn unknown_preset_fails() {
        let spec = spec_from(base_spec(serde_json::json!([{
            "id": "main",
            "clips": [{
                "id": "a", "type": "media", "asset": "img", "duration": 1.0,
                "effects": [{ "type": "preset", "preset": "ghost-preset" }]
            }]
        }])));
        let err = spec.validate_references().unwrap_err();
        assert!(err.contains("unknown preset 'ghost-preset'"), "{err}");
    }

    #[test]
    fn unknown_audio_asset_fails() {
        let mut json = base_spec(serde_json::json!([{
            "id": "main",
            "clips": [{ "id": "a", "type": "media", "asset": "img", "duration": 1.0 }]
        }]));
        json["audio_tracks"] = serde_json::json!([{
            "id": "music",
            "clips": [{ "id": "m1", "asset": "ghost-song", "duration": 1.0 }]
        }]);
        let err = spec_from(json).validate_references().unwrap_err();
        assert!(err.contains("audio track 'music'") && err.contains("unknown asset 'ghost-song'"), "{err}");
    }

    #[test]
    fn audio_clip_on_non_audio_asset_fails() {
        let mut json = base_spec(serde_json::json!([{
            "id": "main",
            "clips": [{ "id": "a", "type": "media", "asset": "img", "duration": 1.0 }]
        }]));
        json["audio_tracks"] = serde_json::json!([{
            "id": "music",
            "clips": [{ "id": "m1", "asset": "ttf", "duration": 1.0 }]
        }]);
        let err = spec_from(json).validate_references().unwrap_err();
        assert!(err.contains("not an audio or video asset"), "{err}");
    }

    #[test]
    fn unknown_font_fails() {
        let spec = spec_from(base_spec(serde_json::json!([{
            "id": "main",
            "clips": [{
                "id": "a", "type": "text", "duration": 1.0,
                "text_params": { "text": "hi", "font": "ghost-font" }
            }]
        }])));
        let err = spec.validate_references().unwrap_err();
        assert!(err.contains("unknown font asset 'ghost-font'"), "{err}");
    }

    #[test]
    fn multiple_errors_reported_together() {
        let spec = spec_from(base_spec(serde_json::json!([{
            "id": "main",
            "clips": [
                { "id": "a", "type": "media", "asset": "nope", "duration": 1.0 },
                {
                    "id": "b", "type": "media", "asset": "img", "duration": 1.0,
                    "effects": [{ "type": "preset", "preset": "ghost" }]
                }
            ],
            "transitions": [
                { "id": "t1", "type": "fade", "duration": 0.5, "from": "a", "to": "ghost" }
            ]
        }])));
        let err = spec.validate_references().unwrap_err();
        assert!(err.contains("3 broken references"), "{err}");
    }

    #[test]
    fn cyclic_presets_are_reported() {
        let mut json = base_spec(serde_json::json!([{
            "id": "main",
            "clips": [{
                "id": "a", "type": "media", "asset": "img", "duration": 1.0,
                "effects": [{ "type": "preset", "preset": "p1" }]
            }]
        }]));
        json["presets"] = serde_json::json!([
            { "name": "p1", "inputs": [], "filters": [{ "type": "preset", "preset": "p2" }] },
            { "name": "p2", "inputs": [], "filters": [{ "type": "preset", "preset": "p1" }] }
        ]);
        let err = spec_from(json).validate_references().unwrap_err();
        assert!(err.contains("preset cycle detected"), "{err}");
    }

    #[test]
    fn deep_preset_nesting_is_reported() {
        // p0 → p1 → … → p6: deeper than expand_effects' depth-5 budget.
        let mut json = base_spec(serde_json::json!([{
            "id": "main",
            "clips": [{
                "id": "a", "type": "media", "asset": "img", "duration": 1.0,
                "effects": [{ "type": "preset", "preset": "p0" }]
            }]
        }]));
        let presets: Vec<serde_json::Value> = (0..7)
            .map(|i| {
                let filters = if i < 6 {
                    serde_json::json!([{ "type": "preset", "preset": format!("p{}", i + 1) }])
                } else {
                    serde_json::json!([{ "type": "blur" }])
                };
                serde_json::json!({ "name": format!("p{i}"), "inputs": [], "filters": filters })
            })
            .collect();
        json["presets"] = serde_json::Value::Array(presets);
        let err = spec_from(json).validate_references().unwrap_err();
        assert!(err.contains("preset nesting deeper than 5"), "{err}");
    }
}
