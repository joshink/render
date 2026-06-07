use std::path::Path;
use std::process::Command;
use image::{GenericImageView, Pixel};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug)]
struct TestSpec {
    width: u32,
    height: u32,
    input: String,
    output: String,
    effects: Vec<serde_json::Value>,
}

fn run_test_case(spec_name: &str) {
    let spec_path = format!("test_cases/{}", spec_name);
    let binary_path = env!("CARGO_BIN_EXE_render-poc");

    println!("Running binary: {} with spec: {}", binary_path, spec_path);

    // Ensure parent directory for output exists
    let spec_file = std::fs::File::open(&spec_path).expect("Failed to open spec JSON file");
    let spec: TestSpec = serde_json::from_reader(spec_file).expect("Failed to parse spec JSON file");
    let output_path = Path::new(&spec.output);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).expect("Failed to create output directory");
    }

    // Clean up old output if it exists
    if output_path.exists() {
        std::fs::remove_file(output_path).expect("Failed to clean up old output image");
    }

    // Execute the rendering binary
    let status = Command::new(binary_path)
        .arg(&spec_path)
        .status()
        .expect("Failed to run rendering binary");

    assert!(status.success(), "Binary execution failed for spec {}", spec_name);
    assert!(output_path.exists(), "Output image was not created for spec {}", spec_name);

    // Load output image
    let out_img = image::open(output_path).expect("Failed to open generated output image");
    assert_eq!(out_img.width(), spec.width, "Output image width mismatch");
    assert_eq!(out_img.height(), spec.height, "Output image height mismatch");

    // Perform specific effect assertions
    let mut has_grayscale = false;
    let mut is_dim = false;
    let mut is_bright = false;

    for effect in &spec.effects {
        if let Some(effect_type) = effect.get("effect_type").and_then(|v| v.as_str()) {
            match effect_type {
                "grayscale" => has_grayscale = true,
                "brightness" => {
                    if let Some(factor) = effect.get("params").and_then(|p| p.get("factor")).and_then(|f| f.as_f64()) {
                        if factor < 1.0 {
                            is_dim = true;
                        } else if factor > 1.0 {
                            is_bright = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Load input image to compare brightness/pixels if needed
    let in_img = image::open(&spec.input).expect("Failed to open input image");
    // Resize input image matching spec to get a correct 1:1 comparison baseline
    let in_resized = in_img.resize_exact(spec.width, spec.height, image::imageops::FilterType::Lanczos3);

    // Verify pixels
    let mut total_in_luma: u64 = 0;
    let mut total_out_luma: u64 = 0;
    let step_usize = 5usize;
    let step_u32 = step_usize as u32;

    for y in (0..spec.height).step_by(step_usize) {
        for x in (0..spec.width).step_by(step_usize) {
            let out_pixel = out_img.get_pixel(x, y).to_rgba();
            let in_pixel = in_resized.get_pixel(x, y).to_rgba();

            // 1. Grayscale verification: R, G, B should be equal (within threshold)
            if has_grayscale {
                let r = out_pixel[0];
                let g = out_pixel[1];
                let b = out_pixel[2];
                // Allow a small delta of 2 due to rounding differences in YUV/grayscale luma formula
                let delta_rg = (r as i32 - g as i32).abs();
                let delta_gb = (g as i32 - b as i32).abs();
                assert!(delta_rg <= 2, "Pixel at ({}, {}) is not grayscale: R={}, G={}", x, y, r, g);
                assert!(delta_gb <= 2, "Pixel at ({}, {}) is not grayscale: G={}, B={}", x, y, g, b);
            }

            // Accumulate luma (simple average of RGB) for brightness verification
            let in_luma = (in_pixel[0] as u32 + in_pixel[1] as u32 + in_pixel[2] as u32) / 3;
            let out_luma = (out_pixel[0] as u32 + out_pixel[1] as u32 + out_pixel[2] as u32) / 3;
            total_in_luma += in_luma as u64;
            total_out_luma += out_luma as u64;
        }
    }

    let num_samples = (((spec.height + step_u32 - 1) / step_u32) * ((spec.width + step_u32 - 1) / step_u32)) as f64;
    let avg_in = total_in_luma as f64 / num_samples;
    let avg_out = total_out_luma as f64 / num_samples;

    println!("Spec {}: Avg input luma = {:.2}, Avg output luma = {:.2}", spec_name, avg_in, avg_out);

    if is_dim {
        assert!(avg_out < avg_in, "Output image is not dimmed! avg_in={:.2}, avg_out={:.2}", avg_in, avg_out);
    } else if is_bright {
        // Since we scale brightness, ensure average luma increased (unless already saturated at 255)
        if avg_in < 250.0 {
            assert!(avg_out > avg_in, "Output image is not brightened! avg_in={:.2}, avg_out={:.2}", avg_in, avg_out);
        }
    } else if !has_grayscale {
        // Identity check: pixel values should be exactly/very close to resized input values
        // Allow tiny delta due to compression/GPU format precision
        let mut diff_count = 0;
        for y in (0..spec.height).step_by(step_usize) {
            for x in (0..spec.width).step_by(step_usize) {
                let out_pixel = out_img.get_pixel(x, y).to_rgba();
                let in_pixel = in_resized.get_pixel(x, y).to_rgba();
                for c in 0..3 {
                    if (out_pixel[c] as i32 - in_pixel[c] as i32).abs() > 3 {
                        diff_count += 1;
                    }
                }
            }
        }
        let total_sampled_channels = num_samples * 3.0;
        let diff_ratio = diff_count as f64 / total_sampled_channels;
        assert!(diff_ratio < 0.05, "Identity output differs too much from input (diff_ratio={:.4})", diff_ratio);
    }
}

#[test]
fn test_01_identity() {
    run_test_case("01_identity.json");
}

#[test]
fn test_02_grayscale() {
    run_test_case("02_grayscale.json");
}

#[test]
fn test_03_brightness_dim() {
    run_test_case("03_brightness_dim.json");
}

#[test]
fn test_04_brightness_bright() {
    run_test_case("04_brightness_bright.json");
}

#[test]
fn test_05_grayscale_brightness() {
    run_test_case("05_grayscale_brightness.json");
}

#[test]
fn test_06_padding_stress() {
    run_test_case("06_padding_stress.json");
}

#[test]
fn test_07_movie() {
    let spec_path = "test_cases/07_movie.json";
    let binary_path = env!("CARGO_BIN_EXE_render-poc");

    println!("Running binary for movie: {} with spec: {}", binary_path, spec_path);

    let output_path = Path::new("test_cases/outputs/07_movie.mp4");
    if output_path.exists() {
        std::fs::remove_file(output_path).expect("Failed to clean up old movie");
    }

    // Execute the rendering binary
    let status = Command::new(binary_path)
        .arg(spec_path)
        .status()
        .expect("Failed to run rendering binary");

    assert!(status.success(), "Binary execution failed for movie spec");
    assert!(output_path.exists(), "Output movie was not created");
    assert!(output_path.metadata().unwrap().len() > 0, "Output movie is empty");
}
