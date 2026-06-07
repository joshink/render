use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RenderSpec {
    pub width: u32,
    pub height: u32,
    pub input: String,
    pub output: String,
    pub effects: Vec<Effect>,
    pub fps: Option<u32>,
    pub duration: Option<f32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Effect {
    pub effect_type: String,
    pub params: Option<HashMap<String, serde_json::Value>>,
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
    pub fn to_shader_params(&self, time: f32) -> ShaderParams {
        let mut grayscale = 0u32;
        let mut brightness = 1.0f32;

        for effect in &self.effects {
            match effect.effect_type.as_str() {
                "grayscale" => {
                    grayscale = 1;
                    if self.duration.is_some() {
                        // Toggle grayscale back and forth every second
                        grayscale = if (time * std::f32::consts::PI).sin() > 0.0 { 1 } else { 0 };
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
                    if self.duration.is_some() {
                        // Oscillate brightness over time (between factor * 0.2 and factor * 1.8)
                        brightness = brightness * (1.0 + 0.8 * (time * 2.0 * std::f32::consts::PI).sin());
                    }
                }
                _ => {}
            }
        }

        ShaderParams {
            grayscale,
            brightness,
            _padding: [0, 0],
        }
    }
}
