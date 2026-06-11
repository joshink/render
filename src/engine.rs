//! GPU render pipeline for the composition engine.
//!
//! This module owns the core render loop: it manages GPU textures, dispatches
//! compute shaders (compositor, built-in effects, and custom user shaders),
//! and reads the final pixel buffer back to the CPU for encoding.

use crate::config::{RenderSpec, ClipType, Clip, Effect, ShaderRegistry, Transition};
use log::error;

/// GPU-side uniform block for custom effect shaders.
///
/// Provides per-dispatch timing and composition metadata so that custom WGSL
/// shaders can animate based on global time, clip-local time, or normalised
/// progress. Padded to 16-byte alignment per the WGSL std140 layout rules.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EngineParams {
    pub time: f32,
    pub clip_time: f32,
    pub progress: f32,
    pub width: u32,
    pub height: u32,
    pub _padding: [u32; 3],
}

/// GPU-side uniform block for custom transition shaders.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TransitionEngineParams {
    pub progress: f32,
    pub duration: f32,
    pub width: u32,
    pub height: u32,
}

/// Serialises a JSON parameter map into a tightly-packed byte buffer for GPU
/// upload as a custom uniform block.
///
/// **Limitations:**
/// - Parameters are sorted **alphabetically by key name**, so the shader
///   author must declare their uniform fields in the same order.
/// - Type detection is best-effort: JSON `f64` → `f32`, `bool` → `u32
///   (0 or 1), `i64` → `i32`. Other JSON types (strings, arrays, objects)
///   are silently skipped.
/// - The returned buffer is zero-padded to **16-byte alignment** to satisfy
///   WGSL uniform block requirements.
/// - If the map is empty (or all values are skipped), a 16-byte zeroed
///   buffer is returned so the GPU never receives a zero-length binding.
pub fn pack_custom_params(
    params: &std::collections::HashMap<String, serde_json::Value>,
    clip_time: f32,
    duration: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort(); // Naga reflection should ideally be used here per spec
    
    let mut buffer = Vec::new();
    for key in keys {
        let val = &params[key];
        
        // 1. Detect vec2
        let is_vec2 = if let Some(arr) = val.as_array() {
            if arr.len() == 2 && arr[0].as_f64().is_some() && arr[1].as_f64().is_some() {
                true
            } else if !arr.is_empty() && arr[0].is_object() && arr[0].get("time").is_some() {
                // Keyframe array. Check if value is an array of length 2
                arr[0].get("value").and_then(|v| v.as_array()).map(|v_arr| v_arr.len() == 2).unwrap_or(false)
            } else {
                false
            }
        } else if let Some(obj) = val.as_object() {
            if let Some(expr_val) = obj.get("expression") {
                if let Some(expr_str) = expr_val.as_str() {
                    expr_str.starts_with('[') && expr_str.ends_with(']')
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        // 2. Detect bool
        let is_bool = if val.is_boolean() {
            true
        } else if let Some(arr) = val.as_array() {
            if !arr.is_empty() && arr[0].is_object() && arr[0].get("time").is_some() {
                arr[0].get("value").map(|v| v.is_boolean()).unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        };

        // 3. Detect integer
        let is_int = if val.is_i64() {
            true
        } else if let Some(arr) = val.as_array() {
            if !arr.is_empty() && arr[0].is_object() && arr[0].get("time").is_some() {
                arr[0].get("value").map(|v| v.is_i64()).unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        };

        // 4. Evaluate and pack based on type
        if is_vec2 {
            // WGSL std140 requires vec2<f32> to start on an 8-byte boundary.
            // Insert padding so the shader's struct field lands where it
            // expects, rather than 4 bytes early after an odd scalar run.
            if buffer.len() % 8 != 0 {
                buffer.resize(buffer.len() + 4, 0);
            }
            let v = crate::config::evaluate_vec2(val, clip_time, duration, width, height, [0.0, 0.0]);
            buffer.extend_from_slice(bytemuck::bytes_of(&v[0]));
            buffer.extend_from_slice(bytemuck::bytes_of(&v[1]));
        } else if is_bool {
            let f = crate::config::evaluate_float(val, clip_time, duration, width, height, 0.0);
            let val_u32 = if f > 0.5 { 1u32 } else { 0u32 };
            buffer.extend_from_slice(bytemuck::bytes_of(&val_u32));
        } else if is_int {
            let f = crate::config::evaluate_float(val, clip_time, duration, width, height, 0.0);
            let val_i32 = f.round() as i32;
            buffer.extend_from_slice(bytemuck::bytes_of(&val_i32));
        } else {
            let f = crate::config::evaluate_float(val, clip_time, duration, width, height, 0.0);
            buffer.extend_from_slice(bytemuck::bytes_of(&f));
        }
    }
    align_uniform_buffer(&mut buffer);
    buffer
}

fn align_uniform_buffer(buf: &mut Vec<u8>) {
    let aligned = (buf.len() + 15) & !15;
    buf.resize(aligned.max(16), 0);
}

pub fn pack_effect_params(
    registry: &ShaderRegistry,
    effect: &Effect,
    clip_time: f32,
    duration: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    if let Some(meta) = registry.find_effect(&effect.effect_type) {
        let mut sorted_params = meta.params.clone();
        sorted_params.sort_by(|a, b| a.target_name().cmp(b.target_name()));

        let mut buffer = Vec::new();
        for param in sorted_params {
            if param.param_type == "depth_map" || param.param_type == "lut" {
                // Asset-reference params bind a texture rather than a scalar; we
                // pack only a presence flag so the shader knows whether to use it.
                let mut has_asset = 0i32;
                if let Some(ref map) = effect.params {
                    if let Some(val) = map.get(&param.name) {
                        if let Some(s) = val.as_str() {
                            if !s.is_empty() {
                                has_asset = 1;
                            }
                        }
                    }
                }
                buffer.extend_from_slice(bytemuck::bytes_of(&has_asset));
            } else {
                let default_val = param.default;
                let val_f32 = effect.params.as_ref()
                    .and_then(|map| map.get(&param.name))
                    .map(|val| crate::config::evaluate_float(val, clip_time, duration, width, height, default_val))
                    .unwrap_or(default_val);

                if param.param_type == "bool" {
                    let val_i32 = if val_f32 > 0.5 { 1i32 } else { 0i32 };
                    buffer.extend_from_slice(bytemuck::bytes_of(&val_i32));
                } else {
                    buffer.extend_from_slice(bytemuck::bytes_of(&val_f32));
                }
            }
        }

        align_uniform_buffer(&mut buffer);
        buffer
    } else {
        if let Some(ref p) = effect.params {
            pack_custom_params(p, clip_time, duration, width, height)
        } else {
            vec![0u8; 16]
        }
    }
}

pub fn pack_transition_params(
    registry: &ShaderRegistry,
    tr: &Transition,
    progress: f32,
    duration: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let shader_id = tr.shader.as_deref().unwrap_or(&tr.transition_type);
    if let Some(meta) = registry.find_transition(shader_id) {
        let mut sorted_params = meta.params.clone();
        sorted_params.sort_by(|a, b| a.target_name().cmp(b.target_name()));

        let mut buffer = Vec::new();
        for param in sorted_params {
            let default_val = param.default;
            let val_f32 = tr.params.as_ref()
                .and_then(|map| map.get(&param.name))
                .map(|val| crate::config::evaluate_float(val, progress * duration, duration, width, height, default_val))
                .unwrap_or(default_val);

            if param.param_type == "bool" {
                let val_i32 = if val_f32 > 0.5 { 1i32 } else { 0i32 };
                buffer.extend_from_slice(bytemuck::bytes_of(&val_i32));
            } else {
                buffer.extend_from_slice(bytemuck::bytes_of(&val_f32));
            }
        }

        align_uniform_buffer(&mut buffer);
        buffer
    } else {
        if let Some(ref p) = tr.params {
            pack_custom_params(p, progress * duration, duration, width, height)
        } else {
            vec![0u8; 16]
        }
    }
}

/// Creates a texture view with the default descriptor.
///
/// A small helper that reduces visual noise at bind group construction sites
/// where every texture needs a view with no special configuration.
fn create_default_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Timeline data derived once from the immutable spec and reused for every
/// frame. `get_clip_start_times` and `resolve_transitions` are pure functions
/// of the spec — and the latter clones every [`Transition`] on each call — so
/// deriving them per-frame is wasted work that scales with the frame count.
pub struct Timeline {
    tracks: Vec<TrackTimeline>,
}

struct TrackTimeline {
    /// Absolute start time of each clip, parallel to the track's `clips`.
    start_times: Vec<f32>,
    /// Resolved `(transition, start_time)` pairs for the track.
    resolved_transitions: Vec<(Transition, f32)>,
}

impl Timeline {
    /// Builds the per-track timeline once from the spec.
    pub fn build(spec: &RenderSpec) -> Self {
        let tracks = spec
            .tracks
            .iter()
            .map(|track| TrackTimeline {
                start_times: track.get_clip_start_times(),
                resolved_transitions: track.resolve_transitions(),
            })
            .collect();
        Timeline { tracks }
    }
}

/// Holds all persistent GPU resources for the render pipeline.
///
/// A single `RenderContext` is created at startup and reused across every
/// frame in the composition.
///
/// # Ping-pong textures
///
/// `texture_a` and `texture_b` form a **ping-pong pair**: each compute
/// dispatch reads from one ("current input") and writes to the other
/// ("current output"), then the references are swapped so the previous
/// output becomes the next input. This avoids read-after-write hazards
/// without ever allocating intermediate textures.
///
/// `texture_c` and `texture_d` are a separate scratch pair used only while a
/// transition is active: the transition's `from` and `to` clips are each
/// rendered off-screen into them before the transition shader blends the two
/// results (see `render_frame_with_timeline`).
///
/// # Feedback textures
///
/// `feedback_texture_a` and `feedback_texture_b` are a second ping-pong
/// pair reserved for **temporal effects** (e.g. optical flow displacement).
/// They carry state across frames so that each frame can read the previous
/// frame's feedback output while writing a new one.
pub struct RenderContext {
    /// Shared with the per-worker GPU cache (see `pipeline::acquire_gpu`),
    /// so dropping a `RenderContext` does not tear the device down between
    /// jobs in serve mode.
    pub device: std::sync::Arc<wgpu::Device>,
    pub queue: std::sync::Arc<wgpu::Queue>,
    /// Effect/transition metadata for the shaders compiled for this render.
    pub registry: ShaderRegistry,
    /// Pre-uploaded media/image textures, keyed by asset ID.
    pub gpu_textures: std::collections::HashMap<String, wgpu::Texture>,
    /// Pre-loaded font assets, keyed by asset ID.
    pub font_assets: std::collections::HashMap<String, Vec<u8>>,
    /// A 1×1 transparent texture used as a fallback when a clip has no asset.
    pub transparent_texture: wgpu::Texture,
    /// Dynamic scratch texture used to upload frame-by-frame text layouts.
    pub text_scratch_texture: wgpu::Texture,
    /// Ping-pong texture A (see struct-level docs).
    pub texture_a: wgpu::Texture,
    /// Ping-pong texture B (see struct-level docs).
    pub texture_b: wgpu::Texture,
    /// Transition scratch texture (see struct-level docs) — not part of the
    /// main ping-pong pair.
    pub texture_c: wgpu::Texture,
    /// Transition scratch texture (see struct-level docs) — not part of the
    /// main ping-pong pair.
    pub texture_d: wgpu::Texture,
    /// Feedback texture A — temporal state for effects like flow.
    pub feedback_texture_a: wgpu::Texture,
    /// Feedback texture B — temporal state for effects like flow.
    pub feedback_texture_b: wgpu::Texture,
    pub compositor_params_buffer: wgpu::Buffer,
    pub engine_params_buffer: wgpu::Buffer,
    pub custom_params_buffer: wgpu::Buffer,
    pub transition_engine_params_buffer: wgpu::Buffer,
    pub transition_custom_params_buffer: wgpu::Buffer,
    pub compositor_pipeline: wgpu::ComputePipeline,
    pub compositor_bind_group_layout: wgpu::BindGroupLayout,
    /// User-registered custom shader pipelines, keyed by shader asset ID or effect type.
    pub custom_shader_pipelines: std::collections::HashMap<String, wgpu::ComputePipeline>,
    pub effect_bind_group_layout: wgpu::BindGroupLayout,
    pub transition_bind_group_layout: wgpu::BindGroupLayout,
    pub readback_buffer: wgpu::Buffer,
    /// Row pitch in bytes, aligned to `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`.
    pub bytes_per_row: u32,
    pub texture_size: wgpu::Extent3d,
    pub workgroups_x: u32,
    pub workgroups_y: u32,
}


impl RenderContext {
    fn render_clip_to_texture<'a>(
        &self,
        clip: &Clip,
        clip_time: f32,
        spec: &RenderSpec,
        time: f32,
        input: &mut &'a wgpu::Texture,
        output: &mut &'a wgpu::Texture,
    ) {
        match clip.clip_type {
            ClipType::Media | ClipType::Solid => {
                self.composite_media_clip(clip, clip_time, spec, input, output);
            }
            ClipType::Text => {
                self.composite_text_clip(clip, clip_time, spec, input, output);
            }
            _ => {}
        }
        let expanded_effects = spec.expand_effects(&clip.effects, clip_time, clip.duration, spec.composition.width, spec.composition.height, 0);
        self.dispatch_expanded_effects(
            &expanded_effects,
            time,
            clip_time,
            clip.duration,
            spec,
            input,
            output,
        );
    }
    /// Renders a single frame, deriving the timeline from `spec` on the fly.
    ///
    /// Prefer [`render_frame_with_timeline`](Self::render_frame_with_timeline)
    /// in a frame loop and build the [`Timeline`] once; this wrapper exists for
    /// one-off callers (e.g. tests, single-image renders).
    pub fn render_frame(&self, time: f32, spec: &RenderSpec) -> Result<Vec<u8>, String> {
        let timeline = Timeline::build(spec);
        self.render_frame_with_timeline(time, spec, &timeline)
    }

    /// Renders a single frame of the composition at the given `time` (seconds),
    /// using a precomputed [`Timeline`] to avoid per-frame re-derivation (and
    /// per-frame `Transition` clones).
    ///
    /// Walks every track bottom-to-top, composites active media/solid/text
    /// clips, dispatches effect and transition shaders, and returns the final
    /// RGBA pixel buffer with premultiplied alpha.
    pub fn render_frame_with_timeline(
        &self,
        time: f32,
        spec: &RenderSpec,
        timeline: &Timeline,
    ) -> Result<Vec<u8>, String> {
        // Publish the absolute timeline time so the `time` expression variable
        // resolves to the global position (distinct from per-clip `clip_time`).
        crate::config::set_current_absolute_time(time);

        let mut current_input = &self.texture_a;
        let mut current_output = &self.texture_b;

        self.clear_canvas(current_input);

        // Process tracks from bottom to top (Z-order)
        for (track_idx, track) in spec.tracks.iter().enumerate() {
            let track_timeline = &timeline.tracks[track_idx];
            let start_times = &track_timeline.start_times;
            let mut active_clip_info = None;
            for (idx, clip) in track.clips.iter().enumerate() {
                let absolute_start = start_times[idx];
                if time >= absolute_start && time <= absolute_start + clip.duration {
                    active_clip_info = Some((clip, time - absolute_start));
                    break;
                }
            }

            let resolved_transitions = &track_timeline.resolved_transitions;
            let active_transition = resolved_transitions.iter().find(|(tr, start)| {
                time >= *start && time <= *start + tr.duration
            });

            if let Some((tr, start)) = active_transition {
                let from_clip = track.clips.iter().find(|c| c.id == tr.from);
                let to_clip = track.clips.iter().find(|c| c.id == tr.to);
                if let (Some(from_clip), Some(to_clip)) = (from_clip, to_clip) {
                    let from_idx = track.clips.iter().position(|c| c.id == tr.from).unwrap();
                    let to_idx = track.clips.iter().position(|c| c.id == tr.to).unwrap();
                    let start_from = start_times[from_idx];
                    let start_to = start_times[to_idx];
                    
                    let clip_time_a = (time - start_from).clamp(0.0, from_clip.duration);
                    let clip_time_b = (time - start_to).clamp(0.0, to_clip.duration);
                    
                    let mut sub_input_a = &self.texture_c;
                    let mut sub_output_a = &self.texture_d;
                    self.copy_texture(current_input, sub_input_a);
                    self.render_clip_to_texture(from_clip, clip_time_a, spec, time, &mut sub_input_a, &mut sub_output_a);
                    self.copy_texture(sub_input_a, current_output);
                    
                    let mut sub_input_b = &self.texture_c;
                    let mut sub_output_b = &self.texture_d;
                    self.copy_texture(current_input, sub_input_b);
                    self.render_clip_to_texture(to_clip, clip_time_b, spec, time, &mut sub_input_b, &mut sub_output_b);
                    
                    let transition_dest = sub_output_b;
                    let progress = ((time - *start).max(0.0) / tr.duration).clamp(0.0, 1.0);
                    
                    self.dispatch_transition(
                        tr,
                        progress,
                        current_output,
                        sub_input_b,
                        transition_dest,
                        spec,
                    );
                    
                    self.copy_texture(transition_dest, current_output);
                    std::mem::swap(&mut current_input, &mut current_output);
                }
            } else if let Some((clip, clip_time)) = active_clip_info {
                match clip.clip_type {
                    ClipType::Media | ClipType::Solid => {
                        self.composite_media_clip(
                            clip, clip_time, spec,
                            &mut current_input, &mut current_output,
                        );
                    }
                    ClipType::Text => {
                        self.composite_text_clip(
                            clip, clip_time, spec,
                            &mut current_input, &mut current_output,
                        );
                    }
                    ClipType::Effect => {
                        let expanded_effects = if let Some(ref shader_id) = clip.shader {
                            vec![Effect {
                                effect_type: "custom_shader".to_string(),
                                shader: Some(shader_id.clone()),
                                preset: None,
                                params: clip.params.clone(),
                            }]
                        } else if clip.preset.is_some() {
                            let temp_effect = Effect {
                                effect_type: "preset".to_string(),
                                shader: None,
                                preset: clip.preset.clone(),
                                params: clip.params.clone(),
                            };
                            spec.expand_effects(&[temp_effect], clip_time, clip.duration, spec.composition.width, spec.composition.height, 0)
                        } else {
                            spec.expand_effects(&clip.effects, clip_time, clip.duration, spec.composition.width, spec.composition.height, 0)
                        };

                        self.dispatch_expanded_effects(
                            &expanded_effects,
                            time,
                            clip_time,
                            clip.duration,
                            spec,
                            &mut current_input,
                            &mut current_output,
                        );
                    }
                }
            }
        }

        self.readback_pixels(current_input, spec)
    }

    /// Clears the given texture to transparent black (`rgba(0,0,0,0)`) using
    /// a GPU render pass.
    ///
    /// This is always the first step of each frame so that the composition
    /// starts from a clean slate.
    fn clear_canvas(&self, texture: &wgpu::Texture) {
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Clear Base Texture"),
        });
        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Clear Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &create_default_view(texture),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit(Some(encoder.finish()));
    }

    /// Composites a media or solid-colour clip onto the current canvas.
    ///
    /// Evaluates the clip's animated transform (position, scale, rotation,
    /// opacity), resolves blend mode and built-in per-clip effects (grayscale,
    /// brightness), uploads compositor uniforms, creates a bind group for
    /// the compositor compute pipeline, and dispatches. After dispatch the
    /// ping-pong textures are swapped so the composited result becomes the
    /// new input for subsequent passes.
    fn composite_media_clip<'a>(
        &self,
        clip: &Clip,
        clip_time: f32,
        spec: &RenderSpec,
        current_input: &mut &'a wgpu::Texture,
        current_output: &mut &'a wgpu::Texture,
    ) {
        let mut comp_params = clip.eval_compositor_params(clip_time, spec);

        let media_texture_ref = clip.asset.as_ref()
            .and_then(|asset_id| self.gpu_textures.get(asset_id))
            .unwrap_or(&self.transparent_texture);
        let media_texture_view = create_default_view(media_texture_ref);

        let mut final_scale = comp_params.scale;
        if clip.clip_type == ClipType::Media {
            let clip_width = media_texture_ref.width() as f32;
            let clip_height = media_texture_ref.height() as f32;
            let comp_width = spec.composition.width as f32;
            let comp_height = spec.composition.height as f32;

            let scale_mode = clip.scale_mode.as_deref().unwrap_or("fit");
            let base_scale = match scale_mode {
                "stretch" => [comp_width / clip_width, comp_height / clip_height],
                "fit" => {
                    let s = (comp_width / clip_width).min(comp_height / clip_height);
                    [s, s]
                }
                "fill" => {
                    let s = (comp_width / clip_width).max(comp_height / clip_height);
                    [s, s]
                }
                "natural" => [1.0, 1.0],
                _ => {
                    log::warn!("Unknown scale mode '{}', falling back to fit", scale_mode);
                    let s = (comp_width / clip_width).min(comp_height / clip_height);
                    [s, s]
                }
            };
            final_scale = [comp_params.scale[0] * base_scale[0], comp_params.scale[1] * base_scale[1]];
        } else if clip.clip_type == ClipType::Solid {
            let comp_width = spec.composition.width as f32;
            let comp_height = spec.composition.height as f32;
            final_scale = [comp_params.scale[0] * comp_width, comp_params.scale[1] * comp_height];
        }

        let (clip_type_u32, solid_color) = match clip.clip_type {
            ClipType::Solid => {
                let color = clip.solid_params.as_ref().map(|p| p.color).unwrap_or([0.0, 0.0, 0.0, 1.0]);
                (1u32, color)
            }
            _ => (0u32, [0.0, 0.0, 0.0, 1.0]),
        };

        comp_params.scale = final_scale;
        comp_params.clip_type = clip_type_u32;
        comp_params.solid_color = solid_color;

        self.queue.write_buffer(&self.compositor_params_buffer, 0, bytemuck::bytes_of(&comp_params));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Compositor Bind Group"),
            layout: &self.compositor_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&create_default_view(current_input)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&media_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&create_default_view(current_output)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.compositor_params_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Compositor Dispatch"),
        });
        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Compositor Compute Pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.compositor_pipeline);
            compute_pass.set_bind_group(0, &bind_group, &[]);
            compute_pass.dispatch_workgroups(self.workgroups_x, self.workgroups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
        std::mem::swap(current_input, current_output);
    }

    /// Composites a text overlay layout clip.
    fn composite_text_clip<'a>(
        &self,
        clip: &Clip,
        clip_time: f32,
        spec: &RenderSpec,
        current_input: &mut &'a wgpu::Texture,
        current_output: &mut &'a wgpu::Texture,
    ) {
        if let Some(ref text_params) = clip.text_params {
            let root_node = if let Some(ref kind) = text_params.kind {
                if kind == "layout" {
                    match text_params.body.clone() {
                        Some(body) => body,
                        None => {
                            log::error!("Text clip kind='layout' but body is missing, skipping");
                            return;
                        }
                    }
                } else {
                    Self::simple_to_layout(text_params)
                }
            } else if let Some(ref body) = text_params.body {
                body.clone()
            } else {
                Self::simple_to_layout(text_params)
            };

            let mut resolved = crate::layout::resolve_layout_node(
                &root_node,
                clip_time,
                clip.duration,
                spec.composition.width,
                spec.composition.height,
            );

            resolved.measure(
                spec.composition.width as f32,
                spec.composition.height as f32,
                &self.font_assets,
            );
            resolved.arrange(
                0.0,
                0.0,
                spec.composition.width as f32,
                spec.composition.height as f32,
            );

            let mut dest_img = image::ImageBuffer::from_pixel(
                spec.composition.width,
                spec.composition.height,
                image::Rgba([0, 0, 0, 0]),
            );
            let entrance = text_params.entrance.as_ref();
            let exit = text_params.exit.as_ref();
            resolved.rasterize(
                &mut dest_img,
                &self.font_assets,
                clip_time,
                clip.duration,
                entrance,
                exit,
            );

            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.text_scratch_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &dest_img,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * spec.composition.width),
                    rows_per_image: Some(spec.composition.height),
                },
                self.texture_size,
            );

            let media_texture_view = create_default_view(&self.text_scratch_texture);
            let comp_params = clip.eval_compositor_params(clip_time, spec);

            self.queue.write_buffer(&self.compositor_params_buffer, 0, bytemuck::bytes_of(&comp_params));

            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Compositor Bind Group (Text)"),
                layout: &self.compositor_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&create_default_view(current_input)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&media_texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&create_default_view(current_output)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.compositor_params_buffer.as_entire_binding(),
                    },
                ],
            });

            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Compositor Dispatch (Text)"),
            });
            {
                let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Compositor Compute Pass (Text)"),
                    timestamp_writes: None,
                });
                compute_pass.set_pipeline(&self.compositor_pipeline);
                compute_pass.set_bind_group(0, &bind_group, &[]);
                compute_pass.dispatch_workgroups(self.workgroups_x, self.workgroups_y, 1);
            }
            self.queue.submit(Some(encoder.finish()));
            std::mem::swap(current_input, current_output);
        }
    }

    fn simple_to_layout(params: &crate::config::TextParams) -> crate::config::LayoutNode {
        crate::config::LayoutNode {
            r#type: "text".to_string(),
            spacing: None,
            alignment: None,
            children: None,
            padding: None,
            size: None,
            text: params.text.as_ref().map(|s| serde_json::Value::String(s.clone())),
            font: params.font.clone(),
            font_size: params.font_size.clone(),
            color: params.color.as_ref().map(|c| serde_json::Value::from(c.to_vec())),
            axes: params.axes.clone(),
        }
    }


    /// Dispatches an effect (either built-in or custom) using the unified compute layout.
    fn dispatch_effect<'a>(
        &self,
        effect: &Effect,
        shader_id: &str,
        time: f32,
        clip_time: f32,
        duration: f32,
        progress: f32,
        spec: &RenderSpec,
        current_input: &mut &'a wgpu::Texture,
        current_output: &mut &'a wgpu::Texture,
    ) {
        if let Some(pipeline) = self.custom_shader_pipelines.get(shader_id) {
            let engine_params = EngineParams {
                time,
                clip_time,
                progress,
                width: spec.composition.width,
                height: spec.composition.height,
                _padding: [0, 0, 0],
            };
            self.queue.write_buffer(&self.engine_params_buffer, 0, bytemuck::bytes_of(&engine_params));

            let custom_params_data = pack_effect_params(&self.registry, effect, clip_time, duration, spec.composition.width, spec.composition.height);
            self.queue.write_buffer(&self.custom_params_buffer, 0, &custom_params_data);

            let depth_texture_ref = crate::config::get_depth_map_asset_id_from_effects(&self.registry, std::slice::from_ref(effect))
                .and_then(|asset_id| self.gpu_textures.get(&asset_id))
                .unwrap_or(&self.transparent_texture);
            let depth_view = create_default_view(depth_texture_ref);

            let lut_texture_ref = crate::config::get_lut_asset_id_from_effects(&self.registry, std::slice::from_ref(effect))
                .and_then(|asset_id| self.gpu_textures.get(&asset_id))
                .unwrap_or(&self.transparent_texture);
            let lut_view = create_default_view(lut_texture_ref);

            let frame_idx = (time * spec.composition.fps as f32).round() as u32;
            let (feedback_in, feedback_out) = if frame_idx % 2 == 0 {
                (&self.feedback_texture_a, &self.feedback_texture_b)
            } else {
                (&self.feedback_texture_b, &self.feedback_texture_a)
            };
            let feedback_in_view = create_default_view(feedback_in);
            let feedback_out_view = create_default_view(feedback_out);

            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("Effect Bind Group: {}", shader_id)),
                layout: &self.effect_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&create_default_view(current_input)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&create_default_view(current_output)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.engine_params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.custom_params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&depth_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&feedback_in_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(&feedback_out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(&lut_view),
                    },
                ],
            });

            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(&format!("Effect Dispatch: {}", shader_id)),
            });
            {
                let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(&format!("Effect Compute Pass: {}", shader_id)),
                    timestamp_writes: None,
                });
                compute_pass.set_pipeline(pipeline);
                compute_pass.set_bind_group(0, &bind_group, &[]);
                compute_pass.dispatch_workgroups(self.workgroups_x, self.workgroups_y, 1);
            }
            self.queue.submit(Some(encoder.finish()));
            std::mem::swap(current_input, current_output);
        } else {
            error!("Shader pipeline '{}' not found!", shader_id);
        }
    }

    fn dispatch_expanded_effects<'a>(
        &self,
        effects: &[Effect],
        time: f32,
        clip_time: f32,
        duration: f32,
        spec: &RenderSpec,
        current_input: &mut &'a wgpu::Texture,
        current_output: &mut &'a wgpu::Texture,
    ) {
        let progress = (clip_time / duration).clamp(0.0, 1.0);

        for effect in effects {
            let shader_id = effect.shader.as_ref().unwrap_or(&effect.effect_type);
            self.dispatch_effect(
                effect,
                shader_id,
                time,
                clip_time,
                duration,
                progress,
                spec,
                current_input,
                current_output,
            );
        }
    }

    fn dispatch_transition(
        &self,
        tr: &crate::config::Transition,
        progress: f32,
        tex_from: &wgpu::Texture,
        tex_to: &wgpu::Texture,
        output_tex: &wgpu::Texture,
        spec: &RenderSpec,
    ) {
        let shader_id = tr.shader.as_deref().unwrap_or(&tr.transition_type);
        if let Some(pipeline) = self.custom_shader_pipelines.get(shader_id) {
            let transition_params = TransitionEngineParams {
                progress,
                duration: tr.duration,
                width: spec.composition.width,
                height: spec.composition.height,
            };
            self.queue.write_buffer(&self.transition_engine_params_buffer, 0, bytemuck::bytes_of(&transition_params));

            let custom_params_data = pack_transition_params(&self.registry, tr, progress, tr.duration, spec.composition.width, spec.composition.height);
            self.queue.write_buffer(&self.transition_custom_params_buffer, 0, &custom_params_data);

            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("Transition Bind Group: {}", shader_id)),
                layout: &self.transition_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&create_default_view(tex_from)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&create_default_view(tex_to)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&create_default_view(output_tex)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.transition_engine_params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: self.transition_custom_params_buffer.as_entire_binding(),
                    },
                ],
            });

            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(&format!("Transition Dispatch: {}", shader_id)),
            });
            {
                let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(&format!("Transition Compute Pass: {}", shader_id)),
                    timestamp_writes: None,
                });
                compute_pass.set_pipeline(pipeline);
                compute_pass.set_bind_group(0, &bind_group, &[]);
                compute_pass.dispatch_workgroups(self.workgroups_x, self.workgroups_y, 1);
            }
            self.queue.submit(Some(encoder.finish()));
        } else {
            error!("Transition shader pipeline '{}' not found!", shader_id);
        }
    }

    fn copy_texture(&self, source: &wgpu::Texture, destination: &wgpu::Texture) {
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Copy Texture"),
        });
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: source,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: destination,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            self.texture_size,
        );
        self.queue.submit(Some(encoder.finish()));
    }

    /// Copies the final composited texture back to the CPU and returns the
    /// pixel data as a flat `Vec<u8>` in RGBA order.
    ///
    /// # Row-pitch unpadding
    ///
    /// WebGPU requires `bytes_per_row` to be a multiple of 256. If the
    /// composition width is not a multiple of 64 pixels (64 × 4 bytes = 256),
    /// the mapped buffer contains padding bytes at the end of each row that
    /// must be stripped before the pixel data is usable.
    ///
    /// # Alpha
    ///
    /// The shader pipeline works in **straight (un-associated) alpha** and
    /// this method preserves it: pixels are returned exactly as composited,
    /// with `R`, `G`, `B` independent of `A`. That is what PNG (and the
    /// `image` crate's RGBA encoders) expect — premultiplying here would
    /// darken every semi-transparent pixel in the saved file.
    ///
    /// The video path premultiplies separately (see
    /// [`premultiply_alpha_on_black`] in `main.rs`) because ffmpeg drops the
    /// alpha channel when converting to an opaque pixel format, and
    /// premultiplied RGB is exactly the result of flattening onto black.
    fn readback_pixels(&self, source_texture: &wgpu::Texture, spec: &RenderSpec) -> Result<Vec<u8>, String> {
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Readback Encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: source_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.readback_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(self.bytes_per_row),
                    rows_per_image: Some(spec.composition.height),
                },
            },
            self.texture_size,
        );
        self.queue.submit(Some(encoder.finish()));

        let buffer_slice = self.readback_buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).unwrap();
        });

        self.device.poll(wgpu::Maintain::Wait);

        if let Ok(Ok(())) = receiver.recv() {
            let data = buffer_slice.get_mapped_range();
            let total_pixels = (spec.composition.width * spec.composition.height * 4) as usize;
            let mut unpadded_pixels = vec![0u8; total_pixels];
            let mut dest_idx = 0;
            for row in 0..spec.composition.height {
                let start = (row * self.bytes_per_row) as usize;
                let end = start + (spec.composition.width * 4) as usize;
                let row_data = &data[start..end];

                // Copy straight (un-premultiplied) alpha through unchanged.
                unpadded_pixels[dest_idx..dest_idx + row_data.len()].copy_from_slice(row_data);
                dest_idx += row_data.len();
            }
            drop(data);
            self.readback_buffer.unmap();
            Ok(unpadded_pixels)
        } else {
            // Typically GPU device loss; the caller decides whether that fails
            // one job or the whole process — never exit from library code.
            Err("Failed to map readback buffer back to CPU (GPU device lost?)".to_string())
        }
    }
}

