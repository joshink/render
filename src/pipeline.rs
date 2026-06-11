//! End-to-end render pipeline, callable as a library.
//!
//! This module owns everything between "a parsed spec" and "an output that
//! exists at its destination": remote-asset fetching, GPU initialisation,
//! resource and shader-pipeline creation, the frame loop (single image or
//! ffmpeg-encoded video), and the final upload for remote destinations.
//!
//! It is used by both the one-shot CLI path and the `serve` HTTP mode, so it
//! reports failures as `Result` values rather than exiting the process, and
//! emits [`Progress`] events through a caller-supplied callback (the server
//! forwards these to SSE subscribers).

use crate::config::*;
use crate::download::fetch_remote_url;
use crate::engine::{EngineParams, RenderContext, Timeline, TransitionEngineParams};
use crate::upload::{upload_gcs, upload_mux, upload_s3, upload_signed_url};
use log::{error, info};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

// ─── Public API ───────────────────────────────────────────────────────────────

/// Everything the pipeline needs besides the spec itself. A trimmed-down,
/// CLI-agnostic version of the binary's argument set.
#[derive(Debug, Default, Clone)]
pub struct RenderOptions {
    /// Extra directories to scan for WGSL effect/transition shaders.
    pub include_paths: Vec<std::path::PathBuf>,
    pub aws_key: Option<String>,
    pub aws_secret: Option<String>,
    pub gcs_key: Option<String>,
    pub gcs_secret: Option<String>,
    pub mux_token_id: Option<String>,
    pub mux_token_secret: Option<String>,
    /// When set, debug spot-check frames and review.json are written here.
    /// The directory (and a `frames/` subdir) must already exist.
    pub debug_run_dir: Option<std::path::PathBuf>,
}

/// Coarse pipeline stage notifications, emitted in order. `Rendering` fires
/// once per frame for video outputs, so consumers that persist or transmit
/// events should be comfortable with high frequency (SSE consumers get them
/// coalesced through a `watch` channel).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum Progress {
    FetchingAssets,
    LoadingAssets,
    InitializingGpu,
    CompilingShaders,
    Rendering { frame: u32, total_frames: u32 },
    Uploading,
}

/// Successful pipeline result.
pub struct RenderOutcome {
    /// Where the output ended up: the local path, the remote URI, or — for
    /// `mux://` destinations — the Mux asset id (`upload:<id>` if the asset
    /// id was not yet assigned).
    pub output: String,
    pub timings: PerformanceTimings,
}

pub struct PerformanceTimings {
    /// End-to-end duration of `pipeline::render`. The CLI overwrites this
    /// with the full process duration before printing.
    pub total: std::time::Duration,
    /// Spec parse time — only known to callers that load the spec themselves
    /// (the CLI); `pipeline::render` receives an already-parsed spec, so it
    /// leaves this `None` and the profile omits the line.
    pub spec_load: Option<std::time::Duration>,
    pub img_load: std::time::Duration,
    pub gpu_init: std::time::Duration,
    pub resources: std::time::Duration,
    pub pipeline: std::time::Duration,
    pub render: std::time::Duration,
    pub save: std::time::Duration,
}

pub fn print_performance_profile(t: &PerformanceTimings) {
    info!("================ Performance Profile ================");
    info!("Total Duration:       {:?}", t.total);
    if let Some(spec_load) = t.spec_load {
        info!("- Config parsing:      {:?}", spec_load);
    }
    info!("- Input asset load:   {:?}", t.img_load);
    info!("- GPU Init:           {:?}", t.gpu_init);
    info!("- Resources creation: {:?}", t.resources);
    info!("- Shader compilation: {:?}", t.pipeline);
    info!("- Rendering loop:     {:?}", t.render);
    info!("- Output saving/enc:  {:?}", t.save);
    info!("=====================================================");
}

// ─── Spec loading helpers ─────────────────────────────────────────────────────

/// Parses raw spec source into spec JSON, transpiling KDL when `is_kdl`.
/// Shared by [`load_spec_file`] (which dispatches on file extension) and the
/// HTTP server (which dispatches on `Content-Type`) so the two entry points
/// can never drift apart in what they accept.
pub fn spec_json_from_str(src: &str, is_kdl: bool) -> Result<serde_json::Value, String> {
    if is_kdl {
        crate::kdl_spec::kdl_to_spec_json(src).map_err(|e| format!("Invalid KDL spec:\n{}", e))
    } else {
        serde_json::from_str(src).map_err(|e| format!("Invalid JSON spec: {}", e))
    }
}

/// Reads a spec file from disk, transpiling `.kdl` sources to spec JSON.
pub fn load_spec_file(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let is_kdl = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("kdl"))
        .unwrap_or(false);
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read spec file {:?}: {}", path, e))?;
    spec_json_from_str(&src, is_kdl).map_err(|e| format!("Error in spec {:?}: {}", path, e))
}

