mod config;

use config::RenderSpec;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use log::{info, error};

fn main() {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    info!("Starting Render PoC...");

    // 1. Load JSON spec
    let spec_path = Path::new("spec.json");
    if !spec_path.exists() {
        error!("spec.json not found in the current directory!");
        std::process::exit(1);
    }

    let file = File::open(spec_path).expect("Failed to open spec.json");
    let reader = BufReader::new(file);
    let spec: RenderSpec = serde_json::from_reader(reader).expect("Failed to parse spec.json");
    info!("Loaded spec: {:?}", spec);

    // 2. Load input image
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

    // 3. Initialize Headless wgpu
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

    // 4. Set up GPU textures
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
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
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
    let shader_params = spec.to_shader_params();
    info!("Shader params: {:?}", shader_params);

    let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Params Buffer"),
        size: std::mem::size_of::<config::ShaderParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    queue.write_buffer(&params_buffer, 0, bytemuck::bytes_of(&shader_params));

    // 6. Set up compute pipeline
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

    // 7. Dispatch compute work
    info!("Recording GPU commands...");
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
        
        // Calculate workgroups based on 16x16 size in shader
        let workgroups_x = (spec.width + 15) / 16;
        let workgroups_y = (spec.height + 15) / 16;
        compute_pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
    }

    // 8. Copy output texture to readback buffer
    // Calculate aligned row pitch (must be multiple of 256 bytes)
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

    info!("Submitting commands to GPU...");
    queue.submit(Some(encoder.finish()));

    // 9. Read buffer back to CPU
    info!("Reading back output texture from GPU...");
    let buffer_slice = readback_buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    
    buffer_slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).unwrap();
    });

    // Wait for the GPU to finish rendering and mapping the buffer
    device.poll(wgpu::Maintain::Wait);

    if let Ok(Ok(())) = receiver.recv() {
        let data = buffer_slice.get_mapped_range();
        
        // Strip 256-byte alignment padding from rows if it exists
        let mut unpadded_pixels = Vec::with_capacity((spec.width * spec.height * 4) as usize);
        for row in 0..spec.height {
            let start = (row * bytes_per_row) as usize;
            let end = start + (spec.width * 4) as usize;
            unpadded_pixels.extend_from_slice(&data[start..end]);
        }
        
        // Drop the mapped view before unmapping the buffer
        drop(data);
        readback_buffer.unmap();

        // 10. Save the final image
        info!("Saving processed image to '{}'...", spec.output);
        image::save_buffer(
            &spec.output,
            &unpadded_pixels,
            spec.width,
            spec.height,
            image::ExtendedColorType::Rgba8,
        )
        .expect("Failed to save output image");

        info!("Success! Render complete.");
    } else {
        error!("Failed to map buffer back to CPU!");
    }
}
