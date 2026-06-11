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

// ─── Parameter-contract conformance ─────────────────────────────────────────
//
// Custom params reach a shader as a uniform buffer packed in *name-sorted*
// order: `pack_effect_params` packs metadata params sorted by target name, and
// `pack_custom_params` (the no-metadata path) packs the spec-provided keys
// sorted. The shader's `CustomParams` struct is the other half of that
// contract, so for every library shader we enforce:
//
// 1. `CustomParams` fields are declared in ascending (byte-wise) name order —
//    otherwise packed values land on the wrong fields.
// 2. Every declared field is actually read by the shader body — a declared
//    but unused param is a silent no-op for the user who sets it.
// 3. A metadata header, when present, parses and its target names match the
//    struct fields exactly — otherwise defaults/spot-checks drift from the
//    real layout.

/// Field names of the `CustomParams` struct, in declaration order.
fn custom_params_fields(source: &str) -> Option<Vec<String>> {
    let struct_start = source.find("struct CustomParams")?;
    let body_start = struct_start + source[struct_start..].find('{')? + 1;
    let body_end = body_start + source[body_start..].find('}')?;
    let mut fields = Vec::new();
    for line in source[body_start..body_end].lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if let Some((name, _ty)) = line.split_once(':') {
            let name = name.trim();
            if !name.is_empty() {
                fields.push(name.to_string());
            }
        }
    }
    Some(fields)
}

/// Name of the uniform variable of type `CustomParams`
/// (`var<uniform> NAME: CustomParams;`).
fn custom_params_var(source: &str) -> Option<String> {
    source.lines().find_map(|line| {
        let idx = line.find("var<uniform>")?;
        let (name, ty) = line[idx + "var<uniform>".len()..].split_once(':')?;
        (ty.trim().trim_end_matches(';').trim() == "CustomParams").then(|| name.trim().to_string())
    })
}

/// Explicit alignment padding (e.g. `z_padding`, `padding1`) is part of the
/// struct layout, not a real parameter — exempt from the unused-field check
/// and from metadata comparison.
fn is_padding_field(name: &str) -> bool {
    name.starts_with('_') || name.to_ascii_lowercase().contains("padding")
}

/// True if `var.field` appears in `source` not followed by another identifier
/// character (so `params.radius` does not match `params.radius_inner`).
fn references_field(source: &str, var: &str, field: &str) -> bool {
    let needle = format!("{}.{}", var, field);
    let mut search_from = 0;
    while let Some(pos) = source[search_from..].find(&needle) {
        let end = search_from + pos + needle.len();
        let next_char_is_ident = source[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if !next_char_is_ident {
            return true;
        }
        search_from = end;
    }
    false
}

fn library_shader_sources(subdir: &str) -> Vec<(PathBuf, String)> {
    let dir = library_dir().join(subdir);
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {:?}: {}", dir, e)) {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("wgsl") {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", path, e));
            sources.push((path, source));
        }
    }
    assert!(!sources.is_empty(), "No .wgsl files found under library/{}/", subdir);
    sources
}

/// Checks rules 1–3 above for one shader; returns the violations found.
fn param_contract_violations(path: &Path, source: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let fields = custom_params_fields(source);

    if let Some(fields) = &fields {
        let mut sorted = fields.clone();
        sorted.sort();
        if *fields != sorted {
            violations.push(format!(
                "{:?}: CustomParams fields are not in sorted order (packing is name-sorted); \
                 declared {:?}, expected {:?}",
                path, fields, sorted
            ));
        }
        match custom_params_var(source) {
            Some(var) => {
                for field in fields.iter().filter(|f| !is_padding_field(f)) {
                    if !references_field(source, &var, field) {
                        violations.push(format!(
                            "{:?}: CustomParams field `{}` is never read — setting it is a silent no-op",
                            path, field
                        ));
                    }
                }
            }
            None => violations.push(format!(
                "{:?}: CustomParams struct has no `var<uniform>` binding",
                path
            )),
        }
    }

    // Metadata, when present, must parse and match the struct exactly.
    let metadata_params: Option<Vec<render::config::EffectParamMetadata>> =
        if let Some(json) = render::config::extract_metadata(source) {
            match serde_json::from_str::<render::config::EffectMetadata>(&json) {
                Ok(meta) => Some(meta.params),
                Err(e) => {
                    violations.push(format!("{:?}: EFFECTS_METADATA does not parse: {}", path, e));
                    None
                }
            }
        } else if let Some(json) = render::config::extract_transition_metadata(source) {
            match serde_json::from_str::<render::config::TransitionMetadata>(&json) {
                Ok(meta) => Some(meta.params),
                Err(e) => {
                    violations.push(format!("{:?}: TRANSITION_METADATA does not parse: {}", path, e));
                    None
                }
            }
        } else {
            None
        };

    if let Some(params) = metadata_params {
        let mut targets: Vec<String> = params
            .iter()
            .map(|p| p.target_name().to_string())
            .filter(|t| !is_padding_field(t))
            .collect();
        targets.sort();
        let fields: Vec<String> = fields
            .unwrap_or_default()
            .into_iter()
            .filter(|f| !is_padding_field(f))
            .collect();
        if targets != fields {
            violations.push(format!(
                "{:?}: metadata target names {:?} do not match CustomParams fields {:?}",
                path, targets, fields
            ));
        }
    }

    violations
}

#[test]
fn effect_and_transition_params_are_conformant() {
    let mut violations = Vec::new();
    for subdir in ["effects", "transitions"] {
        for (path, source) in library_shader_sources(subdir) {
            violations.extend(param_contract_violations(&path, &source));
        }
    }
    assert!(
        violations.is_empty(),
        "Shader parameter-contract violations:\n{}",
        violations.join("\n")
    );
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