/// Applies a dotted-path override (e.g. `composition.width = 1920`) onto the
/// raw spec JSON, creating intermediate objects as needed. The value string is
/// coerced to number/bool/null where it parses as one.
pub fn apply_override(json: &mut serde_json::Value, path: &str, value_str: &str) -> Result<(), String> {
    let json_val = if let Ok(n) = value_str.parse::<i64>() {
        serde_json::Value::Number(n.into())
    } else if let Ok(f) = value_str.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(f) {
            serde_json::Value::Number(num)
        } else {
            serde_json::Value::String(value_str.to_string())
        }
    } else if value_str == "true" {
        serde_json::Value::Bool(true)
    } else if value_str == "false" {
        serde_json::Value::Bool(false)
    } else if value_str == "null" {
        serde_json::Value::Null
    } else {
        let s = if (value_str.starts_with('"') && value_str.ends_with('"')) ||
                   (value_str.starts_with('\'') && value_str.ends_with('\'')) {
            &value_str[1..value_str.len() - 1]
        } else {
            value_str
        };
        serde_json::Value::String(s.to_string())
    };

    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return Err("Empty path".to_string());
    }

    let mut current = json;
    for (i, &part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            if !current.is_object() {
                *current = serde_json::Value::Object(serde_json::Map::new());
            }
            if let Some(map) = current.as_object_mut() {
                map.insert(part.to_string(), json_val);
                return Ok(());
            } else {
                return Err("Failed to convert value to object".to_string());
            }
        } else {
            if !current.is_object() {
                *current = serde_json::Value::Object(serde_json::Map::new());
            }
            let map = current.as_object_mut().unwrap();
            if !map.contains_key(part) || !map.get(part).unwrap().is_object() {
                map.insert(part.to_string(), serde_json::Value::Object(serde_json::Map::new()));
            }
            current = map.get_mut(part).unwrap();
        }
    }
    Ok(())
}

/// Sorts keyframes and deserializes the raw spec JSON into a [`RenderSpec`].
pub fn finalize_spec(mut spec_value: serde_json::Value) -> Result<RenderSpec, String> {
    sort_value_keyframes(&mut spec_value);
    serde_json::from_value(spec_value).map_err(|e| format!("Invalid render spec: {}", e))
}

// ─── Path resolution ──────────────────────────────────────────────────────────

/// Resolves the absolute or relative path to the `library/` folder.
/// Checks RENDER_LIBRARY_PATH environment variable first, then fallback directories.
pub fn get_library_path() -> std::path::PathBuf {
    // 1. Check environment variable
    if let Ok(path_str) = std::env::var("RENDER_LIBRARY_PATH") {
        let env_path = std::path::PathBuf::from(path_str);
        if env_path.exists() && env_path.is_dir() {
            return env_path;
        }
    }

    // 2. Check current working directory ./library
    let cwd_lib = std::path::Path::new("library");
    if cwd_lib.exists() && cwd_lib.is_dir() && cwd_lib.join("compositor.wgsl").exists() {
        return cwd_lib.to_path_buf();
    }

    // 3. Check relative to executable path (walking up parent directories)
    if let Ok(exe_path) = std::env::current_exe() {
        let mut dir = exe_path.parent();
        while let Some(parent) = dir {
            let lib_path = parent.join("library");
            if lib_path.exists() && lib_path.is_dir() && lib_path.join("compositor.wgsl").exists() {
                return lib_path;
            }
            dir = parent.parent();
        }
    }

    // Fallback to relative path in current directory
    std::path::PathBuf::from("library")
}

/// Resolves a resource path relative to the library directory if it does not exist at CWD.
pub fn resolve_asset_path(path: &str) -> std::path::PathBuf {
    let raw_path = std::path::PathBuf::from(path);
    if raw_path.exists() {
        return raw_path;
    }

    let lib_path = get_library_path();

    // Try relative to library path
    let try_lib = lib_path.join(path);
    if try_lib.exists() {
        return try_lib;
    }

    // If path starts with "library/", strip it and join with lib_path
    if path.starts_with("library/") {
        let stripped = &path["library/".len()..];
        let try_stripped = lib_path.join(stripped);
        if try_stripped.exists() {
            return try_stripped;
        }
    }

    // If path starts with "fonts/", strip it and join with lib_path/fonts/
    if path.starts_with("fonts/") {
        let stripped = &path["fonts/".len()..];
        let try_font = lib_path.join("fonts").join(stripped);
        if try_font.exists() {
            return try_font;
        }
    }

    // Default to the original path
    raw_path
}

// ─── Remote asset fetching ────────────────────────────────────────────────────

/// Downloads any `http(s)://` asset paths in the spec to local cache files,
/// rewriting the spec paths in place.
pub fn fetch_remote_assets(spec: &mut RenderSpec) -> Result<(), String> {
    for (asset_id, asset) in &mut spec.assets {
        let (kind, path) = match asset {
            Asset::Video { path } => ("Video", path),
            Asset::Image { path } => ("Image", path),
            Asset::Audio { path } => ("Audio", path),
            Asset::Shader { path } => ("Shader", path),
            Asset::Lut { path } => ("LUT", path),
            Asset::Font { path, .. } => ("Font", path),
        };
        if path.starts_with("http://") || path.starts_with("https://") {
            info!("Fetching remote {} asset '{}' from URL: {}", kind, asset_id, path);
            *path = fetch_remote_url(path).map_err(|e| {
                format!("Failed to fetch remote {} asset '{}': {}", kind, asset_id, e)
            })?;
        }
    }
    Ok(())
}

