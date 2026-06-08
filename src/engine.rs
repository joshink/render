//! GPU render pipeline for the composition engine.
//!
//! This module owns the core render loop: it manages GPU textures, dispatches
//! compute shaders (compositor, built-in effects, and custom user shaders),
//! and reads the final pixel buffer back to the CPU for encoding.

use crate::config::{RenderSpec, ClipType, CompositorParams, Clip};
use log::{error};

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

/// Serialises a JSON parameter map into a tightly-packed byte buffer for GPU
/// upload as a custom uniform block.
///
/// **Limitations:**
/// - Parameters are sorted **alphabetically by key name**, so the shader
///   author must declare their uniform fields in the same order.
/// - Type detection is best-effort: JSON `f64` → `f32`, `bool` → `u32`
///   (0 or 1), `i64` → `i32`. Other JSON types (strings, arrays, objects)
///   are silently skipped.
/// - The returned buffer is zero-padded to **16-byte alignment** to satisfy
///   WGSL uniform block requirements.
/// - If the map is empty (or all values are skipped), a 16-byte zeroed
///   buffer is returned so the GPU never receives a zero-length binding.
pub fn pack_custom_params(params: &std::collections::HashMap<String, serde_json::Value>) -> Vec<u8> {
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort(); // Naga reflection should ideally be used here per spec
    
    let mut buffer = Vec::new();
    for key in keys {
        let val = &params[key];
        if let Some(f) = val.as_f64() {
            buffer.extend_from_slice(bytemuck::bytes_of(&(f as f32)));
        } else if let Some(b) = val.as_bool() {
            let val_u32 = if b { 1u32 } else { 0u32 };
            buffer.extend_from_slice(bytemuck::bytes_of(&val_u32));
        } else if let Some(i) = val.as_i64() {
            buffer.extend_from_slice(bytemuck::bytes_of(&(i as i32)));
        }
    }
    
    let aligned_len = (buffer.len() + 15) & !15;
    while buffer.len() < aligned_len {
        buffer.push(0);
    }
    if buffer.is_empty() {
        buffer.resize(16, 0);
    }
    buffer
}

/// Creates a texture view with the default descriptor.
///
/// A small helper that reduces visual noise at bind group construction sites
/// where every texture needs a view with no special configuration.
fn create_default_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor::default())
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
/// # Feedback textures
///
/// `feedback_texture_a` and `feedback_texture_b` are a second ping-pong
/// pair reserved for **temporal effects** (e.g. optical flow displacement).
/// They carry state across frames so that each frame can read the previous
/// frame's feedback output while writing a new one.
pub struct RenderContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Pre-uploaded media/image textures, keyed by asset ID.
    pub gpu_textures: std::collections::HashMap<String, wgpu::Texture>,
    /// A 1×1 transparent texture used as a fallback when a clip has no asset.
    pub transparent_texture: wgpu::Texture,
    /// Ping-pong texture A (see struct-level docs).
    pub texture_a: wgpu::Texture,
    /// Ping-pong texture B (see struct-level docs).
    pub texture_b: wgpu::Texture,
    /// Feedback texture A — temporal state for effects like flow.
    pub feedback_texture_a: wgpu::Texture,
    /// Feedback texture B — temporal state for effects like flow.
    pub feedback_texture_b: wgpu::Texture,
    pub compositor_params_buffer: wgpu::Buffer,
    pub engine_params_buffer: wgpu::Buffer,
    pub custom_params_buffer: wgpu::Buffer,
    pub built_in_params_buffer: wgpu::Buffer,
    pub compositor_pipeline: wgpu::ComputePipeline,
    pub compositor_bind_group_layout: wgpu::BindGroupLayout,
    pub effects_wgsl_pipeline: wgpu::ComputePipeline,
    pub effects_wgsl_bind_group_layout: wgpu::BindGroupLayout,
    /// User-registered custom shader pipelines, keyed by shader asset ID.
    pub custom_shader_pipelines: std::collections::HashMap<String, wgpu::ComputePipeline>,
    pub effect_bind_group_layout: wgpu::BindGroupLayout,
    pub readback_buffer: wgpu::Buffer,
    /// Row pitch in bytes, aligned to `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`.
    pub bytes_per_row: u32,
    pub texture_size: wgpu::Extent3d,
    pub workgroups_x: u32,
    pub workgroups_y: u32,
}

