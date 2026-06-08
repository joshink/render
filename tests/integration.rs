use std::path::Path;
use std::process::Command;
use image::{GenericImageView, Pixel};
use serde::Deserialize;

use render_poc::config::{RenderSpec, ClipType};

fn run_test_case(spec_name: &str) {
    let spec_path = format!("test_cases/{}", spec_name);
    let binary_path = env!("CARGO_BIN_EXE_render-poc");
    
    // Determine the base folder name (e.g., "01_identity" from "01_identity.json")
    let spec_base = spec_name.strip_suffix(".json").unwrap_or(spec_name);
    let debug_base_dir = format!("test_cases/outputs/debug/{}", spec_base);
    
    println!("Running binary: {} with spec: {} and debug dir: {}", binary_path, spec_path, debug_base_dir);

    // Parse spec first to get output path and composition parameters
    let spec_file = std::fs::File::open(&spec_path).expect("Failed to open spec JSON file");
    let spec: RenderSpec = serde_json::from_reader(spec_file).expect("Failed to parse spec JSON file");
    let output_path = Path::new(&spec.output);
    
    // Clean up old output if it exists
    if output_path.exists() {
        std::fs::remove_file(output_path).expect("Failed to clean up old output image/movie");
    }
    
    let debug_base_path = Path::new(&debug_base_dir);
    if debug_base_path.exists() {
        std::fs::remove_dir_all(debug_base_path).expect("Failed to clean up old debug base directory");
    }

    // Execute the rendering binary
    let status = Command::new(binary_path)
        .arg("-i")
        .arg(&spec_path)
        .arg("--debug")
        .arg(&debug_base_dir)
        .status()
        .expect("Failed to run rendering binary");

    assert!(status.success(), "Binary execution failed for spec {}", spec_name);
    assert!(output_path.exists(), "Output file was not created for spec {}", spec_name);

    // Verify debug folder structure
    let run_folder_name = format!("{}_json_render_0001", spec_base);
    let run_folder_path = debug_base_path.join(run_folder_name);
    
    assert!(run_folder_path.exists(), "Debug run folder {:?} was not created", run_folder_path);
    assert!(run_folder_path.join("logs.txt").exists(), "logs.txt was not created");
    assert!(run_folder_path.join("review.json").exists(), "review.json was not created");

    // Load review.json
    let review_file = std::fs::File::open(run_folder_path.join("review.json")).expect("Failed to open review.json");
    #[derive(Deserialize, Debug)]
    struct ReviewFrame {
        frame: u32,
        timestamp: f32,
        file: String,
        explanation: String,
    }
    let review_frames: Vec<ReviewFrame> = serde_json::from_reader(review_file).expect("Failed to parse review.json");
    assert!(!review_frames.is_empty(), "review.json is empty");

    // Load input image and resize to match spec composition
    let input_path = spec.get_input_path().expect("No media clip with asset path found in the test spec");
    let in_img = image::open(&input_path).expect("Failed to open input image");
    let in_resized = in_img.resize_exact(spec.composition.width, spec.composition.height, image::imageops::FilterType::Lanczos3);

    // Static effects from spec (for single frame checks or static checks)
    let mut static_grayscale = false;
    let mut static_dim = false;
    let mut static_bright = false;

    for track in &spec.tracks {
        for clip in &track.clips {
            if clip.clip_type == ClipType::Media {
                for effect in &clip.effects {
                    match effect.effect_type.as_str() {
                        "grayscale" => static_grayscale = true,
                        "brightness" => {
                            if let Some(ref params) = effect.params {
                                if let Some(factor_val) = params.get("factor") {
                                    if let Some(factor) = factor_val.as_f64() {
                                        if factor < 1.0 {
                                            static_dim = true;
                                        } else if factor > 1.0 {
                                            static_bright = true;
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let is_movie = spec.output.ends_with(".mp4");
    let is_blend_modes = spec_name == "10_blend_modes.json";
    let is_effect_layer = spec_name == "11_effect_layer.json";
    let is_transform_keyframe = spec_name == "12_transform_keyframe.json";
    let is_custom_shader_effect = spec_name == "13_custom_shader_effect.json";
    let is_hsl_adjust = spec_name == "14_hsl_adjust.json";
    let is_blur_glow = spec_name == "15_blur_glow.json" || spec_name == "21_presets.json";
    let is_film_effects = spec_name == "16_film_effects.json";
    let is_depth_blur = spec_name == "17_depth_blur.json";
    let is_fluid_flow = spec_name == "18_fluid_flow.json";
    let is_pixelation = spec_name == "19_pixelation.json";
    let is_chromatic_aberration = spec_name == "20_chromatic_aberration.json";

    let is_identity = !static_grayscale && !static_dim && !static_bright
        && !is_blend_modes && !is_effect_layer && !is_transform_keyframe && !is_custom_shader_effect
        && !is_hsl_adjust && !is_blur_glow && !is_film_effects && !is_depth_blur && !is_fluid_flow
        && !is_pixelation && !is_chromatic_aberration;

    // Assertions for each frame in review.json
    for review in &review_frames {
        let frame_path = run_folder_path.join(&review.file);
        assert!(frame_path.exists(), "Frame file {:?} does not exist", frame_path);

        let out_img = image::open(frame_path).expect("Failed to open debug frame image");
        assert_eq!(out_img.width(), spec.composition.width, "Debug frame width mismatch");
        assert_eq!(out_img.height(), spec.composition.height, "Debug frame height mismatch");

        // Determine active properties for this frame
        let mut check_grayscale = None;
        let mut check_dim = None;
        let mut check_bright = None;

        if is_movie {
            if review.explanation.contains("Grayscale is ON") {
                check_grayscale = Some(true);
            } else if review.explanation.contains("Grayscale is OFF") {
                check_grayscale = Some(false);
            }

            if review.explanation.contains("Brightness at peak") {
                if static_bright {
                    check_bright = Some(true);
                } else if static_dim {
                    check_dim = Some(true);
                }
            } else if review.explanation.contains("Brightness at trough") {
                check_dim = Some(true);
            }
        } else {
            if static_grayscale {
                check_grayscale = Some(true);
            }
            if static_dim {
                check_dim = Some(true);
            }
            if static_bright {
                check_bright = Some(true);
            }
            if is_blend_modes {
                check_dim = Some(true);
            }
            if is_effect_layer {
                check_grayscale = Some(true);
            }
            if is_transform_keyframe {
                check_dim = Some(true);
            }
        }

        // Perform pixel checks
        let mut total_in_luma: u64 = 0;
        let mut total_out_luma: u64 = 0;
        let mut has_color = false;
        let step_usize = 5usize;
        let step_u32 = step_usize as u32;

        for y in (0..spec.composition.height).step_by(step_usize) {
            for x in (0..spec.composition.width).step_by(step_usize) {
                let out_pixel = out_img.get_pixel(x, y).to_rgba();
                let in_pixel = in_resized.get_pixel(x, y).to_rgba();

                let r = out_pixel[0];
                let g = out_pixel[1];
                let b = out_pixel[2];

                if let Some(true) = check_grayscale {
                    let delta_rg = (r as i32 - g as i32).abs();
                    let delta_gb = (g as i32 - b as i32).abs();
                    assert!(delta_rg <= 2, "Pixel at ({}, {}) is not grayscale: R={}, G={}", x, y, r, g);
                    assert!(delta_gb <= 2, "Pixel at ({}, {}) is not grayscale: G={}, B={}", x, y, g, b);
                } else {
                    let delta_rg = (r as i32 - g as i32).abs();
                    let delta_gb = (g as i32 - b as i32).abs();
                    if delta_rg > 5 || delta_gb > 5 {
                        has_color = true;
                    }
                }

                let in_luma = (in_pixel[0] as u32 + in_pixel[1] as u32 + in_pixel[2] as u32) / 3;
                let out_luma = (out_pixel[0] as u32 + out_pixel[1] as u32 + out_pixel[2] as u32) / 3;
                total_in_luma += in_luma as u64;
                total_out_luma += out_luma as u64;
            }
        }

        if let Some(false) = check_grayscale {
            assert!(has_color, "Frame {} (t={:.2}) is grayscale but should have color!", review.frame, review.timestamp);
        }

        let num_samples = (((spec.composition.height + step_u32 - 1) / step_u32) * ((spec.composition.width + step_u32 - 1) / step_u32)) as f64;
        let avg_in = total_in_luma as f64 / num_samples;
        let avg_out = total_out_luma as f64 / num_samples;

        println!("Frame {} (t={:.2}): Avg input luma = {:.2}, Avg output luma = {:.2}", review.frame, review.timestamp, avg_in, avg_out);

        if let Some(true) = check_dim {
            assert!(avg_out < avg_in, "Frame {} not dimmed! avg_in={:.2}, avg_out={:.2}", review.frame, avg_in, avg_out);
        } else if let Some(true) = check_bright {
            if avg_in < 250.0 {
                assert!(avg_out > avg_in, "Frame {} not brightened! avg_in={:.2}, avg_out={:.2}", review.frame, avg_in, avg_out);
            }
        } else if is_identity {
            // Identity check (if no active effects/filters on this frame)
            let mut diff_count = 0;
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
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
            assert!(diff_ratio < 0.05, "Frame {} differs too much from input (diff_ratio={:.4})", review.frame, diff_ratio);
        } else if is_custom_shader_effect {
            let mut diff_count = 0;
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
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
            assert!(diff_ratio > 0.05, "Frame {} did not have custom wave effect applied (diff_ratio={:.4} <= 0.05)", review.frame, diff_ratio);
        } else if is_pixelation {
            let mut diff_count = 0;
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
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
            assert!(diff_ratio > 0.05, "Frame {} did not have pixelation applied (diff_ratio={:.4} <= 0.05)", review.frame, diff_ratio);
        } else if is_chromatic_aberration {
            let mut diff_count = 0;
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
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
            assert!(diff_ratio > 0.05, "Frame {} did not have chromatic aberration applied (diff_ratio={:.4} <= 0.05)", review.frame, diff_ratio);
        } else if is_hsl_adjust {
            let mut diff_count = 0;
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
                    let out_pixel = out_img.get_pixel(x, y).to_rgba();
                    let in_pixel = in_resized.get_pixel(x, y).to_rgba();
                    for c in 0..3 {
                        if (out_pixel[c] as i32 - in_pixel[c] as i32).abs() > 10 {
                            diff_count += 1;
                        }
                    }
                }
            }
            let total_sampled_channels = num_samples * 3.0;
            let diff_ratio = diff_count as f64 / total_sampled_channels;
            assert!(diff_ratio > 0.1, "Frame {} did not have HSL adjustment applied (diff_ratio={:.4} <= 0.1)", review.frame, diff_ratio);
        } else if is_blur_glow {
            let mut diff_count = 0;
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
                    let out_pixel = out_img.get_pixel(x, y).to_rgba();
                    let in_pixel = in_resized.get_pixel(x, y).to_rgba();
                    for c in 0..3 {
                        if (out_pixel[c] as i32 - in_pixel[c] as i32).abs() > 5 {
                            diff_count += 1;
                        }
                    }
                }
            }
            let total_sampled_channels = num_samples * 3.0;
            let diff_ratio = diff_count as f64 / total_sampled_channels;
            assert!(diff_ratio > 0.1, "Frame {} did not have Blur/Glow applied (diff_ratio={:.4} <= 0.1)", review.frame, diff_ratio);
        } else if is_film_effects {
            let mut diff_count = 0;
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
                    let out_pixel = out_img.get_pixel(x, y).to_rgba();
                    let in_pixel = in_resized.get_pixel(x, y).to_rgba();
                    for c in 0..3 {
                        if (out_pixel[c] as i32 - in_pixel[c] as i32).abs() > 2 {
                            diff_count += 1;
                        }
                    }
                }
            }
            let total_sampled_channels = num_samples * 3.0;
            let diff_ratio = diff_count as f64 / total_sampled_channels;
            assert!(diff_ratio > 0.05, "Frame {} did not have film grain noise applied (diff_ratio={:.4} <= 0.05)", review.frame, diff_ratio);
        } else if is_depth_blur {
            let cx = spec.composition.width as i32 / 2;
            let cy = spec.composition.height as i32 / 2;
            let mut center_diff = 0.0;
            let mut edge_diff = 0.0;
            let mut center_count = 0.0;
            let mut edge_count = 0.0;
            
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
                    let out_pixel = out_img.get_pixel(x, y).to_rgba();
                    let in_pixel = in_resized.get_pixel(x, y).to_rgba();
                    let dx = x as i32 - cx;
                    let dy = y as i32 - cy;
                    let dist = ((dx * dx + dy * dy) as f64).sqrt();
                    let normalized_dist = dist / (cx as f64);
                    
                    let mut diff = 0.0;
                    for c in 0..3 {
                        diff += (out_pixel[c] as f32 - in_pixel[c] as f32).abs() as f64;
                    }
                    diff /= 3.0;
                    
                    if normalized_dist < 0.25 {
                        center_diff += diff;
                        center_count += 1.0;
                    } else if normalized_dist > 0.6 {
                        edge_diff += diff;
                        edge_count += 1.0;
                    }
                }
            }
            
            let avg_center_diff = center_diff / center_count;
            let avg_edge_diff = edge_diff / edge_count;
            println!("Depth blur: Avg center diff = {:.2}, Avg edge diff = {:.2}", avg_center_diff, avg_edge_diff);
            assert!(avg_center_diff < 5.0, "Center of depth blur is too blurry! diff={:.2}", avg_center_diff);
            assert!(avg_edge_diff > 10.0, "Edges of depth blur are not blurred enough! diff={:.2}", avg_edge_diff);
        } else if is_fluid_flow {
            let mut diff_count = 0;
            for y in (0..spec.composition.height).step_by(step_usize) {
                for x in (0..spec.composition.width).step_by(step_usize) {
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
            assert!(diff_ratio > 0.05, "Frame {} did not have fluid flow displacement applied (diff_ratio={:.4} <= 0.05)", review.frame, diff_ratio);
        }
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
    run_test_case("07_movie.json");
}

#[test]
fn test_08_movie_audio() {
    run_test_case("08_movie_audio.json");
    
    // Verify audio stream presence using ffprobe
    let output_path = Path::new("test_cases/outputs/08_movie_audio.mp4");
    let ffprobe_status = Command::new("ffprobe")
        .args(&[
            "-v", "error",
            "-show_entries", "stream=codec_type",
            "-of", "csv=p=0",
            output_path.to_str().unwrap(),
        ])
        .output();
    if let Ok(output) = ffprobe_status {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("audio"), "Output MP4 does not contain an audio stream! Streams: {}", stdout);
        println!("ffprobe verified audio stream exists: {}", stdout.trim());
    } else {
        panic!("Failed to run ffprobe to verify audio");
    }
}

#[test]
fn test_09_ripple_relative() {
    run_test_case("09_ripple_relative.json");
}

#[test]
fn test_10_blend_modes() {
    run_test_case("10_blend_modes.json");
}

#[test]
fn test_11_effect_layer() {
    run_test_case("11_effect_layer.json");
}

#[test]
fn test_12_transform_keyframe() {
    run_test_case("12_transform_keyframe.json");
}

#[test]
fn test_13_custom_shader_effect() {
    run_test_case("13_custom_shader_effect.json");
}

#[test]
fn test_14_hsl_adjust() {
    run_test_case("14_hsl_adjust.json");
}

#[test]
fn test_15_blur_glow() {
    run_test_case("15_blur_glow.json");
}

#[test]
fn test_16_film_effects() {
    run_test_case("16_film_effects.json");
}

#[test]
fn test_17_depth_blur() {
    run_test_case("17_depth_blur.json");
}

#[test]
fn test_18_fluid_flow() {
    run_test_case("18_fluid_flow.json");
}

#[test]
fn test_19_pixelation() {
    run_test_case("19_pixelation.json");
}

#[test]
fn test_20_chromatic_aberration() {
    run_test_case("20_chromatic_aberration.json");
}

#[test]
fn test_21_presets() {
    run_test_case("21_presets.json");
}