// ─── Asset loading ────────────────────────────────────────────────────────────

/// Loads all image/video assets from the spec, resizes them to the composition
/// size, and returns them as an RGBA hashmap keyed by asset ID.
fn load_asset_images(spec: &RenderSpec) -> Result<HashMap<String, image::RgbaImage>, String> {
    let mut cpu_images: HashMap<String, image::RgbaImage> = HashMap::new();
    for (asset_id, asset) in &spec.assets {
        match asset {
            Asset::Image { path } | Asset::Video { path } => {
                let img = image::open(path)
                    .map_err(|e| format!("Failed to load asset '{}' from {}: {}", asset_id, path, e))?;
                let img = img.to_rgba8();
                info!("Loaded asset '{}' from {} ({}x{})", asset_id, path, img.width(), img.height());
                cpu_images.insert(asset_id.clone(), img);
            }
            Asset::Lut { path } => {
                let resolved = resolve_asset_path(path);
                let atlas = crate::lut::load_lut_atlas(&resolved);
                info!(
                    "Loaded LUT asset '{}' from {:?} (atlas {}x{})",
                    asset_id, resolved, atlas.width(), atlas.height()
                );
                cpu_images.insert(asset_id.clone(), atlas);
            }
            _ => {}
        }
    }
    Ok(cpu_images)
}

fn load_font_assets(spec: &RenderSpec) -> HashMap<String, Vec<u8>> {
    let mut font_assets = HashMap::new();
    for (asset_id, asset) in &spec.assets {
        if let Asset::Font { path, .. } = asset {
            let resolved_path = resolve_asset_path(path);
            let bytes = std::fs::read(&resolved_path).unwrap_or_else(|e| {
                error!("Failed to read font file '{:?}': {:?}", resolved_path, e);
                Vec::new()
            });
            info!("Loaded font asset '{}' from {:?}", asset_id, resolved_path);
            font_assets.insert(asset_id.clone(), bytes);
        }
    }
    font_assets
}

// ─── GPU initialisation ──────────────────────────────────────────────────────

/// Creates the wgpu instance, selects a high-performance adapter, and opens
/// the device + queue. Returns `(device, queue, adapter_name)`.
/// One thread's cached GPU handle, shared across the renders that thread runs.
type GpuHandle = (std::sync::Arc<wgpu::Device>, std::sync::Arc<wgpu::Queue>, String);

thread_local! {
    /// Per-thread GPU device cache. Adapter discovery and device creation cost
    /// seconds and churn driver handles, so in serve mode each worker thread
    /// initialises once and reuses the device for every job it processes. The
    /// cache is per-thread rather than per-process because a `wgpu::Device`
    /// poisoned by one worker's device loss must not take down its siblings.
    ///
    /// `ManuallyDrop` keeps the TLS destructor from dropping the device at
    /// thread exit: wgpu's teardown touches other thread-locals that may
    /// already be destroyed by then, which panics and aborts the process.
    /// The handle is intentionally leaked at thread exit; explicit cleanup
    /// goes through [`invalidate_gpu_cache`].
    static GPU_CACHE: std::cell::RefCell<Option<std::mem::ManuallyDrop<GpuHandle>>> =
        const { std::cell::RefCell::new(None) };
}

/// Drops this thread's cached GPU device so the next render re-initialises
/// from scratch. Call after a failed or panicked render: device loss is the
/// most common cause, and a lost device fails every subsequent submission.
pub fn invalidate_gpu_cache() {
    GPU_CACHE.with(|cache| {
        if let Some(handle) = cache.borrow_mut().take() {
            drop(std::mem::ManuallyDrop::into_inner(handle));
        }
    });
}

/// Returns this thread's cached GPU device, initialising it on first use.
fn acquire_gpu() -> Result<GpuHandle, String> {
    GPU_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(handle) = cache.as_ref() {
            return Ok(GpuHandle::clone(handle));
        }
        let handle = init_gpu()?;
        *cache = Some(std::mem::ManuallyDrop::new(handle.clone()));
        Ok(handle)
    })
}

fn init_gpu() -> Result<GpuHandle, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok_or_else(|| "Failed to find an appropriate GPU adapter".to_string())?;

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
    .map_err(|e| format!("Failed to create GPU device: {}", e))?;

    Ok((std::sync::Arc::new(device), std::sync::Arc::new(queue), adapter_name))
}

// ─── GPU resources ────────────────────────────────────────────────────────────

const CUSTOM_PARAMS_BUFFER_SIZE: u64 = 1024;

