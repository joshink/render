use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RenderSpec {
    pub width: u32,
    pub height: u32,
    pub input: String,
    pub output: String,
    pub effects: Vec<Effect>,
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
    pub fn to_shader_params(&self) -> ShaderParams {
        let mut grayscale = 0u32;
        let mut brightness = 1.0f32;

        for effect in &self.effects {
            match effect.effect_type.as_str() {
                "grayscale" => {
                    grayscale = 1;
                }
                "brightness" => {
                    if let Some(ref params) = effect.params {
                        if let Some(factor_val) = params.get("factor") {
                            if let Some(factor) = factor_val.as_f64() {
                                brightness = factor as f32;
                            }
                        }
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
