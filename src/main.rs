mod config;

use config::RenderSpec;
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;
use log::{info, error};

#[cfg(target_os = "macos")]
fn get_resident_set_size() -> Option<usize> {
    use std::mem;
    let mut info: libc::mach_task_basic_info = unsafe { mem::zeroed() };
    let mut count = (mem::size_of::<libc::mach_task_basic_info>() / mem::size_of::<libc::integer_t>()) as libc::mach_msg_type_number_t;
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut libc::integer_t,
            &mut count,
        )
    };
    if result == libc::KERN_SUCCESS {
        Some(info.resident_size as usize)
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn get_resident_set_size() -> Option<usize> {
    None
}

fn main() {
    let start_time = Instant::now();
    let mem_start = get_resident_set_size();

    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    info!("Starting Render PoC...");

    // 1. Load JSON spec (from command line argument or default to spec.json)
    let spec_load_start = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let spec_path_str = args.get(1).map(|s| s.as_str()).unwrap_or("spec.json");
    let spec_path = Path::new(spec_path_str);
    if !spec_path.exists() {
        error!("Spec file '{}' not found!", spec_path_str);
        std::process::exit(1);
    }

    let file = File::open(spec_path).expect("Failed to open spec file");
    let reader = BufReader::new(file);
    let spec: RenderSpec = serde_json::from_reader(reader).expect("Failed to parse spec file");
    info!("Loaded spec: {:?}", spec);
    let spec_load_dur = spec_load_start.elapsed();

    // 2. Load input image
    let img_load_start = Instant::now();
    let input_path = Path::new(&spec.input);
    if !input_path.exists() {
        error!("Input image '{}' not found!", spec.input);
        std::process::exit(1);
    }

    let img = image::open(input_path).expect("Failed to open input image");
    let (img_w, img_h) = (img.width(), img.height());
    info!("Input image dimensions: {}x{}", img_w, img_h);

    // If input image dimensions differ from spec, resize to match the spec
    let img = if img_w != spec.width || img_h != spec.height {
        info!("Resizing input image to match spec dimensions: {}x{}", spec.width, spec.height);
        img.resize_exact(spec.width, spec.height, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let raw_pixels = rgba.as_raw();
    let img_load_dur = img_load_start.elapsed();

    // 3. Initialize Headless wgpu
    let gpu_init_start = Instant::now();
    info!("Initializing wgpu instance...");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    info!("Requesting adapter...");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("Failed to find an appropriate GPU adapter!");

    let info = adapter.get_info();
    info!("Using GPU adapter: {} ({:?})", info.name, info.backend);

    info!("Requesting device and queue...");
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("Headless Device"),
            required_features: wgpu::Features::empty(),
            // Ensure limits are suitable for our texture sizes
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .expect("Failed to create device!");
    let gpu_init_dur = gpu_init_start.elapsed();
    let mem_after_gpu = get_resident_set_size();

    // 4. Set up GPU textures
    let resources_start = Instant::now();
    let texture_size = wgpu::Extent3d {
        width: spec.width,
        height: spec.height,
        depth_or_array_layers: 1,
    };

    let input_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Input Texture"),
        size: texture_size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    // Write input image pixels to GPU texture
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &input_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        raw_pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(spec.width * 4),
            rows_per_image: Some(spec.height),
        },
        texture_size,
    );

    let output_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Output Texture"),
        size: texture_size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Storage textures on wgpu require non-srgb formats like Rgba8Unorm
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    // 5. Create uniform buffer for shader params
    let shader_params = spec.to_shader_params(0.0);
    info!("Shader params: {:?}", shader_params);

    let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Params Buffer"),
        size: std::mem::size_of::<config::ShaderParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    queue.write_buffer(&params_buffer, 0, bytemuck::bytes_of(&shader_params));
    let resources_dur = resources_start.elapsed();

    // 6. Set up compute pipeline
    let pipeline_start = Instant::now();
    info!("Compiling WGSL shader...");
    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Effects Shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("effects.wgsl"))),
    });

    // Bind groups
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Compute Bind Group Layout"),
        entries: &[
            // Input Texture
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            // Output Texture
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            // Uniform Params
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Compute Bind Group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&input_texture.create_view(&wgpu::TextureViewDescriptor::default())),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&output_texture.create_view(&wgpu::TextureViewDescriptor::default())),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Compute Pipeline Layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Compute Pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader_module,
        entry_point: "main",
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    let pipeline_dur = pipeline_start.elapsed();

    // 7. Allocate readback buffer (calculated from width and height once)
    let bytes_per_pixel = 4;
    let unaligned_bytes_per_row = spec.width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bytes_per_row = (unaligned_bytes_per_row + align - 1) & !(align - 1);
    let buffer_size = bytes_per_row * spec.height;

    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Readback Buffer"),
        size: buffer_size as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut render_dur = std::time::Duration::from_secs(0);
    let mut save_dur = std::time::Duration::from_secs(0);

    let is_movie = spec.fps.is_some() && spec.duration.is_some();
    if is_movie {
        let fps = spec.fps.unwrap();
        let duration = spec.duration.unwrap();
        let num_frames = (fps as f32 * duration).round() as u32;
        info!("Rendering video movie: {} frames at {} FPS ({} seconds)...", num_frames, fps, duration);

        let save_start_inst = Instant::now();
        let mut ffmpeg_cmd = Command::new("ffmpeg")
            .args(&[
                "-y",
                "-f", "rawvideo",
                "-pix_fmt", "rgba",
                "-s", &format!("{}x{}", spec.width, spec.height),
                "-r", &fps.to_string(),
                "-i", "-",
                "-c:v", "libx264",
                "-pix_fmt", "yuv420p",
                &spec.output,
            ])
            .stdin(Stdio::piped())
            .spawn()
            .expect("Failed to spawn ffmpeg process");
        
        let mut ffmpeg_stdin = ffmpeg_cmd.stdin.take().expect("Failed to open stdin for ffmpeg");
        save_dur += save_start_inst.elapsed();

        for frame in 0..num_frames {
            let time = frame as f32 / fps as f32;
            
            let frame_render_start = Instant::now();
            // Update params uniform buffer for the current frame time
            let shader_params = spec.to_shader_params(time);
            queue.write_buffer(&params_buffer, 0, bytemuck::bytes_of(&shader_params));

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(&format!("Compute Encoder Frame {}", frame)),
            });

            {
                let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(&format!("Compute Pass Frame {}", frame)),
                    timestamp_writes: None,
                });
                compute_pass.set_pipeline(&compute_pipeline);
                compute_pass.set_bind_group(0, &bind_group, &[]);
                
                let workgroups_x = (spec.width + 15) / 16;
                let workgroups_y = (spec.height + 15) / 16;
                compute_pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
            }

            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &output_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &readback_buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(spec.height),
                    },
                },
                texture_size,
            );

            queue.submit(Some(encoder.finish()));

            // Read buffer back
            let buffer_slice = readback_buffer.slice(..);
            let (sender, receiver) = std::sync::mpsc::channel();
            buffer_slice.map_async(wgpu::MapMode::Read, move |v| {
                sender.send(v).unwrap();
            });

            device.poll(wgpu::Maintain::Wait);

            if let Ok(Ok(())) = receiver.recv() {
                let data = buffer_slice.get_mapped_range();
                
                let mut unpadded_pixels = Vec::with_capacity((spec.width * spec.height * 4) as usize);
                for row in 0..spec.height {
                    let start = (row * bytes_per_row) as usize;
                    let end = start + (spec.width * 4) as usize;
                    unpadded_pixels.extend_from_slice(&data[start..end]);
                }
                drop(data);
                readback_buffer.unmap();
                
                render_dur += frame_render_start.elapsed();

                // Pipe to ffmpeg
                let frame_save_start = Instant::now();
                ffmpeg_stdin.write_all(&unpadded_pixels).expect("Failed to write raw frame to ffmpeg");
                save_dur += frame_save_start.elapsed();
            } else {
                error!("Failed to map buffer back to CPU at frame {}!", frame);
                std::process::exit(1);
            }
        }

        // Close stdin to let ffmpeg complete encoding
        let finish_save_start = Instant::now();
        std::mem::drop(ffmpeg_stdin);
        let status = ffmpeg_cmd.wait().expect("Failed to wait for ffmpeg");
        if !status.success() {
            error!("ffmpeg process failed with exit code: {:?}", status.code());
        }
        save_dur += finish_save_start.elapsed();
    } else {
        info!("Rendering single image...");
        let frame_render_start = Instant::now();
        
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Compute Encoder"),
        });

        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Compute Pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&compute_pipeline);
            compute_pass.set_bind_group(0, &bind_group, &[]);
            
            let workgroups_x = (spec.width + 15) / 16;
            let workgroups_y = (spec.height + 15) / 16;
            compute_pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(spec.height),
                },
            },
            texture_size,
        );

        queue.submit(Some(encoder.finish()));

        let buffer_slice = readback_buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).unwrap();
        });

        device.poll(wgpu::Maintain::Wait);

        if let Ok(Ok(())) = receiver.recv() {
            let data = buffer_slice.get_mapped_range();
            
            let mut unpadded_pixels = Vec::with_capacity((spec.width * spec.height * 4) as usize);
            for row in 0..spec.height {
                let start = (row * bytes_per_row) as usize;
                let end = start + (spec.width * 4) as usize;
                unpadded_pixels.extend_from_slice(&data[start..end]);
            }
            drop(data);
            readback_buffer.unmap();

            render_dur += frame_render_start.elapsed();

            let frame_save_start = Instant::now();
            image::save_buffer(
                &spec.output,
                &unpadded_pixels,
                spec.width,
                spec.height,
                image::ExtendedColorType::Rgba8,
            )
            .expect("Failed to save output image");
            save_dur += frame_save_start.elapsed();
        } else {
            error!("Failed to map buffer back to CPU!");
            std::process::exit(1);
        }
    }

    let mem_after_render = get_resident_set_size();
    let total_dur = start_time.elapsed();

    info!("================ Performance Profile ================");
    info!("Total Duration:       {:?}", total_dur);
    info!("- Config parsing:      {:?}", spec_load_dur);
    info!("- Input asset load:   {:?}", img_load_dur);
    info!("- GPU Init:           {:?}", gpu_init_dur);
    info!("- Resources creation: {:?}", resources_dur);
    info!("- Shader compilation: {:?}", pipeline_dur);
    info!("- Rendering loop:     {:?}", render_dur);
    info!("- Output saving/enc:  {:?}", save_dur);
    if let Some(rss) = mem_start {
        info!("Memory Profile:");
        info!("- Startup RSS:        {:.2} MB", rss as f64 / 1024.0 / 1024.0);
    }
    if let Some(rss) = mem_after_gpu {
        info!("- Post-GPU Init RSS:  {:.2} MB", rss as f64 / 1024.0 / 1024.0);
    }
    if let Some(rss) = mem_after_render {
        info!("- Post-Render RSS:    {:.2} MB", rss as f64 / 1024.0 / 1024.0);
    }
    info!("=====================================================");
}
