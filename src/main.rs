use log::{LevelFilter, error, info};
use render_poc::config::*;
use render_poc::engine::{EngineParams, RenderContext, TransitionEngineParams};
use serde::Serialize;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

// ─── Logging ──────────────────────────────────────────────────────────────────

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

// ─── CLI argument parsing ─────────────────────────────────────────────────────

struct CliArgs {
    spec_path: std::path::PathBuf,
    debug_dir: Option<std::path::PathBuf>,
    include_paths: Vec<std::path::PathBuf>,
}

fn parse_args() -> CliArgs {
    let args: Vec<String> = env::args().collect();
    let mut spec_path_str = None;
    let mut debug_dir_path_str = None;
    let mut include_paths = Vec::new();
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
            "-I" | "--include" => {
                if i + 1 < args.len() {
                    include_paths.push(std::path::PathBuf::from(args[i + 1].clone()));
                    i += 2;
                } else {
                    eprintln!("Error: Missing value for -I/--include option");
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
        eprintln!("Usage: render-poc [-i <spec.json>] [--debug <dir>] [-I <path>]");
        std::process::exit(1);
    });

    CliArgs {
        spec_path: std::path::PathBuf::from(spec_path_str),
        debug_dir: debug_dir_path_str.map(std::path::PathBuf::from),
        include_paths,
    }
}

// ─── Debug directory resolution ───────────────────────────────────────────────

/// Computes a unique run directory name under `debug_dir` based on the spec
/// filename, e.g. `spec_json_render_0001/`.
fn resolve_debug_run_dir(
    debug_dir: &std::path::Path,
    spec_path: &std::path::Path,
) -> std::path::PathBuf {
    let spec_file_name = spec_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("spec")
        .replace('.', "_");

    let mut counter = 1;
    loop {
        let folder_name = format!("{}_render_{:04}", spec_file_name, counter);
        let path = debug_dir.join(folder_name);
        if !path.exists() {
            return path;
        }
        counter += 1;
    }
}

/// Creates the debug directory structure and wires up the file logger.
fn setup_logging(run_dir: &std::path::Path) {
    std::fs::create_dir_all(run_dir.join("frames")).expect("Failed to create debug frames dir");
    let log_file = File::create(run_dir.join("logs.txt")).expect("Failed to create logs.txt");
    if let Ok(mut file_guard) = LOGGER.file.lock() {
        *file_guard = Some(log_file);
    }
}

// ─── GPU initialisation ──────────────────────────────────────────────────────

/// Creates the wgpu instance, selects a high-performance adapter, and opens
/// the device + queue. Returns `(device, queue, adapter_name)`.
fn init_gpu() -> (wgpu::Device, wgpu::Queue, String) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("Failed to find an appropriate adapter");

    let adapter_name = adapter.get_info().name.clone();

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .expect("Failed to create device");

    (device, queue, adapter_name)
}

// ─── Asset loading ────────────────────────────────────────────────────────────

/// Loads all image/video assets from the spec, resizes them to the composition
/// size, and returns them as an RGBA hashmap keyed by asset ID.
fn load_asset_images(spec: &RenderSpec) -> HashMap<String, image::RgbaImage> {
    let mut cpu_images: HashMap<String, image::RgbaImage> = HashMap::new();
    for (asset_id, asset) in &spec.assets {
        match asset {
            Asset::Image { path } | Asset::Video { path } => {
                let img =
                    image::open(path).unwrap_or_else(|_| panic!("Failed to load asset: {}", path));
                let img = img.to_rgba8();
                info!("Loaded asset '{}' from {} ({}x{})", asset_id, path, img.width(), img.height());
                cpu_images.insert(asset_id.clone(), img);
            }
            _ => {}
        }
    }
    cpu_images
}

// ─── FFmpeg argument building ─────────────────────────────────────────────────

