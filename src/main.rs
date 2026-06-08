use log::{LevelFilter, error, info};
use render_poc::config::*;
use render_poc::engine::{EngineParams, RenderContext};
use serde::Serialize;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

struct DualLogger {
    file: Mutex<Option<File>>,
}

impl log::Log for DualLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let msg = format!("[{}] {}", record.level(), record.args());
            println!("{}", msg);
            if let Ok(mut file_guard) = self.file.lock() {
                if let Some(ref mut file) = *file_guard {
                    let _ = writeln!(file, "{}", msg);
                }
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut file_guard) = self.file.lock() {
            if let Some(ref mut file) = *file_guard {
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

#[cfg(target_os = "macos")]
fn get_resident_set_size() -> Option<usize> {
    None // Removed libc call to satisfy compiler/cleanliness since not essential
}

#[cfg(not(target_os = "macos"))]
fn get_resident_set_size() -> Option<usize> {
    None
}

fn main() {
    let start_time = Instant::now();
    let mem_start = get_resident_set_size();

    let args: Vec<String> = env::args().collect();
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
                    eprintln!("Error: Missing value for -i option");
                    std::process::exit(1);
                }
            }
            "--debug" => {
                if i + 1 < args.len() {
                    debug_dir_path_str = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: Missing value for --debug option");
                    std::process::exit(1);
                }
            }
            s if s.starts_with('-') => {
                eprintln!("Error: Unknown option: {}", s);
                std::process::exit(1);
            }
            s => {
                spec_path_str = Some(s.to_string());
                i += 1;
            }
        }
    }

    let spec_path_str = spec_path_str.unwrap_or_else(|| {
        eprintln!("Usage: render-poc [-i <spec.json>] [--debug <dir>]");
        std::process::exit(1);
    });
    let spec_path = std::path::Path::new(&spec_path_str);
    
    let mut run_dir_path = None;
    if let Some(ref debug_dir_str) = debug_dir_path_str {
        let spec_file_name = spec_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("spec")
            .replace('.', "_");
        let debug_dir = std::path::Path::new(debug_dir_str);
        
        let mut counter = 1;
        let run_dir = loop {
            let folder_name = format!("{}_render_{:04}", spec_file_name, counter);
            let path = debug_dir.join(folder_name);
            if !path.exists() {
                break path;
            }
            counter += 1;
        };
        run_dir_path = Some(run_dir);
    }

    if let Some(ref run_dir) = run_dir_path {
        std::fs::create_dir_all(run_dir.join("frames")).expect("Failed to create debug frames dir");
        let log_file = File::create(run_dir.join("logs.txt")).expect("Failed to create logs.txt");
        if let Ok(mut file_guard) = LOGGER.file.lock() {
            *file_guard = Some(log_file);
        }
    }

    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(LevelFilter::Info));

    let spec_start = Instant::now();
    let spec_file = File::open(spec_path).expect("Failed to open spec file");
    let reader = BufReader::new(spec_file);
    let spec: RenderSpec = serde_json::from_reader(reader).expect("Failed to parse JSON");
    let spec_load_dur = spec_start.elapsed();

    info!("Initializing headless video rendering pipeline...");
    info!("Composition size: {}x{}", spec.composition.width, spec.composition.height);

    let mut spot_check_frames = HashMap::new();
    if run_dir_path.is_some() {
        let events = spec.get_spot_check_events();
        let fps = spec.composition.fps;
        let is_movie = spec.output.ends_with(".mp4");
        let num_frames = if is_movie {
            (spec.composition.duration * fps as f32).round() as u32
        } else {
            1
        };
        for (time, expl) in events {
            let mut frame = (time * fps as f32).round() as u32;
            if frame >= num_frames && num_frames > 0 {
                frame = num_frames - 1;
            }
            spot_check_frames.entry(frame).or_insert_with(Vec::new).push((time, expl));
        }
    }

    let img_load_start = Instant::now();
    let mut cpu_images: HashMap<String, image::RgbaImage> = HashMap::new();
    for (asset_id, asset) in &spec.assets {
        match asset {
            Asset::Image { path } | Asset::Video { path } => {
                let img = image::open(path).unwrap_or_else(|_| panic!("Failed to load asset: {}", path));
                let img = img.resize_exact(spec.composition.width, spec.composition.height, image::imageops::FilterType::Lanczos3).to_rgba8();
                cpu_images.insert(asset_id.clone(), img);
                info!("Loaded asset '{}' from {}", asset_id, path);
            }
            _ => {}
        }
    }
    let img_load_dur = img_load_start.elapsed();

    let gpu_init_start = Instant::now();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    })).expect("Failed to find an appropriate adapter");

    info!("Using GPU: {:?}", adapter.get_info().name);

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    )).expect("Failed to create device");
    let gpu_init_dur = gpu_init_start.elapsed();

    let mem_after_gpu = get_resident_set_size();

    let resources_start = Instant::now();
    
    let texture_size = wgpu::Extent3d {
        width: spec.composition.width,
        height: spec.composition.height,
        depth_or_array_layers: 1,
    };

    let texture_desc = wgpu::TextureDescriptor {
        size: texture_size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
        label: Some("Ping/Pong Texture"),
        view_formats: &[],
    };
    
    let texture_a = device.create_texture(&texture_desc);
    let texture_b = device.create_texture(&texture_desc);

    let transparent_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Transparent Asset Texture"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture { texture: &transparent_texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        &[0, 0, 0, 0],
        wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
        wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
    );

    let mut gpu_textures = HashMap::new();
    for (asset_id, img) in &cpu_images {
        let tex_size = wgpu::Extent3d {
            width: img.width(),
            height: img.height(),
            depth_or_array_layers: 1,
        };
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(asset_id),
            size: tex_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            img,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(4 * img.width()), rows_per_image: Some(img.height()) },
            tex_size,
        );
        gpu_textures.insert(asset_id.clone(), tex);
    }

    let bytes_per_pixel = 4;
    let bytes_per_row = (spec.composition.width * bytes_per_pixel + 255) & !255;
    
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Readback Buffer"),
        size: (bytes_per_row * spec.composition.height) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let compositor_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Compositor Params Buffer"),
        size: std::mem::size_of::<CompositorParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let engine_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Engine Params Buffer"),
        size: std::mem::size_of::<EngineParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let custom_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Custom Params Buffer"),
        size: 1024,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let built_in_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Built-in Effect Params Buffer"),
        size: std::mem::size_of::<ShaderParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let resources_dur = resources_start.elapsed();

    let pipeline_start = Instant::now();
    let compositor_shader_str = std::fs::read_to_string("src/compositor.wgsl").expect("Failed to read compositor.wgsl");
    let compositor_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Compositor Shader"),
        source: wgpu::ShaderSource::Wgsl(compositor_shader_str.into()),
    });

    let compositor_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Compositor Bind Group Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::Rgba8Unorm, view_dimension: wgpu::TextureViewDimension::D2 }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
        ],
    });

    let compositor_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Compositor Pipeline Layout"),
        bind_group_layouts: &[&compositor_bind_group_layout],
        push_constant_ranges: &[],
    });

    let compositor_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Compositor Pipeline"),
        layout: Some(&compositor_pipeline_layout),
        module: &compositor_shader,
        entry_point: "main",
        cache: None,
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });

    let effects_wgsl_str = std::fs::read_to_string("src/effects.wgsl").expect("Failed to read effects.wgsl");
    let effects_wgsl_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Built-in Effects Shader"),
        source: wgpu::ShaderSource::Wgsl(effects_wgsl_str.into()),
    });

    let effects_wgsl_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Built-in Effect Bind Group Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::Rgba8Unorm, view_dimension: wgpu::TextureViewDimension::D2 }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
        ],
    });

    let effects_wgsl_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Built-in Effect Pipeline Layout"),
        bind_group_layouts: &[&effects_wgsl_bind_group_layout],
        push_constant_ranges: &[],
    });

    let effects_wgsl_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Built-in Effect Pipeline"),
        layout: Some(&effects_wgsl_pipeline_layout),
        module: &effects_wgsl_shader,
        entry_point: "main",
        cache: None,
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });

    let effect_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Effect Bind Group Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::Rgba8Unorm, view_dimension: wgpu::TextureViewDimension::D2 }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
        ],
    });

    let effect_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Effect Pipeline Layout"),
        bind_group_layouts: &[&effect_bind_group_layout],
        push_constant_ranges: &[],
    });

    let mut custom_shader_pipelines = HashMap::new();
    for (asset_id, asset) in &spec.assets {
        if let Asset::Shader { path } = asset {
            let shader_str = std::fs::read_to_string(path).unwrap_or_else(|_| panic!("Failed to read shader {}", path));
            let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(asset_id),
                source: wgpu::ShaderSource::Wgsl(shader_str.into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(asset_id),
                layout: Some(&effect_pipeline_layout),
                module: &shader_module,
                entry_point: "main",
                cache: None,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            });
            custom_shader_pipelines.insert(asset_id.clone(), pipeline);
            info!("Compiled custom shader pipeline: {}", asset_id);
        }
    }
    let pipeline_dur = pipeline_start.elapsed();

    let mut render_dur = std::time::Duration::from_secs(0);
    let mut save_dur = std::time::Duration::from_secs(0);

    let is_movie = spec.output.ends_with(".mp4");

    let render_context = RenderContext {
        device,
        queue,
        gpu_textures,
        transparent_texture,
        texture_a,
        texture_b,
        compositor_params_buffer,
        engine_params_buffer,
        custom_params_buffer,
        built_in_params_buffer,
        compositor_pipeline,
        compositor_bind_group_layout,
        effects_wgsl_pipeline,
        effects_wgsl_bind_group_layout,
        custom_shader_pipelines,
        effect_bind_group_layout,
        readback_buffer,
        bytes_per_row,
        texture_size,
        workgroups_x: (spec.composition.width + 15) / 16,
        workgroups_y: (spec.composition.height + 15) / 16,
    };

    let render_frame = |time: f32| -> Vec<u8> {
        render_context.render_frame(time, &spec, is_movie)
    };

    if is_movie {
        info!("Rendering video...");
        let fps = spec.composition.fps;
        let num_frames = (spec.composition.duration * fps as f32).round() as u32;

        let mut ffmpeg_args = vec![
            "-y".to_string(),
            "-f".to_string(), "rawvideo".to_string(),
            "-pix_fmt".to_string(), "rgba".to_string(),
            "-s".to_string(), format!("{}x{}", spec.composition.width, spec.composition.height),
            "-r".to_string(), fps.to_string(),
            "-i".to_string(), "-".to_string(),
        ];

        let audio_clips = spec.get_audio_clips();
        if !audio_clips.is_empty() {
            for (path, _, _) in &audio_clips {
                ffmpeg_args.push("-i".to_string());
                ffmpeg_args.push(path.clone());
            }

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
            ffmpeg_args.push("-c:v".to_string());
            ffmpeg_args.push("libx264".to_string());
            ffmpeg_args.push("-c:a".to_string());
            ffmpeg_args.push("aac".to_string());
            ffmpeg_args.push("-shortest".to_string());
        } else {
            ffmpeg_args.push("-c:v".to_string());
            ffmpeg_args.push("libx264".to_string());
        }
        
        ffmpeg_args.push(spec.output.clone());

        let save_start_inst = Instant::now();
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
