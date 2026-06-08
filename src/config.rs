use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RenderSpec {
    pub version: String,
    pub output: String,
    pub composition: Composition,
    pub assets: HashMap<String, Asset>,
    pub tracks: Vec<Track>,
    pub audio_tracks: Option<Vec<AudioTrack>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Composition {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
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
    Font {
        provider: String,
        path: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Track {
    pub id: String,
    #[serde(default)]
    pub start: f32,
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioTrack {
    pub id: String,
    #[serde(default)]
    pub start: f32,
    pub clips: Vec<AudioClip>,
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioClip {
    pub id: String,
    pub asset: String,
    pub duration: f32,
    #[serde(default)]
    pub offset: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Clip {
    pub id: String,
    #[serde(rename = "type")]
    pub clip_type: String,
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
    pub blend_mode: Option<String>,
    #[serde(default)]
    pub params: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SolidParams {
    pub color: [f32; 4],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TextParams {
    pub text: String,
    pub font: String,
    pub font_size: serde_json::Value,
    pub color: [f32; 4],
    #[serde(default)]
    pub axes: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Transform {
    pub position: Option<serde_json::Value>,
    pub scale: Option<serde_json::Value>,
    pub rotation: Option<serde_json::Value>,
    pub opacity: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Effect {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub shader: Option<String>,
    #[serde(default)]
    pub params: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
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

// Memory layout aligned to 16-byte boundaries for the WGSL uniform block
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShaderParams {
    pub grayscale: u32,
    pub brightness: f32,
    pub _padding: [u32; 2], // Pad to 16 bytes (128-bit alignment)
}

impl RenderSpec {
    pub fn get_input_path(&self) -> Option<String> {
        // Look for the first media clip that has an associated asset
        for track in &self.tracks {
            for clip in &track.clips {
                if clip.clip_type == "media" {
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

        // Always check composition start and end
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
                                    let abs_time = clip_start + t;
                                    events.push((
                                        abs_time,
                                        format!(
                                            "Track '{}' - Clip '{}' - Brightness at peak (factor oscillation)",
                                            track.id, clip.id
                                        ),
                                    ));
                                    t += 1.0;
                                }
                                let mut t = 0.75;
                                while t < clip.duration {
                                    let abs_time = clip_start + t;
                                    events.push((
                                        abs_time,
                                        format!(
                                            "Track '{}' - Clip '{}' - Brightness at trough (factor oscillation)",
                                            track.id, clip.id
                                        ),
                                    ));
                                    t += 1.0;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            for transition in &track.transitions {
                let trans_start = transition.start;
                let trans_mid = transition.start + transition.duration / 2.0;
                let trans_end = transition.start + transition.duration;

                events.push((
                    trans_start,
                    format!(
                        "Track '{}' - Transition '{}' starts (from '{}' to '{}')",
                        track.id, transition.id, transition.from, transition.to
                    ),
                ));
                events.push((
                    trans_mid,
                    format!("Track '{}' - Transition '{}' midpoint", track.id, transition.id),
                ));
                events.push((
                    trans_end,
                    format!("Track '{}' - Transition '{}' ends", track.id, transition.id),
                ));
            }
        }

        // Keep events within [0, duration]
        events.retain(|(t, _)| *t >= 0.0 && *t <= self.composition.duration);
        events
    }

    pub fn to_shader_params(&self, time: f32) -> ShaderParams {
        let mut grayscale = 0u32;
        let mut brightness = 1.0f32;

        let is_movie = self.output.ends_with(".mp4");

        // Find the active clip and evaluate its effects
        for track in &self.tracks {
            let start_times = track.get_clip_start_times();
            for (idx, clip) in track.clips.iter().enumerate() {
                let absolute_start = start_times[idx];
                if time >= absolute_start && time <= absolute_start + clip.duration {
                    let clip_time = time - absolute_start;
                    for effect in &clip.effects {
                        match effect.effect_type.as_str() {
                            "grayscale" => {
                                grayscale = 1;
                                if is_movie {
                                    // Toggle grayscale back and forth every second (relative to clip start)
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
                                    // Oscillate brightness over time (between factor * 0.2 and factor * 1.8, relative to clip start)
                                    brightness = brightness * (1.0 + 0.8 * (clip_time * 2.0 * std::f32::consts::PI).sin());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        ShaderParams {
            grayscale,
            brightness,
            _padding: [0, 0],
        }
    }
}

// Memory layout aligned to 16-byte boundaries for the WebGPU uniform block in compositor.wgsl
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
    let cleaned = expr
        .replace("clip.time", &clip_time.to_string())
        .replace("time", &clip_time.to_string())
        .replace("comp.width", &width.to_string())
        .replace("comp.height", &height.to_string());
    
    if let Some(pos) = cleaned.find('*') {
        let left = cleaned[..pos].trim().parse::<f32>().unwrap_or(0.0);
        let right = cleaned[pos+1..].trim().parse::<f32>().unwrap_or(0.0);
        return left * right;
    }
    if let Some(pos) = cleaned.find('/') {
        let left = cleaned[..pos].trim().parse::<f32>().unwrap_or(0.0);
        let right = cleaned[pos+1..].trim().parse::<f32>().unwrap_or(1.0);
        return left / right;
    }
    if let Some(pos) = cleaned.find('+') {
        let left = cleaned[..pos].trim().parse::<f32>().unwrap_or(0.0);
        let right = cleaned[pos+1..].trim().parse::<f32>().unwrap_or(0.0);
        return left + right;
    }
    if let Some(pos) = cleaned.find('-') {
        let left = cleaned[..pos].trim().parse::<f32>().unwrap_or(0.0);
        let right = cleaned[pos+1..].trim().parse::<f32>().unwrap_or(0.0);
        return left - right;
    }
    
    cleaned.parse::<f32>().unwrap_or(default)
}
