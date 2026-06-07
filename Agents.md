# Onboarding Guide for AI Coding Agents

Welcome to **Render**! This document provides context, architectural guidelines, and safety constraints for agents working on this codebase.

---

## 1. Codebase Layout

- [Cargo.toml](file:///Users/joshink/Development/render/Cargo.toml): Core dependencies. We use `wgpu` for headless rendering, `serde`/`serde_json` for config deserialization, `image` for pixel buffer IO, and `bytemuck` for safe CPU-to-GPU data transmission.
- [src/config.rs](file:///Users/joshink/Development/render/src/config.rs): Decodes JSON specs. Translates dynamic parameter configurations into a statically sized, memory-aligned uniform struct (`ShaderParams`).
- [src/effects.wgsl](file:///Users/joshink/Development/render/src/effects.wgsl): The WGSL compute shader code. It performs the per-pixel computations.
- [src/main.rs](file:///Users/joshink/Development/render/src/main.rs): Setup code for the WebGPU instance, device, queue, bind groups, pipelines, and CPU-GPU transfer orchestrations.

---

## 2. Strict GPU Programming Constraints

When writing or modifying rendering code in this repository, you must adhere to the following hardware-level constraints:

### A. WebGPU Buffer Row-Pitch Alignment (Crucial)
When copying textures to buffers (e.g., `encoder.copy_texture_to_buffer`) for reading pixels back to the CPU, WebGPU enforces that **the bytes per row of the destination buffer must be a multiple of 256 bytes** (`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`).
*   **Formula:** `bytes_per_row = (width * bytes_per_pixel + 255) & !255;`
*   **Unpadding:** The resulting buffer mapped back to the CPU will contain padding bytes at the end of each row if the width is not a multiple of 64 pixels (for 4-byte RGBA). You **must** copy the row slices individually to remove this padding before passing the pixel array to encoders or image saving libraries.

### B. WGSL Uniform Block Alignment
WGSL uniform variables (like the `Params` struct) must align to 16-byte boundaries (std140 layout).
*   Always ensure your Rust structs sent as uniforms are decorated with `#[repr(C)]` and contain explicit padding fields (e.g., `_padding: [u32; 2]`) to make their total byte size a multiple of 16.
*   Annotate these structs with `#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]` for zero-overhead casting.

### C. Headless GPU Requesting
Since this is a headless engine, never request a surface or swapchain unless window previewing is explicitly requested.
*   Request the adapter with `compatible_surface: None`.
*   Ensure the storage texture format matches standard non-SRGB options (e.g., `wgpu::TextureFormat::Rgba8Unorm`) since SRGB write-only storage textures are not widely supported on some backends.

---

## 3. Tech Stack Roadmap

If you are tasked with extending the codebase, focus on the following priorities:

1.  **Video Decoding Pipeline:**
    *   Integrate `ffmpeg-next` or `video-rs` to demux container files and decode streams to raw frame buffers.
    *   Implement YUV (YUV420p) texture uploads. Since GPUs natively process RGB, write a WGSL shader to perform YUV-to-RGB color space conversions (BT.601/BT.709/BT.2020) directly on the GPU.
2.  **Render Graph (DAG):**
    *   Build a pipeline scheduler that resolves timelines (tracks, overlaps, start/end boundaries) into a Directed Acyclic Graph (DAG) of compute and render passes.
3.  **Expression Engine:**
    *   Implement variable-over-time evaluation (keyframing) using a lightweight expression parser (like `evalexpr`) or an embedded script VM (like Lua or QuickJS) to allow dynamic property scripts (e.g., scaling, rotating, or vibrating elements).