/// Builds the complete `ffmpeg` argument list for video encoding, including
/// optional audio mixing via `-filter_complex`.
fn build_ffmpeg_args(
    spec: &RenderSpec,
    audio_clips: &[(String, f32, f32)],
) -> Vec<String> {
    let mut ffmpeg_args = vec![
        "-y".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "-pix_fmt".to_string(),
        "rgba".to_string(),
        "-s".to_string(),
        format!("{}x{}", spec.composition.width, spec.composition.height),
        "-r".to_string(),
        spec.composition.fps.to_string(),
        "-i".to_string(),
        "-".to_string(),
    ];

    if !audio_clips.is_empty() {
        for (path, _, _) in audio_clips {
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
    ffmpeg_args
}

// ─── Debug review output ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct ReviewFrame {
    frame: u32,
    timestamp: f32,
    file: String,
    explanation: String,
}

/// Writes the `review.json` summary of spot-checked frames and tears down the
/// file logger.
fn write_debug_review(
    run_dir: &std::path::Path,
    spot_check_frames: &HashMap<u32, Vec<(f32, String)>>,
) {
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

// ─── Performance reporting ────────────────────────────────────────────────────

struct PerformanceTimings {
    total: std::time::Duration,
    spec_load: std::time::Duration,
    img_load: std::time::Duration,
    gpu_init: std::time::Duration,
    resources: std::time::Duration,
    pipeline: std::time::Duration,
    render: std::time::Duration,
    save: std::time::Duration,
    mem_start: Option<usize>,
    mem_after_gpu: Option<usize>,
    mem_after_render: Option<usize>,
}

fn print_performance_profile(t: &PerformanceTimings) {
    info!("================ Performance Profile ================");
    info!("Total Duration:       {:?}", t.total);
    info!("- Config parsing:      {:?}", t.spec_load);
    info!("- Input asset load:   {:?}", t.img_load);
    info!("- GPU Init:           {:?}", t.gpu_init);
    info!("- Resources creation: {:?}", t.resources);
    info!("- Shader compilation: {:?}", t.pipeline);
    info!("- Rendering loop:     {:?}", t.render);
    info!("- Output saving/enc:  {:?}", t.save);
    if let Some(rss) = t.mem_start {
        info!("Memory Profile:");
        info!("- Startup RSS:        {:.2} MB", rss as f64 / 1024.0 / 1024.0);
    }
    if let Some(rss) = t.mem_after_gpu {
        info!("- Post-GPU Init RSS:  {:.2} MB", rss as f64 / 1024.0 / 1024.0);
    }
    if let Some(rss) = t.mem_after_render {
        info!("- Post-Render RSS:    {:.2} MB", rss as f64 / 1024.0 / 1024.0);
    }
    info!("=====================================================");
}

// ─── Platform helpers ─────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn get_resident_set_size() -> Option<usize> {
    None // Removed libc call to satisfy compiler/cleanliness since not essential
}

#[cfg(not(target_os = "macos"))]
fn get_resident_set_size() -> Option<usize> {
    None
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    let start_time = Instant::now();
    let mem_start = get_resident_set_size();

    // ── CLI & debug setup ────────────────────────────────────────────────
    let args = parse_args();
    let run_dir_path = args
        .debug_dir
        .as_ref()
        .map(|d| resolve_debug_run_dir(d, &args.spec_path));
    if let Some(ref run_dir) = run_dir_path {
        setup_logging(run_dir);
    }
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(LevelFilter::Info));

    // ── Spec loading ─────────────────────────────────────────────────────
    let spec_start = Instant::now();
    let spec_file = File::open(&args.spec_path).expect("Failed to open spec file");
    let reader = BufReader::new(spec_file);
    let spec: RenderSpec = serde_json::from_reader(reader).expect("Failed to parse JSON");
    let spec_load_dur = spec_start.elapsed();

    info!("Initializing headless video rendering pipeline...");
    info!("Composition size: {}x{}", spec.composition.width, spec.composition.height);

    // ── Spot-check frame selection (debug mode) ──────────────────────────
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

    // ── Asset loading ────────────────────────────────────────────────────
    let img_load_start = Instant::now();
    let cpu_images = load_asset_images(&spec);
    let mut font_assets = HashMap::new();
    for (asset_id, asset) in &spec.assets {
        if let Asset::Font { path, .. } = asset {
            let bytes = std::fs::read(path).unwrap_or_else(|e| {
                error!("Failed to read font file '{}': {:?}", path, e);
                Vec::new()
            });
            info!("Loaded font asset '{}' from {}", asset_id, path);
            font_assets.insert(asset_id.clone(), bytes);
        }
    }
    let img_load_dur = img_load_start.elapsed();

    // ── GPU initialisation ───────────────────────────────────────────────
    let gpu_init_start = Instant::now();
    let (device, queue, gpu_name) = init_gpu();
    let gpu_init_dur = gpu_init_start.elapsed();
    info!("Using GPU: {:?}", gpu_name);

    let mem_after_gpu = get_resident_set_size();

    // ── Resource creation ────────────────────────────────────────────────
    let resources_start = Instant::now();

    // -- Textures --
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
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        label: Some("Ping/Pong Texture"),
        view_formats: &[],
    };

    let texture_a = device.create_texture(&texture_desc);
    let texture_b = device.create_texture(&texture_desc);
    let texture_c = device.create_texture(&texture_desc);
    let texture_d = device.create_texture(&texture_desc);
    let feedback_texture_a = device.create_texture(&texture_desc);
    let feedback_texture_b = device.create_texture(&texture_desc);
    let text_scratch_texture_desc = wgpu::TextureDescriptor {
        label: Some("Text Scratch Texture"),
        ..texture_desc
    };
    let text_scratch_texture = device.create_texture(&text_scratch_texture_desc);

    // Clear feedback textures to transparent black
    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Clear Feedback Textures"),
        });
        for tex in &[&feedback_texture_a, &feedback_texture_b] {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Clear Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tex.create_view(&wgpu::TextureViewDescriptor::default()),
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
    }

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

    // -- GPU asset textures --
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

    // -- Buffers --
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

    let transition_engine_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Transition Engine Params Buffer"),
        size: std::mem::size_of::<TransitionEngineParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let transition_custom_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Transition Custom Params Buffer"),
        size: 1024,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let resources_dur = resources_start.elapsed();

    // ── Pipeline compilation ─────────────────────────────────────────────
    let pipeline_start = Instant::now();
    let compositor_shader_str = std::fs::read_to_string("src/compositor.wgsl").expect("Failed to read compositor.wgsl");
    let compositor_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Compositor Shader"),
        source: wgpu::ShaderSource::Wgsl(compositor_shader_str.into()),
    });

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
        label: Some("Compositor Pipeline"),
        layout: Some(&compositor_pipeline_layout),
        module: &compositor_shader,
        entry_point: "main",
        cache: None,
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });


    let effect_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Effect Bind Group Layout"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
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

    let transition_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Transition Bind Group Layout"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 4,
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

    let transition_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Transition Pipeline Layout"),
        bind_group_layouts: &[&transition_bind_group_layout],
        push_constant_ranges: &[],
    });

    // Scan default directories "library/effects/" and "library/transitions/" and --include paths
    let mut wgsl_files = Vec::new();
    
    // Scan default directory "library/effects/"
    let effects_dir = std::path::Path::new("library/effects");
    if effects_dir.exists() && effects_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(effects_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().map_or(false, |ext| ext == "wgsl") {
                    wgsl_files.push(path);
                }
            }
        }
    }

    // Scan default directory "library/transitions/"
    let transitions_dir = std::path::Path::new("library/transitions");
    if transitions_dir.exists() && transitions_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(transitions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().map_or(false, |ext| ext == "wgsl") {
                    wgsl_files.push(path);
                }
            }
        }
    }
    
    // Scan custom include paths from command line
    for path in &args.include_paths {
        if path.is_file() {
            wgsl_files.push(path.clone());
        } else if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() && p.extension().map_or(false, |ext| ext == "wgsl") {
                        wgsl_files.push(p);
                    }
                }
            }
        }
    }

    let mut transition_shaders = std::collections::HashSet::new();
    for track in &spec.tracks {
        for tr in &track.transitions {
            if let Some(ref sh) = tr.shader {
                transition_shaders.insert(sh.clone());
            } else {
                transition_shaders.insert(tr.transition_type.clone());
            }
        }
    }

    let mut custom_shader_pipelines = HashMap::new();

    for file_path in wgsl_files {
        let file_stem = file_path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
            
        let shader_str = match std::fs::read_to_string(&file_path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to read shader file {:?}: {}", file_path, e);
                continue;
            }
        };
        
        let is_transition = transition_shaders.contains(&file_stem)
            || shader_str.contains("TransitionEngineParams")
            || shader_str.contains("tex_to");
        
        if let Some(metadata_str) = extract_transition_metadata(&shader_str) {
            match serde_json::from_str::<TransitionMetadata>(&metadata_str) {
                Ok(meta) => {
                    info!("Loaded transition metadata: {} from {:?}", meta.transition_type, file_path);
                    let transition_type = meta.transition_type.clone();
                    register_transition_metadata(meta);
                    
                    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some(&transition_type),
                        source: wgpu::ShaderSource::Wgsl(shader_str.into()),
                    });
                    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(&transition_type),
                        layout: Some(&transition_pipeline_layout),
                        module: &shader_module,
                        entry_point: "main",
                        cache: None,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    });
                    custom_shader_pipelines.insert(transition_type.clone(), pipeline);
                }
                Err(e) => {
                    log::error!("Failed to parse transition metadata in {:?}: {}", file_path, e);
                }
            }
        } else if let Some(metadata_str) = extract_metadata(&shader_str) {
            match serde_json::from_str::<EffectMetadata>(&metadata_str) {
                Ok(meta) => {
                    info!("Loaded effect metadata: {} from {:?}", meta.effect_type, file_path);
                    let effect_type = meta.effect_type.clone();
                    register_effect_metadata(meta);
                    
                    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some(&effect_type),
                        source: wgpu::ShaderSource::Wgsl(shader_str.into()),
                    });
                    let is_tr = transition_shaders.contains(&effect_type) || is_transition;
                    let layout = if is_tr { &transition_pipeline_layout } else { &effect_pipeline_layout };
                    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(&effect_type),
                        layout: Some(layout),
                        module: &shader_module,
                        entry_point: "main",
                        cache: None,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    });
                    custom_shader_pipelines.insert(effect_type.clone(), pipeline);
                }
                Err(e) => {
                    log::error!("Failed to parse metadata in {:?}: {}", file_path, e);
                }
            }
        } else {
            info!("Compiling shader without metadata: {} from {:?}", file_stem, file_path);
            let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&file_stem),
                source: wgpu::ShaderSource::Wgsl(shader_str.into()),
            });
            let layout = if is_transition { &transition_pipeline_layout } else { &effect_pipeline_layout };
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(&file_stem),
                layout: Some(layout),
                module: &shader_module,
                entry_point: "main",
                cache: None,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            });
            custom_shader_pipelines.insert(file_stem.clone(), pipeline);
        }
    }

    // Compile spec-declared shaders
    for (asset_id, asset) in &spec.assets {
        if let Asset::Shader { path } = asset {
            if !custom_shader_pipelines.contains_key(asset_id) {
                let shader_str = std::fs::read_to_string(path).unwrap_or_else(|_| panic!("Failed to read shader {}", path));
                
                if let Some(metadata_str) = extract_transition_metadata(&shader_str) {
                    if let Ok(meta) = serde_json::from_str::<TransitionMetadata>(&metadata_str) {
                        register_transition_metadata(meta);
                    }
                } else if let Some(metadata_str) = extract_metadata(&shader_str) {
                    if let Ok(meta) = serde_json::from_str::<EffectMetadata>(&metadata_str) {
                        register_effect_metadata(meta);
                    }
                }
                
                let is_tr = transition_shaders.contains(asset_id)
                    || shader_str.contains("TransitionEngineParams")
                    || shader_str.contains("tex_to");
                let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(asset_id),
                    source: wgpu::ShaderSource::Wgsl(shader_str.into()),
                });
                let layout = if is_tr { &transition_pipeline_layout } else { &effect_pipeline_layout };
                let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(asset_id),
                    layout: Some(layout),
                    module: &shader_module,
                    entry_point: "main",
                    cache: None,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                });
                custom_shader_pipelines.insert(asset_id.clone(), pipeline);
                info!("Compiled spec-declared {} shader pipeline: {}", if is_tr { "transition" } else { "effect" }, asset_id);
            }
        }
    }
    let pipeline_dur = pipeline_start.elapsed();

    // ── Render loop ──────────────────────────────────────────────────────
    let mut render_dur = std::time::Duration::from_secs(0);
    let mut save_dur = std::time::Duration::from_secs(0);

    let is_movie = spec.output.ends_with(".mp4");

    let render_context = RenderContext {
        device,
        queue,
        gpu_textures,
        font_assets,
        transparent_texture,
        text_scratch_texture,
        texture_a,
        texture_b,
        texture_c,
        texture_d,
        feedback_texture_a,
        feedback_texture_b,
        compositor_params_buffer,
        engine_params_buffer,
        custom_params_buffer,
        transition_engine_params_buffer,
        transition_custom_params_buffer,
        compositor_pipeline,
        compositor_bind_group_layout,
        custom_shader_pipelines,
        effect_bind_group_layout,
        transition_bind_group_layout,
        readback_buffer,
        bytes_per_row,
        texture_size,
        workgroups_x: (spec.composition.width + 15) / 16,
        workgroups_y: (spec.composition.height + 15) / 16,
    };

    let render_frame = |time: f32| -> Vec<u8> {
        render_context.render_frame(time, &spec)
    };

    if is_movie {
        info!("Rendering video...");
        let fps = spec.composition.fps;
        let num_frames = (spec.composition.duration * fps as f32).round() as u32;

        let audio_clips = spec.get_audio_clips();
        let ffmpeg_args = build_ffmpeg_args(&spec, &audio_clips);

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

    // ── Debug review & performance report ────────────────────────────────
    if let Some(ref run_dir) = run_dir_path {
        write_debug_review(run_dir, &spot_check_frames);
    }

    let mem_after_render = get_resident_set_size();

    print_performance_profile(&PerformanceTimings {
        total: start_time.elapsed(),
        spec_load: spec_load_dur,
        img_load: img_load_dur,
        gpu_init: gpu_init_dur,
        resources: resources_dur,
        pipeline: pipeline_dur,
        render: render_dur,
        save: save_dur,
        mem_start,
        mem_after_gpu,
        mem_after_render,
    });
}
