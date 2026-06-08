use crate::config::{RenderSpec, ClipType, ShaderParams, CompositorParams};
use log::{error};

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

pub struct RenderContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub gpu_textures: std::collections::HashMap<String, wgpu::Texture>,
    pub transparent_texture: wgpu::Texture,
    pub texture_a: wgpu::Texture,
    pub texture_b: wgpu::Texture,
    pub compositor_params_buffer: wgpu::Buffer,
    pub engine_params_buffer: wgpu::Buffer,
    pub custom_params_buffer: wgpu::Buffer,
    pub built_in_params_buffer: wgpu::Buffer,
    pub compositor_pipeline: wgpu::ComputePipeline,
    pub compositor_bind_group_layout: wgpu::BindGroupLayout,
    pub effects_wgsl_pipeline: wgpu::ComputePipeline,
    pub effects_wgsl_bind_group_layout: wgpu::BindGroupLayout,
    pub custom_shader_pipelines: std::collections::HashMap<String, wgpu::ComputePipeline>,
    pub effect_bind_group_layout: wgpu::BindGroupLayout,
    pub readback_buffer: wgpu::Buffer,
    pub bytes_per_row: u32,
    pub texture_size: wgpu::Extent3d,
    pub workgroups_x: u32,
    pub workgroups_y: u32,
}

impl RenderContext {
    pub fn render_frame(&self, time: f32, spec: &RenderSpec, is_movie: bool) -> Vec<u8> {
        let mut current_input = &self.texture_a;
        let mut current_output = &self.texture_b;

        // Clear current_input to transparent black using a render pass
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Clear Base Texture"),
        });
        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Clear Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &current_input.create_view(&wgpu::TextureViewDescriptor::default()),
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
                        let media_texture_view = media_texture_ref.create_view(&wgpu::TextureViewDescriptor::default());

                        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("Compositor Bind Group"),
                            layout: &self.compositor_bind_group_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(&current_input.create_view(&wgpu::TextureViewDescriptor::default())),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::TextureView(&media_texture_view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 2,
                                    resource: wgpu::BindingResource::TextureView(&current_output.create_view(&wgpu::TextureViewDescriptor::default())),
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
                        std::mem::swap(&mut current_input, &mut current_output);
                    }
                    ClipType::Effect => {
                        let progress = (clip_time / clip.duration).clamp(0.0, 1.0);
                        if let Some(ref shader_id) = clip.shader {
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
                                            resource: wgpu::BindingResource::TextureView(&current_input.create_view(&wgpu::TextureViewDescriptor::default())),
                                        },
                                        wgpu::BindGroupEntry {
                                            binding: 1,
                                            resource: wgpu::BindingResource::TextureView(&current_output.create_view(&wgpu::TextureViewDescriptor::default())),
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
                                std::mem::swap(&mut current_input, &mut current_output);
                            } else {
                                error!("Custom shader pipeline '{}' not found!", shader_id);
                            }
                        } else {
                            // Built-in effects.wgsl
                            let (grayscale, brightness) = clip.eval_built_in_effects(clip_time, is_movie);

                            let built_in_params = ShaderParams {
                                grayscale,
                                brightness,
                                _padding: [0, 0],
                            };
                            self.queue.write_buffer(&self.built_in_params_buffer, 0, bytemuck::bytes_of(&built_in_params));

                            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("Built-in Effect Bind Group"),
                                layout: &self.effects_wgsl_bind_group_layout,
                                entries: &[
                                    wgpu::BindGroupEntry {
                                        binding: 0,
                                        resource: wgpu::BindingResource::TextureView(&current_input.create_view(&wgpu::TextureViewDescriptor::default())),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 1,
                                        resource: wgpu::BindingResource::TextureView(&current_output.create_view(&wgpu::TextureViewDescriptor::default())),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 2,
                                        resource: self.built_in_params_buffer.as_entire_binding(),
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
                            std::mem::swap(&mut current_input, &mut current_output);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Read the final composition (current_input) back to CPU
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Readback Encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: current_input,
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
            error!("Failed to map buffer back to CPU at time {}!", time);
            std::process::exit(1);
        }
    }
}