impl RenderContext {
    /// Renders a single frame of the composition at the given `time` (seconds).
    ///
    /// Walks every track bottom-to-top, composites active media/solid clips,
    /// dispatches effect shaders, and returns the final RGBA pixel buffer
    /// with premultiplied alpha.
    pub fn render_frame(&self, time: f32, spec: &RenderSpec, is_movie: bool) -> Vec<u8> {
        let mut current_input = &self.texture_a;
        let mut current_output = &self.texture_b;

        self.clear_canvas(current_input);

        // Process tracks from bottom to top (Z-order)
        for track in &spec.tracks {
            let start_times = track.get_clip_start_times();
            let mut active_clip_info = None;
            for (idx, clip) in track.clips.iter().enumerate() {
                let absolute_start = start_times[idx];
                if time >= absolute_start && time <= absolute_start + clip.duration {
                    active_clip_info = Some((clip, time - absolute_start));
                    break;
                }
            }

            if let Some((clip, clip_time)) = active_clip_info {
                match clip.clip_type {
                    ClipType::Media | ClipType::Solid => {
                        self.composite_media_clip(
                            clip, clip_time, spec, is_movie,
                            &mut current_input, &mut current_output,
                        );
                    }
                    ClipType::Effect => {
                        let progress = (clip_time / clip.duration).clamp(0.0, 1.0);
                        if let Some(ref shader_id) = clip.shader {
                            self.dispatch_custom_effect(
                                clip, shader_id, time, clip_time, progress, spec,
                                &mut current_input, &mut current_output,
                            );
                        } else {
                            self.dispatch_builtin_effects(
                                clip, time, clip_time, spec, is_movie,
                                &mut current_input, &mut current_output,
                            );
                        }
                    }
                    _ => {}
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
        is_movie: bool,
        current_input: &mut &'a wgpu::Texture,
        current_output: &mut &'a wgpu::Texture,
    ) {
        let position = clip.eval_position(clip_time, spec.composition.width, spec.composition.height);
        let scale = clip.eval_scale(clip_time, spec.composition.width, spec.composition.height);
        let rotation = clip.eval_rotation(clip_time, spec.composition.width, spec.composition.height);
        let opacity = clip.eval_opacity(clip_time, spec.composition.width, spec.composition.height);
        let blend_mode_u32 = clip.blend_mode.as_ref().map(|b| b.clone().as_u32()).unwrap_or(0);
        let (grayscale, brightness) = clip.eval_built_in_effects(clip_time, is_movie);

        let (clip_type_u32, solid_color) = match clip.clip_type {
            ClipType::Solid => {
                let color = clip.solid_params.as_ref().map(|p| p.color).unwrap_or([0.0, 0.0, 0.0, 1.0]);
                (1u32, color)
            }
            _ => (0u32, [0.0, 0.0, 0.0, 1.0]),
        };

        let comp_params = CompositorParams {
            position,
            scale,
            rotation,
            opacity,
            clip_type: clip_type_u32,
            blend_mode: blend_mode_u32,
            grayscale,
            brightness,
            _padding: [0, 0],
            solid_color,
        };
        self.queue.write_buffer(&self.compositor_params_buffer, 0, bytemuck::bytes_of(&comp_params));

        let media_texture_ref = clip.asset.as_ref()
            .and_then(|asset_id| self.gpu_textures.get(asset_id))
            .unwrap_or(&self.transparent_texture);
        let media_texture_view = create_default_view(media_texture_ref);

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

    /// Dispatches a user-provided custom WGSL shader as a full-screen effect.
    ///
    /// Looks up the pre-compiled compute pipeline by `shader_id`, uploads
    /// `EngineParams` (timing) and any user-defined `params` (via
    /// `pack_custom_params`), then dispatches. Logs an error and no-ops if
    /// the pipeline was not registered at startup.
    fn dispatch_custom_effect<'a>(
        &self,
        clip: &Clip,
        shader_id: &str,
        time: f32,
        clip_time: f32,
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

            let custom_params_data = if let Some(ref p) = clip.params {
                pack_custom_params(p)
            } else {
                vec![0u8; 16]
            };
            self.queue.write_buffer(&self.custom_params_buffer, 0, &custom_params_data);

            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Custom Effect Bind Group"),
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
                ],
            });

            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Custom Effect Dispatch"),
            });
            {
                let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Custom Effect Compute Pass"),
                    timestamp_writes: None,
                });
                compute_pass.set_pipeline(pipeline);
                compute_pass.set_bind_group(0, &bind_group, &[]);
                compute_pass.dispatch_workgroups(self.workgroups_x, self.workgroups_y, 1);
            }
            self.queue.submit(Some(encoder.finish()));
            std::mem::swap(current_input, current_output);
        } else {
            error!("Custom shader pipeline '{}' not found!", shader_id);
        }
    }

    /// Dispatches the built-in `effects.wgsl` shader for standard post-
    /// processing (blur, glow, colour grading, film grain, depth blur, flow,
    /// etc.).
    ///
    /// Evaluates `ShaderParams` from the clip's effect list, resolves the
    /// depth-map texture (falling back to transparent), selects the correct
    /// feedback ping-pong pair based on frame index, and dispatches.
    fn dispatch_builtin_effects<'a>(
        &self,
        clip: &Clip,
        time: f32,
        clip_time: f32,
        spec: &RenderSpec,
        is_movie: bool,
        current_input: &mut &'a wgpu::Texture,
        current_output: &mut &'a wgpu::Texture,
    ) {
        let built_in_params = clip.eval_shader_params(clip_time, spec.composition.width, spec.composition.height, time, is_movie);
        self.queue.write_buffer(&self.built_in_params_buffer, 0, bytemuck::bytes_of(&built_in_params));

        let depth_texture_ref = clip.get_depth_map_asset_id()
            .and_then(|asset_id| self.gpu_textures.get(&asset_id))
            .unwrap_or(&self.transparent_texture);
        let depth_view = create_default_view(depth_texture_ref);

        let frame_idx = (time * spec.composition.fps as f32).round() as u32;
        let (feedback_in, feedback_out) = if frame_idx % 2 == 0 {
            (&self.feedback_texture_a, &self.feedback_texture_b)
        } else {
            (&self.feedback_texture_b, &self.feedback_texture_a)
        };
        let feedback_in_view = create_default_view(feedback_in);
        let feedback_out_view = create_default_view(feedback_out);

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Built-in Effect Bind Group"),
            layout: &self.effects_wgsl_bind_group_layout,
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
                    resource: self.built_in_params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&feedback_in_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&feedback_out_view),
                },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Built-in Effect Dispatch"),
        });
        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Built-in Effect Compute Pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.effects_wgsl_pipeline);
            compute_pass.set_bind_group(0, &bind_group, &[]);
            compute_pass.dispatch_workgroups(self.workgroups_x, self.workgroups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
        std::mem::swap(current_input, current_output);
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
    /// # Premultiplied alpha conversion
    ///
    /// The shader pipeline works in **straight (un-associated) alpha**, but
    /// downstream consumers (PNG encoders, video muxers) expect
    /// **premultiplied alpha**. This method multiplies each channel
    /// (`R`, `G`, `B`) by `A / 255`, leaving `A` untouched, so that
    /// semi-transparent regions composite correctly in the final output.
    fn readback_pixels(&self, source_texture: &wgpu::Texture, spec: &RenderSpec) -> Vec<u8> {
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
            let mut unpadded_pixels = Vec::with_capacity((spec.composition.width * spec.composition.height * 4) as usize);
            for row in 0..spec.composition.height {
                let start = (row * self.bytes_per_row) as usize;
                let end = start + (spec.composition.width * 4) as usize;
                let row_data = &data[start..end];
                
                // Premultiplied alpha: multiply RGB by normalised alpha
                for chunk in row_data.chunks_exact(4) {
                    let r = chunk[0];
                    let g = chunk[1];
                    let b = chunk[2];
                    let a = chunk[3];
                    
                    let alpha_factor = a as f32 / 255.0;
                    let r_pre = (r as f32 * alpha_factor).round().clamp(0.0, 255.0) as u8;
                    let g_pre = (g as f32 * alpha_factor).round().clamp(0.0, 255.0) as u8;
                    let b_pre = (b as f32 * alpha_factor).round().clamp(0.0, 255.0) as u8;
                    unpadded_pixels.push(r_pre);
                    unpadded_pixels.push(g_pre);
                    unpadded_pixels.push(b_pre);
                    unpadded_pixels.push(a);
                }
            }
            drop(data);
            self.readback_buffer.unmap();
            unpadded_pixels
        } else {
            error!("Failed to map readback buffer back to CPU!");
            std::process::exit(1);
        }
    }
}
