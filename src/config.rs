use serde::Deserialize;
use std::collections::HashMap;
use evalexpr::{eval_with_context, ContextWithMutableVariables, HashMapContext};

/// Root specification for a render job.
#[derive(Deserialize, Debug, Clone)]
pub struct RenderSpec {
    pub version: String,
    pub output: String,
    pub composition: Composition,
    pub assets: HashMap<String, Asset>,
    pub tracks: Vec<Track>,
    pub audio_tracks: Option<Vec<AudioTrack>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Composition {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration: f32,
}

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

#[derive(Deserialize, Debug, Clone)]
pub struct Track {
    pub id: String,
    #[serde(default)]
    pub start: f32,
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AudioTrack {
    pub id: String,
    #[serde(default)]
    pub start: f32,
    pub clips: Vec<AudioClip>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AudioClip {
    pub id: String,
    pub asset: String,
    pub duration: f32,
    #[serde(default)]
    pub offset: f32,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClipType {
    Media,
    Solid,
    Text,
    Effect,
}

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
    pub blend_mode: Option<BlendMode>,
    #[serde(default)]
    pub params: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SolidParams {
    pub color: [f32; 4],
}

#[derive(Deserialize, Debug, Clone)]
pub struct TextParams {
    pub text: String,
    pub font: String,
    pub font_size: serde_json::Value,
    pub color: [f32; 4],
    #[serde(default)]
    pub axes: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Transform {
    pub position: Option<serde_json::Value>,
    pub scale: Option<serde_json::Value>,
    pub rotation: Option<serde_json::Value>,
    pub opacity: Option<serde_json::Value>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Effect {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub shader: Option<String>,
    #[serde(default)]
    pub params: Option<HashMap<String, serde_json::Value>>,
}

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

/// GPU layout for custom built-in effect shader uniforms.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShaderParams {
    pub grayscale: u32,
    pub brightness: f32,
    pub _padding: [u32; 2],
}

/// GPU layout for compositor shader uniforms.
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

impl Track {
    pub fn get_clip_start_times(&self) -> Vec<f32> {
        let mut start_times = Vec::with_capacity(self.clips.len());
        let mut current_time = self.start;
        for clip in &self.clips {
            current_time += clip.offset.max(0.0);
            start_times.push(current_time);
            current_time += clip.duration;
        }
        start_times
    }
}

impl AudioTrack {
    pub fn get_clip_start_times(&self) -> Vec<f32> {
        let mut start_times = Vec::with_capacity(self.clips.len());
        let mut current_time = self.start;
        for clip in &self.clips {
            current_time += clip.offset.max(0.0);
            start_times.push(current_time);
            current_time += clip.duration;
        }
        start_times
    }
}

impl RenderSpec {
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
        let mut grayscale = 0u32;
        let mut brightness = 1.0f32;
        for effect in &self.effects {
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
                        brightness = brightness * (1.0 + 0.8 * (clip_time * 2.0 * std::f32::consts::PI).sin());
                    }
                }
                _ => {}
            }
        }
        (grayscale, brightness)
    }
}

pub fn evaluate_float(value: &serde_json::Value, clip_time: f32, width: u32, height: u32, default: f32) -> f32 {
    if value.is_null() {
        return default;
    }
    if let Some(num) = value.as_f64() {
        return num as f32;
    }
    if let Some(obj) = value.as_object() {
        if let Some(expr_val) = obj.get("expression") {
            if let Some(expr_str) = expr_val.as_str() {
                return evaluate_simple_expression(expr_str, clip_time, width, height, default);
            }
        }
    }
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
                    return kf1.1 + progress * (kf2.1 - kf1.1);
                }
            }
        }
    }
    default
}

pub fn evaluate_vec2(value: &serde_json::Value, clip_time: f32, width: u32, height: u32, default: [f32; 2]) -> [f32; 2] {
    if value.is_null() {
        return default;
    }
    if let Some(arr) = value.as_array() {
        if arr.len() == 2 {
            if let (Some(x), Some(y)) = (arr[0].as_f64(), arr[1].as_f64()) {
                return [x as f32, y as f32];
            }
        }
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
                        kfs.push((t, [v_num as f32, v_num as f32]));
                    }
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
                    let rx = kf1.1[0] + progress * (kf2.1[0] - kf1.1[0]);
                    let ry = kf1.1[1] + progress * (kf2.1[1] - kf1.1[1]);
                    return [rx, ry];
                }
            }
        }
    }
    if let Some(num) = value.as_f64() {
        return [num as f32, num as f32];
    }
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

pub fn evaluate_simple_expression(expr: &str, clip_time: f32, width: u32, height: u32, default: f32) -> f32 {
    let cleaned_expr = expr.replace(".", "_");
    let mut context = HashMapContext::new();
    let _ = context.set_value("time".into(), (clip_time as f64).into());
    let _ = context.set_value("clip_time".into(), (clip_time as f64).into());
    let _ = context.set_value("comp_width".into(), (width as i64).into());
    let _ = context.set_value("comp_height".into(), (height as i64).into());
    let _ = context.set_value("pi".into(), (std::f64::consts::PI).into());

    if let Ok(evalexpr::Value::Float(result)) = eval_with_context(&cleaned_expr, &context) {
        return result as f32;
    }
    if let Ok(evalexpr::Value::Int(result)) = eval_with_context(&cleaned_expr, &context) {
        return result as f32;
    }
    
    // Fallback for simple parse
    expr.parse::<f32>().unwrap_or(default)
}