struct GpuResources {
    texture_a: wgpu::Texture,
    texture_b: wgpu::Texture,
    texture_c: wgpu::Texture,
    texture_d: wgpu::Texture,
    feedback_texture_a: wgpu::Texture,
    feedback_texture_b: wgpu::Texture,
    text_scratch_texture: wgpu::Texture,
    transparent_texture: wgpu::Texture,
    compositor_params_buffer: wgpu::Buffer,
    engine_params_buffer: wgpu::Buffer,
    custom_params_buffer: wgpu::Buffer,
    transition_engine_params_buffer: wgpu::Buffer,
    transition_custom_params_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    bytes_per_row: u32,
    texture_size: wgpu::Extent3d,
}

fn create_gpu_resources(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    spec: &RenderSpec,
) -> GpuResources {
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
        size: CUSTOM_PARAMS_BUFFER_SIZE,
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
        size: CUSTOM_PARAMS_BUFFER_SIZE,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    GpuResources {
        texture_a,
        texture_b,
        texture_c,
        texture_d,
        feedback_texture_a,
        feedback_texture_b,
        transparent_texture,
        text_scratch_texture,
        compositor_params_buffer,
        engine_params_buffer,
        custom_params_buffer,
        transition_engine_params_buffer,
        transition_custom_params_buffer,
        readback_buffer,
        bytes_per_row,
        texture_size,
    }
}

// ─── Shader pipeline compilation ──────────────────────────────────────────────

struct PipelineSet {
    compositor_pipeline: wgpu::ComputePipeline,
    compositor_bind_group_layout: wgpu::BindGroupLayout,
    effect_bind_group_layout: wgpu::BindGroupLayout,
    transition_bind_group_layout: wgpu::BindGroupLayout,
    custom_shader_pipelines: HashMap<String, wgpu::ComputePipeline>,
    /// Metadata extracted from the shaders compiled above, scoped to this
    /// render so concurrent jobs can't observe each other's registrations.
    registry: ShaderRegistry,
}

fn collect_wgsl_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if dir.exists() {
        if dir.is_file() {
            if dir.extension().map_or(false, |ext| ext == "wgsl") {
                out.push(dir.to_path_buf());
            }
        } else if dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().map_or(false, |ext| ext == "wgsl") {
                        out.push(path);
                    }
                }
            }
        }
    }
}

