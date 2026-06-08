mod config;

use config::RenderSpec;
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;
use log::{info, error};
use std::sync::Mutex;
use serde::Serialize;

struct DualLogger {
    file: Mutex<Option<File>>,
}

impl log::Log for DualLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let msg = format!("[{}] {}", record.level(), record.args());
            eprintln!("{}", msg);
            if let Ok(mut opt_file) = self.file.lock() {
                if let Some(ref mut file) = *opt_file {
                    let _ = writeln!(file, "{}", msg);
                }
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut opt_file) = self.file.lock() {
            if let Some(ref mut file) = *opt_file {
                let _ = file.flush();
            }
        }
    }
}

static LOGGER: DualLogger = DualLogger {
    file: Mutex::new(None),
};

#[derive(Serialize)]
struct ReviewFrame {
    frame: u32,
    timestamp: f32,
    file: String,
    explanation: String,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct EngineParams {
    time: f32,
    clip_time: f32,
    progress: f32,
    width: u32,
    height: u32,
    _padding: [u32; 3],
}

fn pack_custom_params(params: &std::collections::HashMap<String, serde_json::Value>) -> Vec<u8> {
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort();
    
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

    // Initialize custom logger
    log::set_logger(&LOGGER)
        .map(|()| log::set_max_level(log::LevelFilter::Info))
        .expect("Failed to set logger");

    info!("Starting Render PoC...");

    // 1. Parse CLI arguments
    let args: Vec<String> = std::env::args().collect();
    let mut spec_path_str = None;
    let mut debug_dir_path_str = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-i" => {
                if i + 1 < args.len() {
                    spec_path_str = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    error!("Missing value for -i option");
                    std::process::exit(1);
                }
            }
            "--debug" => {
                if i + 1 < args.len() {
                    debug_dir_path_str = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    error!("Missing value for --debug option");
                    std::process::exit(1);
                }
            }
            s if s.starts_with('-') => {
                error!("Unknown option: {}", s);
                std::process::exit(1);
            }
            s => {
                spec_path_str = Some(s.to_string());
                i += 1;
            }
        }
    }

    let spec_path_str = spec_path_str.unwrap_or_else(|| "spec.json".to_string());
    let spec_path = Path::new(&spec_path_str);
    if !spec_path.exists() {
        error!("Spec file '{}' not found!", spec_path_str);
        std::process::exit(1);
    }

    let spec_load_start = Instant::now();
    let file = File::open(spec_path).expect("Failed to open spec file");
    let reader = BufReader::new(file);
    let spec: RenderSpec = serde_json::from_reader(reader).expect("Failed to parse spec file");
    info!("Loaded spec: {:?}", spec);
    let spec_load_dur = spec_load_start.elapsed();

    // 1a. Setup debug run directory and logs.txt if debug mode is requested
    let mut run_dir_path: Option<std::path::PathBuf> = None;
    if let Some(ref debug_dir_str) = debug_dir_path_str {
        let spec_file_name = spec_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("spec")
            .replace('.', "_");
        let debug_dir = Path::new(debug_dir_str);
        
        let mut counter = 1;
        let run_dir = loop {
            let folder_name = format!("{}_render_{:04}", spec_file_name, counter);
            let path = debug_dir.join(folder_name);
            if !path.exists() {
                break path;
            }
            counter += 1;
        };

        std::fs::create_dir_all(&run_dir).expect("Failed to create debug run directory");
        std::fs::create_dir_all(run_dir.join("frames")).expect("Failed to create frames directory");

        let logs_file = File::create(run_dir.join("logs.txt")).expect("Failed to create logs.txt");
        if let Ok(mut file_guard) = LOGGER.file.lock() {
            *file_guard = Some(logs_file);
        }
        info!("Debug mode enabled. Output directory: {:?}", run_dir);
        run_dir_path = Some(run_dir);
    }

    // 1b. Determine spot check frames if debug is active
    let mut spot_check_frames = std::collections::HashMap::new();
    if run_dir_path.is_some() {
        let fps = spec.composition.fps;
        let duration = spec.composition.duration;
        let is_movie = spec.output.ends_with(".mp4");
        let num_frames = if is_movie {
            (fps as f32 * duration).round() as u32
        } else {
            1
        };

        let raw_events = spec.get_spot_check_events();
        
        // Group raw events by target frame index
        for (time, explanation) in raw_events {
            let frame_idx = if is_movie {
                ((time * fps as f32).round() as u32).min(num_frames - 1)
            } else {
                0
            };
            spot_check_frames
                .entry(frame_idx)
                .or_insert_with(Vec::new)
                .push((time, explanation));
        }
    }

    // 2. Load all assets into CPU memory first
    let img_load_start = Instant::now();
    let mut raw_images = std::collections::HashMap::new();
    for (asset_id, asset) in &spec.assets {
        match asset {
            config::Asset::Image { path } | config::Asset::Video { path } => {
                let img_path = Path::new(path);
                if img_path.exists() {
                    let img = image::open(img_path).expect(&format!("Failed to open asset image {}", path));
                    let img = img.resize_exact(spec.composition.width, spec.composition.height, image::imageops::FilterType::Lanczos3);
                    let rgba = img.to_rgba8();
                    raw_images.insert(asset_id.clone(), rgba);
                } else {
                    error!("Asset file '{}' not found!", path);
                }
            }
            _ => {}
        }
    }
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
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .expect("Failed to create device!");
    let gpu_init_dur = gpu_init_start.elapsed();
    let mem_after_gpu = get_resident_set_size();

    // 4. Set up GPU textures and resources
    let resources_start = Instant::now();
    let texture_size = wgpu::Extent3d {
        width: spec.composition.width,
        height: spec.composition.height,
        depth_or_array_layers: 1,
    };

    let mut gpu_textures = std::collections::HashMap::new();
    for (asset_id, rgba) in &raw_images {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("Texture Asset: {}", asset_id)),
            size: texture_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba.as_raw(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(spec.composition.width * 4),
                rows_per_image: Some(spec.composition.height),
            },
            texture_size,
        );
        gpu_textures.insert(asset_id.clone(), texture);
    }

    let transparent_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Default Transparent Texture"),
        size: texture_size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let transparent_pixels = vec![0u8; (spec.composition.width * spec.composition.height * 4) as usize];
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &transparent_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &transparent_pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(spec.composition.width * 4),
            rows_per_image: Some(spec.composition.height),
        },
        texture_size,
    );

    let texture_a = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Compositor Texture A"),
        size: texture_size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let texture_b = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Compositor Texture B"),
        size: texture_size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });

    let compositor_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Compositor Params Buffer"),
        size: std::mem::size_of::<config::CompositorParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let engine_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Engine Params Buffer"),
        size: std::mem::size_of::<EngineParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let resources_dur = resources_start.elapsed();

    // 6. Set up compute pipelines and compile shaders
    let pipeline_start = Instant::now();
    info!("Compiling compositor WGSL shader...");
    let compositor_shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Compositor Shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("compositor.wgsl"))),
    });

    info!("Compiling built-in effects WGSL shader...");
    let effects_wgsl_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Built-in Effects Shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("effects.wgsl"))),
    });

    let mut custom_shader_modules = std::collections::HashMap::new();
    for (asset_id, asset) in &spec.assets {
        if let config::Asset::Shader { path } = asset {
            let shader_path = Path::new(path);
            if shader_path.exists() {
                let shader_source = std::fs::read_to_string(shader_path)
                    .expect(&format!("Failed to read custom shader source at {}", path));
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(&format!("Custom Shader Module: {}", asset_id)),
                    source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(shader_source)),
                });
                custom_shader_modules.insert(asset_id.clone(), module);
            } else {
                error!("Custom shader asset file '{}' not found!", path);
            }
        }
    }

    let compositor_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Compositor Bind Group Layout"),
        entries: &[
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
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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

    let compositor_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Compositor Pipeline Layout"),
        bind_group_layouts: &[&compositor_bind_group_layout],
        push_constant_ranges: &[],
    });

    let compositor_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Compositor Compute Pipeline"),
        layout: Some(&compositor_pipeline_layout),
        module: &compositor_shader_module,
        entry_point: "main",
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });

    let effect_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Custom Effect Bind Group Layout"),
        entries: &[
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
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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

    let effect_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Effect Pipeline Layout"),
        bind_group_layouts: &[&effect_bind_group_layout],
        push_constant_ranges: &[],
    });

    let mut custom_shader_pipelines = std::collections::HashMap::new();
    for (asset_id, module) in &custom_shader_modules {
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&format!("Custom Shader Pipeline: {}", asset_id)),
            layout: Some(&effect_pipeline_layout),
            module,
            entry_point: "main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        custom_shader_pipelines.insert(asset_id.clone(), pipeline);
    }

    let effects_wgsl_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("effects.wgsl Bind Group Layout"),
        entries: &[
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

    let effects_wgsl_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("effects.wgsl Pipeline Layout"),
        bind_group_layouts: &[&effects_wgsl_bind_group_layout],
        push_constant_ranges: &[],
    });

    let effects_wgsl_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("effects.wgsl Compute Pipeline"),
        layout: Some(&effects_wgsl_pipeline_layout),
        module: &effects_wgsl_module,
        entry_point: "main",
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    let pipeline_dur = pipeline_start.elapsed();

    // 7. Allocate readback buffer (calculated from width and height once)
    let bytes_per_pixel = 4;
    let unaligned_bytes_per_row = spec.composition.width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bytes_per_row = (unaligned_bytes_per_row + align - 1) & !(align - 1);
    let buffer_size = bytes_per_row * spec.composition.height;

    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Readback Buffer"),
        size: buffer_size as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut render_dur = std::time::Duration::from_secs(0);
    let mut save_dur = std::time::Duration::from_secs(0);

    let is_movie = spec.output.ends_with(".mp4");

    // Local helper to render a single frame at a specific time and read it back
    let render_frame = |time: f32| -> Vec<u8> {
        let mut current_input = &texture_a;
        let mut current_output = &texture_b;

        // Clear current_input to transparent black using a render pass
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
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
        queue.submit(Some(encoder.finish()));

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
                match clip.clip_type.as_str() {
                    "media" | "solid" => {
                        let position = if let Some(ref trans) = clip.transform {
                            if let Some(ref pos_val) = trans.position {
                                config::evaluate_vec2(pos_val, clip_time, spec.composition.width, spec.composition.height, [spec.composition.width as f32 * 0.5, spec.composition.height as f32 * 0.5])
                            } else {
                                [spec.composition.width as f32 * 0.5, spec.composition.height as f32 * 0.5]
                            }
                        } else {
                            [spec.composition.width as f32 * 0.5, spec.composition.height as f32 * 0.5]
                        };

                        let scale = if let Some(ref trans) = clip.transform {
                            if let Some(ref scale_val) = trans.scale {
                                config::evaluate_vec2(scale_val, clip_time, spec.composition.width, spec.composition.height, [1.0, 1.0])
                            } else {
                                [1.0, 1.0]
                            }
                        } else {
                            [1.0, 1.0]
                        };

                        let rotation = if let Some(ref trans) = clip.transform {
                            if let Some(ref rot_val) = trans.rotation {
                                config::evaluate_float(rot_val, clip_time, spec.composition.width, spec.composition.height, 0.0)
                            } else {
                                0.0
                            }
                        } else {
                            0.0
                        };

                        let opacity = if let Some(ref trans) = clip.transform {
                            if let Some(ref op_val) = trans.opacity {
                                config::evaluate_float(op_val, clip_time, spec.composition.width, spec.composition.height, 1.0)
                            } else {
                                1.0
                            }
                        } else {
                            1.0
                        };

                        let blend_mode_str = clip.blend_mode.as_deref().unwrap_or("normal");
                        let blend_mode_u32 = match blend_mode_str {
                            "normal" => 0,
                            "multiply" => 1,
                            "screen" => 2,
                            "overlay" => 3,
                            "darken" => 4,
                            "lighten" => 5,
                            "color_dodge" => 6,
                            "color_burn" => 7,
                            "hard_light" => 8,
                            "soft_light" => 9,
                            "difference" => 10,
                            "exclusion" => 11,
                            _ => 0,
                        };

                        let mut grayscale = 0u32;
                        let mut brightness = 1.0f32;
                        for effect in &clip.effects {
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

                        let (clip_type_u32, solid_color) = match clip.clip_type.as_str() {
                            "solid" => {
                                let color = clip.solid_params.as_ref().map(|p| p.color).unwrap_or([0.0, 0.0, 0.0, 1.0]);
                                (1u32, color)
                            }
                            _ => (0u32, [0.0, 0.0, 0.0, 1.0]),
                        };

                        let comp_params = config::CompositorParams {
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
                        queue.write_buffer(&compositor_params_buffer, 0, bytemuck::bytes_of(&comp_params));

                        let media_texture_ref = clip.asset.as_ref()
                            .and_then(|asset_id| gpu_textures.get(asset_id))
                            .unwrap_or(&transparent_texture);
                        let media_texture_view = media_texture_ref.create_view(&wgpu::TextureViewDescriptor::default());

                        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("Compositor Bind Group"),
                            layout: &compositor_bind_group_layout,
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
                                    resource: compositor_params_buffer.as_entire_binding(),
                                },
                            ],
                        });

                        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Compositor Dispatch"),
                        });
                        {
                            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                label: Some("Compositor Compute Pass"),
                                timestamp_writes: None,
                            });
                            compute_pass.set_pipeline(&compositor_pipeline);
                            compute_pass.set_bind_group(0, &bind_group, &[]);
                            let workgroups_x = (spec.composition.width + 15) / 16;
                            let workgroups_y = (spec.composition.height + 15) / 16;
                            compute_pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
                        }
                        queue.submit(Some(encoder.finish()));
                        std::mem::swap(&mut current_input, &mut current_output);
                    }
                    "effect" => {
                        let progress = (clip_time / clip.duration).clamp(0.0, 1.0);
                        if let Some(ref shader_id) = clip.shader {
                            if let Some(pipeline) = custom_shader_pipelines.get(shader_id) {
                                let engine_params = EngineParams {
                                    time,
                                    clip_time,
                                    progress,
                                    width: spec.composition.width,
                                    height: spec.composition.height,
                                    _padding: [0, 0, 0],
                                };
                                queue.write_buffer(&engine_params_buffer, 0, bytemuck::bytes_of(&engine_params));

                                let custom_params_data = if let Some(ref p) = clip.params {
                                    pack_custom_params(p)
                                } else {
                                    vec![0u8; 16]
                                };

                                let custom_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                                    label: Some("Custom Params Buffer"),
                                    size: custom_params_data.len() as u64,
                                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                                    mapped_at_creation: false,
                                });
                                queue.write_buffer(&custom_params_buffer, 0, &custom_params_data);

                                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                                    label: Some("Custom Effect Bind Group"),
                                    layout: &effect_bind_group_layout,
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
                                            resource: engine_params_buffer.as_entire_binding(),
                                        },
                                        wgpu::BindGroupEntry {
                                            binding: 3,
                                            resource: custom_params_buffer.as_entire_binding(),
                                        },
                                    ],
                                });

                                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                    label: Some("Custom Effect Dispatch"),
                                });
                                {
                                    let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("Custom Effect Compute Pass"),
                                        timestamp_writes: None,
                                    });
                                    compute_pass.set_pipeline(pipeline);
                                    compute_pass.set_bind_group(0, &bind_group, &[]);
                                    let workgroups_x = (spec.composition.width + 15) / 16;
                                    let workgroups_y = (spec.composition.height + 15) / 16;
                                    compute_pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
                                }
                                queue.submit(Some(encoder.finish()));
                                std::mem::swap(&mut current_input, &mut current_output);
                            } else {
                                error!("Custom shader pipeline '{}' not found!", shader_id);
                            }
                        } else {
                            // Built-in effects.wgsl
                            let mut grayscale = 0u32;
                            let mut brightness = 1.0f32;
                            for effect in &clip.effects {
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

                            let built_in_params = config::ShaderParams {
                                grayscale,
                                brightness,
                                _padding: [0, 0],
                            };

                            let built_in_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some("Built-in Effect Params Buffer"),
                                size: std::mem::size_of::<config::ShaderParams>() as u64,
                                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                                mapped_at_creation: false,
                            });
                            queue.write_buffer(&built_in_params_buffer, 0, bytemuck::bytes_of(&built_in_params));

                            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("Built-in Effect Bind Group"),
                                layout: &effects_wgsl_bind_group_layout,
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
                                        resource: built_in_params_buffer.as_entire_binding(),
                                    },
                                ],
                            });

                            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("Built-in Effect Dispatch"),
                            });
                            {
                                let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("Built-in Effect Compute Pass"),
                                    timestamp_writes: None,
                                });
                                compute_pass.set_pipeline(&effects_wgsl_pipeline);
                                compute_pass.set_bind_group(0, &bind_group, &[]);
                                let workgroups_x = (spec.composition.width + 15) / 16;
                                let workgroups_y = (spec.composition.height + 15) / 16;
                                compute_pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
                            }
                            queue.submit(Some(encoder.finish()));
                            std::mem::swap(&mut current_input, &mut current_output);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Read the final composition (current_input) back to CPU
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
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
                buffer: &readback_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(spec.composition.height),
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
            let mut unpadded_pixels = Vec::with_capacity((spec.composition.width * spec.composition.height * 4) as usize);
            for row in 0..spec.composition.height {
                let start = (row * bytes_per_row) as usize;
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
            readback_buffer.unmap();
            unpadded_pixels
        } else {
            error!("Failed to map buffer back to CPU at time {}!", time);
            std::process::exit(1);
        }
    };

    if is_movie {
        let fps = spec.composition.fps;
        let duration = spec.composition.duration;
        let num_frames = (fps as f32 * duration).round() as u32;
        info!("Rendering video movie: {} frames at {} FPS ({} seconds)...", num_frames, fps, duration);

        let save_start_inst = Instant::now();
        let mut ffmpeg_args = vec![
            "-y".to_string(),
            "-f".to_string(),
            "rawvideo".to_string(),
            "-pix_fmt".to_string(),
            "rgba".to_string(),
            "-s".to_string(),
            format!("{}x{}", spec.composition.width, spec.composition.height),
            "-r".to_string(),
            fps.to_string(),
            "-i".to_string(),
            "-".to_string(),
        ];

        let audio_clips = spec.get_audio_clips();
        for (path, _, _) in &audio_clips {
            ffmpeg_args.push("-i".to_string());
            ffmpeg_args.push(path.clone());
        }

        ffmpeg_args.push("-c:v".to_string());
        ffmpeg_args.push("libx264".to_string());
        ffmpeg_args.push("-pix_fmt".to_string());
        ffmpeg_args.push("yuv420p".to_string());

        if !audio_clips.is_empty() {
            let mut filter_complex = String::new();
            if audio_clips.len() == 1 {
                let (_, start, duration) = audio_clips[0];
                let start_ms = (start * 1000.0).round() as u32;
                filter_complex = format!(
                    "[1:a]atrim=0:{:.3},adelay={}|{}[aout]",
                    duration, start_ms, start_ms
                );
            } else {
                for (idx, (_, start, duration)) in audio_clips.iter().enumerate() {
                    let input_idx = idx + 1; // 0 is video stdin
                    let start_ms = (start * 1000.0).round() as u32;
                    filter_complex.push_str(&format!(
                        "[{}:a]atrim=0:{:.3},adelay={}|{}[a{}];",
                        input_idx, duration, start_ms, start_ms, input_idx
                    ));
                }
                for idx in 0..audio_clips.len() {
                    filter_complex.push_str(&format!("[a{}]", idx + 1));
                }
                filter_complex.push_str(&format!("amix=inputs={}[aout]", audio_clips.len()));
            }

            ffmpeg_args.push("-filter_complex".to_string());
            ffmpeg_args.push(filter_complex);
            ffmpeg_args.push("-map".to_string());
            ffmpeg_args.push("0:v".to_string());
            ffmpeg_args.push("-map".to_string());
            ffmpeg_args.push("[aout]".to_string());
            ffmpeg_args.push("-c:a".to_string());
            ffmpeg_args.push("aac".to_string());
            ffmpeg_args.push("-shortest".to_string());
        }

        ffmpeg_args.push(spec.output.clone());

        let mut ffmpeg_cmd = Command::new("ffmpeg")
            .args(&ffmpeg_args)
            .stdin(Stdio::piped())
            .spawn()
            .expect("Failed to spawn ffmpeg process");
        
        let mut ffmpeg_stdin = ffmpeg_cmd.stdin.take().expect("Failed to open stdin for ffmpeg");
        save_dur += save_start_inst.elapsed();

        for frame in 0..num_frames {
            let time = frame as f32 / fps as f32;
            
            let frame_render_start = Instant::now();
            let unpadded_pixels = render_frame(time);
            render_dur += frame_render_start.elapsed();

            if let Some(ref run_dir) = run_dir_path {
                if spot_check_frames.contains_key(&frame) {
                    let frame_save_path = run_dir.join("frames").join(format!("frame_{:04}.png", frame));
                    image::save_buffer(
                        &frame_save_path,
                        &unpadded_pixels,
                        spec.composition.width,
                        spec.composition.height,
                        image::ExtendedColorType::Rgba8,
                    ).expect("Failed to save debug spot check frame");
                    info!("Exported debug frame {} to {:?}", frame, frame_save_path);
                }
            }

            let frame_save_start = Instant::now();
            ffmpeg_stdin.write_all(&unpadded_pixels).expect("Failed to write raw frame to ffmpeg");
            save_dur += frame_save_start.elapsed();
        }

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
        let unpadded_pixels = render_frame(0.0);
        render_dur += frame_render_start.elapsed();

        if let Some(ref run_dir) = run_dir_path {
            let frame_save_path = run_dir.join("frames").join("frame_0000.png");
            image::save_buffer(
                &frame_save_path,
                &unpadded_pixels,
                spec.composition.width,
                spec.composition.height,
                image::ExtendedColorType::Rgba8,
            ).expect("Failed to save debug spot check frame");
            info!("Exported debug frame 0 to {:?}", frame_save_path);
        }

        let frame_save_start = Instant::now();
        image::save_buffer(
            &spec.output,
            &unpadded_pixels,
            spec.composition.width,
            spec.composition.height,
            image::ExtendedColorType::Rgba8,
        )
        .expect("Failed to save output image");
        save_dur += frame_save_start.elapsed();
    }

    // 8. Compile and write review.json and clean up logger if in debug mode
    if let Some(ref run_dir) = run_dir_path {
        let mut review_frames = Vec::new();
        let mut sorted_keys: Vec<&u32> = spot_check_frames.keys().collect();
        sorted_keys.sort();
        for &frame in sorted_keys {
            if let Some(events) = spot_check_frames.get(&frame) {
                let mut explanations = Vec::new();
                let mut sum_time = 0.0;
                for (t, expl) in events {
                    explanations.push(expl.clone());
                    sum_time += t;
                }
                let avg_time = sum_time / events.len() as f32;
                let explanation = explanations.join("; ");
                review_frames.push(ReviewFrame {
                    frame,
                    timestamp: avg_time,
                    file: format!("frames/frame_{:04}.png", frame),
                    explanation,
                });
            }
        }

        let review_json_path = run_dir.join("review.json");
        let review_file = File::create(review_json_path).expect("Failed to create review.json");
        serde_json::to_writer_pretty(review_file, &review_frames).expect("Failed to write review.json");
        info!("Written review.json to {:?}", run_dir.join("review.json"));

        // Flush and detach log file
        log::logger().flush();
        if let Ok(mut file_guard) = LOGGER.file.lock() {
            *file_guard = None;
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