#[cfg(test)]
mod pack_tests {
    use super::*;

    #[test]
    fn vec2_is_padded_to_eight_byte_alignment() {
        // Sorted keys: "a" (scalar, offset 0..4), then "z" (vec2). std140
        // requires the vec2 at offset 8, so bytes 4..8 must be padding.
        let mut params = std::collections::HashMap::new();
        params.insert("a".to_string(), serde_json::json!(1.0));
        params.insert("z".to_string(), serde_json::json!([2.0, 3.0]));

        let buf = pack_custom_params(&params, 0.0, 1.0, 100, 100);

        assert_eq!(&buf[0..4], bytemuck::bytes_of(&1.0f32), "scalar at offset 0");
        assert_eq!(&buf[4..8], &[0u8; 4], "padding before vec2");
        assert_eq!(&buf[8..12], bytemuck::bytes_of(&2.0f32), "vec2.x at offset 8");
        assert_eq!(&buf[12..16], bytemuck::bytes_of(&3.0f32), "vec2.y at offset 12");
    }

    /// `Timeline::build` must preserve per-track order and produce exactly the
    /// same start times and resolved transitions the per-track methods do — the
    /// invariant that lets the frame loop reuse it instead of re-deriving (and
    /// re-cloning every transition) each frame.
    #[test]
    fn timeline_matches_per_track_derivation() {
        let spec_json = serde_json::json!({
            "version": "1.0",
            "output": "out.mp4",
            "assets": {},
            "audio_tracks": null,
            "composition": { "width": 100, "height": 100, "fps": 30, "duration": 10.0 },
            "tracks": [
                {
                    "id": "t0",
                    "clips": [
                        { "id": "a", "type": "solid", "asset": null, "duration": 2.0 },
                        { "id": "b", "type": "solid", "asset": null, "duration": 3.0 }
                    ],
                    "transitions": [
                        { "id": "x", "type": "fade", "duration": 1.0, "from": "a", "to": "b" }
                    ]
                },
                {
                    "id": "t1",
                    "start": 1.5,
                    "clips": [
                        { "id": "c", "type": "solid", "asset": null, "duration": 4.0 }
                    ]
                }
            ]
        });
        let spec: RenderSpec = serde_json::from_value(spec_json).expect("valid spec");

        let timeline = Timeline::build(&spec);
        assert_eq!(timeline.tracks.len(), spec.tracks.len());

        for (track, tl) in spec.tracks.iter().zip(&timeline.tracks) {
            assert_eq!(tl.start_times, track.get_clip_start_times());
            let expected = track.resolve_transitions();
            assert_eq!(tl.resolved_transitions.len(), expected.len());
            for ((tr, start), (etr, estart)) in tl.resolved_transitions.iter().zip(&expected) {
                assert_eq!(tr.id, etr.id);
                assert_eq!(start, estart);
            }
        }

        // Track 0: clips at 0.0 and 2.0; the (start-less) transition centers on
        // the second clip's start, i.e. 2.0 - 0.5 * 1.0 = 1.5.
        assert_eq!(timeline.tracks[0].start_times, vec![0.0, 2.0]);
        assert_eq!(timeline.tracks[0].resolved_transitions[0].1, 1.5);
        // Track 1 honours its 1.5s track-level start offset.
        assert_eq!(timeline.tracks[1].start_times, vec![1.5]);
    }
}
