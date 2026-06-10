//! Shader validation tests.
//!
//! The render binary loads every `.wgsl` from `library/` at runtime and hands
//! it to wgpu, which validates it against the GPU. CI hosts have no GPU, so a
//! malformed shader (e.g. a typo in the compositor's bilinear sampling path)
//! would otherwise go undetected until production. These tests run naga — the
//! same front-end and validator wgpu uses internally — directly on the sources.

use std::path::{Path, PathBuf};

fn library_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("library")
}

/// Parse and fully validate a single WGSL source, panicking with a readable
/// message on failure.
fn validate_wgsl(path: &Path) {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", path, e));

    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("WGSL parse error in {:?}:\n{}", path, e.emit_to_string(&source)));

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .unwrap_or_else(|e| panic!("WGSL validation error in {:?}: {:?}", path, e));
}

#[test]
fn compositor_shader_is_valid() {
    // The compositor is the shader the engine exercises every frame, and the
    // one the bilinear media-sampling change lives in.
    validate_wgsl(&library_dir().join("compositor.wgsl"));
}

#[test]
fn all_library_shaders_parse_and_validate() {
    let mut checked = 0;
    let mut stack = vec![library_dir()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {:?}: {}", dir, e)) {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("wgsl") {
                validate_wgsl(&path);
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "No .wgsl files found under library/");
}