fn compile_pipelines(
    device: &wgpu::Device,
    spec: &RenderSpec,
    include_paths: &[std::path::PathBuf],
) -> Result<PipelineSet, String> {
    let mut registry = ShaderRegistry::default();
    let lib_path = get_library_path();
    let compositor_path = lib_path.join("compositor.wgsl");
    let compositor_shader_str = std::fs::read_to_string(&compositor_path)
        .map_err(|e| format!("Failed to read compositor.wgsl from {:?}: {}", compositor_path, e))?;
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
            // Binding 7: LUT atlas texture (color look-up tables). Sampled with
            // manual trilinear interpolation via textureLoad, so no sampler is
            // needed. Bound to the transparent fallback when the effect names no LUT.
            wgpu::BindGroupLayoutEntry {
                binding: 7,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
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
    collect_wgsl_files(&lib_path.join("effects"), &mut wgsl_files);
    collect_wgsl_files(&lib_path.join("transitions"), &mut wgsl_files);
    for path in include_paths {
        collect_wgsl_files(path, &mut wgsl_files);
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

        // Robust transition detection checks containing directory first, then falls back to metadata sniffing
        let is_in_transitions_dir = file_path.components().any(|c| c.as_os_str() == "transitions");
        let is_transition = is_in_transitions_dir
            || transition_shaders.contains(&file_stem)
            || shader_str.contains("TransitionEngineParams")
            || shader_str.contains("tex_to");

        if let Some(metadata_str) = extract_transition_metadata(&shader_str) {
            match serde_json::from_str::<TransitionMetadata>(&metadata_str) {
                Ok(meta) => {
                    info!("Loaded transition metadata: {} from {:?}", meta.transition_type, file_path);
                    let transition_type = meta.transition_type.clone();
                    registry.register_transition(meta);

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
                    registry.register_effect(meta);

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
                let resolved_path = resolve_asset_path(path);
                let shader_str = std::fs::read_to_string(&resolved_path)
                    .map_err(|e| format!("Failed to read shader {:?}: {}", resolved_path, e))?;

                if let Some(metadata_str) = extract_transition_metadata(&shader_str) {
                    if let Ok(meta) = serde_json::from_str::<TransitionMetadata>(&metadata_str) {
                        registry.register_transition(meta);
                    }
                } else if let Some(metadata_str) = extract_metadata(&shader_str) {
                    if let Ok(meta) = serde_json::from_str::<EffectMetadata>(&metadata_str) {
                        registry.register_effect(meta);
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

    Ok(PipelineSet {
        compositor_pipeline,
        compositor_bind_group_layout,
        effect_bind_group_layout,
        transition_bind_group_layout,
        custom_shader_pipelines,
        registry,
    })
}

// ─── FFmpeg argument building ─────────────────────────────────────────────────

/// Builds the complete `ffmpeg` argument list for video encoding, including
/// optional audio mixing via `-filter_complex`.
fn build_ffmpeg_args(
    spec: &RenderSpec,
    audio_clips: &[(String, f32, f32, f32)],
    output_path: &str,
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
        for (path, _, _, _) in audio_clips {
            ffmpeg_args.push("-i".to_string());
            ffmpeg_args.push(path.clone());
        }

        // Each clip plays [trim_start, trim_start + duration] of its source, then
        // asetpts re-bases the trimmed segment to PTS 0 so adelay positions it at
        // its absolute timeline start regardless of where it was cut from.
        let mut filter_complex = String::new();
        if audio_clips.len() == 1 {
            let (_, start, duration, trim_start) = audio_clips[0];
            let start_ms = (start * 1000.0).round() as u32;
            filter_complex = format!(
                "[1:a]atrim={:.3}:{:.3},asetpts=PTS-STARTPTS,adelay={}|{}[aout]",
                trim_start, trim_start + duration, start_ms, start_ms
            );
        } else {
            for (idx, (_, start, duration, trim_start)) in audio_clips.iter().enumerate() {
                let input_idx = idx + 1; // 0 is video stdin
                let start_ms = (start * 1000.0).round() as u32;
                filter_complex.push_str(&format!(
                    "[{}:a]atrim={:.3}:{:.3},asetpts=PTS-STARTPTS,adelay={}|{}[a{}];",
                    input_idx, trim_start, trim_start + duration, start_ms, start_ms, input_idx
                ));
            }
            for idx in 0..audio_clips.len() {
                filter_complex.push_str(&format!("[a{}]", idx + 1));
            }
            // normalize=0 keeps each input at unity gain. Without it, amix
            // divides every input's volume by the number of inputs, so mixing
            // N clips would silently attenuate each to 1/N of its level.
            filter_complex.push_str(&format!("amix=inputs={}:normalize=0[aout]", audio_clips.len()));
        }

        ffmpeg_args.push("-filter_complex".to_string());
        ffmpeg_args.push(filter_complex);
        ffmpeg_args.push("-map".to_string());
        ffmpeg_args.push("0:v".to_string());
        ffmpeg_args.push("-map".to_string());
        ffmpeg_args.push("[aout]".to_string());
        ffmpeg_args.push("-c:v".to_string());
        ffmpeg_args.push("libx264".to_string());
        // yuv420p with even-dimension padding for the widest player support.
        // Raw rgba input otherwise drives libx264 to yuv444p, which Safari,
        // QuickTime, and most hardware decoders refuse to play.
        ffmpeg_args.push("-pix_fmt".to_string());
        ffmpeg_args.push("yuv420p".to_string());
        ffmpeg_args.push("-c:a".to_string());
        ffmpeg_args.push("aac".to_string());
        ffmpeg_args.push("-shortest".to_string());
    } else {
        ffmpeg_args.push("-c:v".to_string());
        ffmpeg_args.push("libx264".to_string());
        ffmpeg_args.push("-pix_fmt".to_string());
        ffmpeg_args.push("yuv420p".to_string());
    }

    ffmpeg_args.push(output_path.to_string());
    ffmpeg_args
}

/// Flattens straight-alpha RGBA onto black, in place, by premultiplying each
/// channel by its alpha.
///
/// The render pipeline produces straight (un-associated) alpha. ffmpeg discards
/// the alpha channel when encoding to an opaque format like yuv420p, so without
/// this step semi-transparent pixels would keep their full-intensity RGB instead
/// of fading toward black. Premultiplying here makes the dropped-alpha result
/// identical to compositing the frame over a black background.
fn premultiply_alpha_on_black(pixels: &mut [u8]) {
    for chunk in pixels.chunks_exact_mut(4) {
        let a = chunk[3] as u32;
        chunk[0] = ((chunk[0] as u32 * a + 127) / 255) as u8;
        chunk[1] = ((chunk[1] as u32 * a + 127) / 255) as u8;
        chunk[2] = ((chunk[2] as u32 * a + 127) / 255) as u8;
    }
}

// ─── Debug review output ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct ReviewFrame {
    frame: u32,
    timestamp: f32,
    file: String,
    explanation: String,
}

/// Writes the `review.json` summary of spot-checked frames.
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
    match File::create(&review_json_path) {
        Ok(review_file) => {
            if let Err(e) = serde_json::to_writer_pretty(review_file, &review_frames) {
                error!("Failed to write review.json: {}", e);
            } else {
                info!("Written review.json to {:?}", review_json_path);
            }
        }
        Err(e) => error!("Failed to create review.json: {}", e),
    }
}

// ─── Render loop ──────────────────────────────────────────────────────────────

/// RAII guard that removes a temporary file when dropped, ensuring cleanup
/// even on panic.
struct TempFileGuard {
    path: Option<String>,
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(ref path) = self.path {
            if let Err(e) = std::fs::remove_file(path) {
                log::warn!("Failed to remove temporary file {}: {}", path, e);
            }
        }
    }
}

/// RAII guard that reaps a spawned child process on drop (kill + wait).
///
/// `Child::drop` neither kills nor waits, so an early `return Err(...)`
/// between spawn and `wait()` would otherwise leave a zombie ffmpeg behind —
/// fatal over time in the long-lived serve mode, where enough failed jobs
/// exhaust the PID table.
struct ChildGuard {
    child: Option<std::process::Child>,
}

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        ChildGuard { child: Some(child) }
    }

    fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.as_mut().and_then(|c| c.stdin.take())
    }

    /// Waits for the child to exit normally, disarming the guard.
    fn wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child
            .take()
            .expect("ChildGuard::wait called on disarmed guard")
            .wait()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_render_loop(
    render_context: RenderContext,
    spec: &RenderSpec,
    opts: &RenderOptions,
    spot_check_frames: &HashMap<u32, Vec<(f32, String)>>,
    progress: &dyn Fn(Progress),
    render_dur: &mut std::time::Duration,
    save_dur: &mut std::time::Duration,
) -> Result<String, String> {
    let dest_path = spec.output.path();
    let is_remote = spec.output.is_remote();
    let is_movie = spec.output.is_movie();

    let render_output_path = if is_remote {
        let temp_dir = std::env::temp_dir();
        let ext = if is_movie { "mp4" } else { "png" };
        // A uuid rather than pid+timestamp: serve-mode workers share a pid and
        // can dequeue jobs in the same millisecond, which would make two jobs
        // write (and the first one's guard delete) the same temp file.
        let temp_file = temp_dir.join(format!("render_output_{}.{}", uuid::Uuid::new_v4(), ext));
        temp_file.to_string_lossy().to_string()
    } else {
        dest_path.to_string()
    };

    // RAII guard: ensures temp file is cleaned up even if we panic during
    // rendering or upload. Disarmed (set to None) for local outputs.
    let _temp_guard = TempFileGuard {
        path: if is_remote { Some(render_output_path.clone()) } else { None },
    };

    // Derive the timeline (clip start times + resolved transitions) once; it is
    // a pure function of the spec and would otherwise be recomputed for every
    // frame, re-cloning every transition each time.
    let timeline = Timeline::build(spec);
    let render_frame = |time: f32| -> Result<Vec<u8>, String> {
        render_context.render_frame_with_timeline(time, spec, &timeline)
    };

    if is_movie {
        info!("Rendering video...");
        let fps = spec.composition.fps;
        let num_frames = (spec.composition.duration * fps as f32).round() as u32;

        let audio_clips = spec.get_audio_clips();
        let ffmpeg_args = build_ffmpeg_args(spec, &audio_clips, &render_output_path);

        let save_start_inst = Instant::now();
        let ffmpeg_child = Command::new("ffmpeg")
            .args(&ffmpeg_args)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn ffmpeg process: {}", e))?;
        let mut ffmpeg = ChildGuard::new(ffmpeg_child);

        let mut ffmpeg_stdin = ffmpeg
            .take_stdin()
            .ok_or_else(|| "Failed to open stdin for ffmpeg".to_string())?;
        *save_dur += save_start_inst.elapsed();

        for frame in 0..num_frames {
            let time = frame as f32 / fps as f32;
            progress(Progress::Rendering { frame, total_frames: num_frames });

            let frame_render_start = Instant::now();
            let mut unpadded_pixels = render_frame(time)?;
            *render_dur += frame_render_start.elapsed();

            // Debug PNGs are saved with straight alpha (matching the final image
            // path), so write them before flattening for the video stream.
            if let Some(ref run_dir) = opts.debug_run_dir {
                if spot_check_frames.contains_key(&frame) {
                    let frame_save_path = run_dir.join("frames").join(format!("frame_{:04}.png", frame));
                    image::save_buffer(
                        &frame_save_path,
                        &unpadded_pixels,
                        spec.composition.width,
                        spec.composition.height,
                        image::ExtendedColorType::Rgba8,
                    ).map_err(|e| format!("Failed to save debug spot check frame: {}", e))?;
                    info!("Exported debug frame {} to {:?}", frame, frame_save_path);
                }
            }

            // ffmpeg drops alpha for yuv420p; flatten onto black so transparency
            // resolves correctly instead of leaking full-intensity RGB.
            premultiply_alpha_on_black(&mut unpadded_pixels);

            let frame_save_start = Instant::now();
            ffmpeg_stdin
                .write_all(&unpadded_pixels)
                .map_err(|e| format!("Failed to write raw frame to ffmpeg: {}", e))?;
            *save_dur += frame_save_start.elapsed();
        }

        let finish_save_start = Instant::now();
        std::mem::drop(ffmpeg_stdin);
        let status = ffmpeg
            .wait()
            .map_err(|e| format!("Failed to wait for ffmpeg: {}", e))?;
        if !status.success() {
            // Deliberately stricter than the pre-service CLI, which logged the
            // failure but still uploaded the (likely corrupt) file and exited 0.
            return Err(format!("ffmpeg process failed with exit code: {:?}", status.code()));
        }
        *save_dur += finish_save_start.elapsed();
    } else {
        info!("Rendering single image...");
        progress(Progress::Rendering { frame: 0, total_frames: 1 });
        let frame_render_start = Instant::now();
        let unpadded_pixels = render_frame(0.0)?;
        *render_dur += frame_render_start.elapsed();

        if let Some(ref run_dir) = opts.debug_run_dir {
            let frame_save_path = run_dir.join("frames").join("frame_0000.png");
            image::save_buffer(
                &frame_save_path,
                &unpadded_pixels,
                spec.composition.width,
                spec.composition.height,
                image::ExtendedColorType::Rgba8,
            ).map_err(|e| format!("Failed to save debug spot check frame: {}", e))?;
            info!("Exported debug frame 0 to {:?}", frame_save_path);
        }

        let frame_save_start = Instant::now();
        image::save_buffer(
            &render_output_path,
            &unpadded_pixels,
            spec.composition.width,
            spec.composition.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("Failed to save output image: {}", e))?;
        *save_dur += frame_save_start.elapsed();
    }

    if !is_remote {
        return Ok(dest_path.to_string());
    }

    info!("Uploading rendered output from {} to remote destination {}...", render_output_path, dest_path);
    progress(Progress::Uploading);
    let upload_start = Instant::now();

    let spec_creds = spec.output.credentials();
    // Each branch resolves to the output's final handle: the destination URI,
    // except for Mux, whose meaningful handle (the asset id) is only known
    // after upload.
    let upload_result = if dest_path.starts_with("http://") || dest_path.starts_with("https://") {
        upload_signed_url(&render_output_path, dest_path).map(|()| dest_path.to_string())
    } else if dest_path.starts_with("s3://") {
        let (key, secret, region) = resolve_upload_creds(spec_creds, &opts.aws_key, &opts.aws_secret);
        upload_s3(&render_output_path, dest_path, key, secret, region).map(|()| dest_path.to_string())
    } else if dest_path.starts_with("gs://") {
        let (key, secret, region) = resolve_upload_creds(spec_creds, &opts.gcs_key, &opts.gcs_secret);
        upload_gcs(&render_output_path, dest_path, key, secret, region).map(|()| dest_path.to_string())
    } else if spec.output.is_mux() {
        let (token_id, token_secret, _) = resolve_upload_creds(spec_creds, &opts.mux_token_id, &opts.mux_token_secret);
        upload_mux(&render_output_path, token_id, token_secret)
    } else {
        Err(format!("Unsupported remote scheme in output: {}", dest_path))
    };

    // Either way, temp_guard removes the local temp file on drop.
    match upload_result {
        Ok(final_output) => {
            info!("Upload completed successfully in {:?}", upload_start.elapsed());
            Ok(final_output)
        }
        Err(e) => {
            error!("Upload failed: {}", e);
            Err(format!("Upload failed: {}", e))
        }
    }
}

/// Merges per-spec output credentials with the process-level fallbacks: a
/// field present in the spec wins, missing fields fall back to the values
/// passed at startup. The region only ever comes from the spec.
fn resolve_upload_creds(
    spec_creds: Option<&OutputCredentials>,
    fallback_key: &Option<String>,
    fallback_secret: &Option<String>,
) -> (Option<String>, Option<String>, Option<String>) {
    match spec_creds {
        Some(creds) => (
            creds.key.clone().or_else(|| fallback_key.clone()),
            creds.secret.clone().or_else(|| fallback_secret.clone()),
            creds.region.clone(),
        ),
        None => (fallback_key.clone(), fallback_secret.clone(), None),
    }
}

// ─── Pipeline entry point ─────────────────────────────────────────────────────

/// Runs the full pipeline for an already-parsed spec: fetch remote assets,
/// load CPU assets, initialise the GPU, build resources and shader pipelines,
/// render every frame, and (for remote destinations) upload the result.
///
/// `progress` is invoked synchronously on the calling thread as the pipeline
/// advances; pass `&|_| {}` when updates are not needed.
pub fn render(
    mut spec: RenderSpec,
    opts: &RenderOptions,
    progress: &dyn Fn(Progress),
) -> Result<RenderOutcome, String> {
    let start_time = Instant::now();

    progress(Progress::FetchingAssets);
    fetch_remote_assets(&mut spec)?;
    let spec = spec;

    info!("Initializing headless video rendering pipeline...");
    info!("Composition size: {}x{}", spec.composition.width, spec.composition.height);

    // ── Asset loading ────────────────────────────────────────────────────
    progress(Progress::LoadingAssets);
    let img_load_start = Instant::now();
    let cpu_images = load_asset_images(&spec)?;
    let font_assets = load_font_assets(&spec);
    let img_load_dur = img_load_start.elapsed();

    // ── GPU initialisation ───────────────────────────────────────────────
    progress(Progress::InitializingGpu);
    let gpu_init_start = Instant::now();
    let (device, queue, gpu_name) = acquire_gpu()?;
    let gpu_init_dur = gpu_init_start.elapsed();
    info!("Using GPU: {:?}", gpu_name);

    // ── Resource creation ────────────────────────────────────────────────
    let resources_start = Instant::now();
    let gpu_res = create_gpu_resources(&device, &queue, &spec);
    let resources_dur = resources_start.elapsed();

    // ── Pipeline compilation ─────────────────────────────────────────────
    progress(Progress::CompilingShaders);
    let pipeline_start = Instant::now();
    let p_set = compile_pipelines(&device, &spec, &opts.include_paths)?;
    let pipeline_dur = pipeline_start.elapsed();

    // ── Spot-check frame selection (debug mode) ──────────────────────────
    // After pipeline compilation: effect/transition spot checks come from the
    // shader metadata that compile_pipelines just registered.
    let mut spot_check_frames: HashMap<u32, Vec<(f32, String)>> = HashMap::new();
    if opts.debug_run_dir.is_some() {
        let events = spec.get_spot_check_events(&p_set.registry);
        let fps = spec.composition.fps;
        let num_frames = if spec.output.is_movie() {
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

    // ── GPU asset textures binding ──
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
    // The textures now live on the GPU; free the decoded CPU-side copies
    // (potentially hundreds of MB) instead of holding them through the loop.
    drop(cpu_images);

    // ── Render loop ──────────────────────────────────────────────────────
    let mut render_dur = std::time::Duration::from_secs(0);
    let mut save_dur = std::time::Duration::from_secs(0);

    let render_context = RenderContext {
        device,
        queue,
        registry: p_set.registry,
        gpu_textures,
        font_assets,
        transparent_texture: gpu_res.transparent_texture,
        text_scratch_texture: gpu_res.text_scratch_texture,
        texture_a: gpu_res.texture_a,
        texture_b: gpu_res.texture_b,
        texture_c: gpu_res.texture_c,
        texture_d: gpu_res.texture_d,
        feedback_texture_a: gpu_res.feedback_texture_a,
        feedback_texture_b: gpu_res.feedback_texture_b,
        compositor_params_buffer: gpu_res.compositor_params_buffer,
        engine_params_buffer: gpu_res.engine_params_buffer,
        custom_params_buffer: gpu_res.custom_params_buffer,
        transition_engine_params_buffer: gpu_res.transition_engine_params_buffer,
        transition_custom_params_buffer: gpu_res.transition_custom_params_buffer,
        compositor_pipeline: p_set.compositor_pipeline,
        compositor_bind_group_layout: p_set.compositor_bind_group_layout,
        custom_shader_pipelines: p_set.custom_shader_pipelines,
        effect_bind_group_layout: p_set.effect_bind_group_layout,
        transition_bind_group_layout: p_set.transition_bind_group_layout,
        readback_buffer: gpu_res.readback_buffer,
        bytes_per_row: gpu_res.bytes_per_row,
        texture_size: gpu_res.texture_size,
        workgroups_x: (spec.composition.width + 15) / 16,
        workgroups_y: (spec.composition.height + 15) / 16,
    };

    let loop_result = run_render_loop(
        render_context,
        &spec,
        opts,
        &spot_check_frames,
        progress,
        &mut render_dur,
        &mut save_dur,
    );

    // ── Debug review ─────────────────────────────────────────────────────
    // Written before failure propagation: the review manifest matters most
    // when a render went wrong and the spot-check frames need a post-mortem.
    if let Some(ref run_dir) = opts.debug_run_dir {
        write_debug_review(run_dir, &spot_check_frames);
    }

    let output = match loop_result {
        Ok(output) => output,
        Err(e) => {
            // A failed render loop often means the GPU device is lost; force
            // the next job on this thread to re-initialise rather than fail
            // against a dead device.
            invalidate_gpu_cache();
            return Err(e);
        }
    };

    Ok(RenderOutcome {
        output,
        timings: PerformanceTimings {
            total: start_time.elapsed(),
            spec_load: None,
            img_load: img_load_dur,
            gpu_init: gpu_init_dur,
            resources: resources_dur,
            pipeline: pipeline_dur,
            render: render_dur,
            save: save_dur,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_flattens_semi_transparent_toward_black() {
        // Half-opaque mid-grey: each channel scales by 128/255 ≈ 0.502.
        let mut px = vec![200u8, 100, 50, 128];
        premultiply_alpha_on_black(&mut px);
        assert_eq!(px, vec![100, 50, 25, 128]);
    }

    #[test]
    fn premultiply_leaves_opaque_pixels_unchanged() {
        let mut px = vec![10u8, 20, 30, 255];
        premultiply_alpha_on_black(&mut px);
        assert_eq!(px, vec![10, 20, 30, 255]);
    }

    #[test]
    fn premultiply_zeroes_fully_transparent_rgb() {
        let mut px = vec![200u8, 100, 50, 0];
        premultiply_alpha_on_black(&mut px);
        assert_eq!(px, vec![0, 0, 0, 0]);
    }

    #[test]
    fn apply_override_coerces_types_and_creates_nested_paths() {
        let mut json = serde_json::json!({});
        apply_override(&mut json, "composition.width", "1920").unwrap();
        apply_override(&mut json, "composition.label", "hello").unwrap();
        apply_override(&mut json, "flag", "true").unwrap();
        assert_eq!(json["composition"]["width"], 1920);
        assert_eq!(json["composition"]["label"], "hello");
        assert_eq!(json["flag"], true);
    }
}
