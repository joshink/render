use std::path::Path;
use std::process::Command;
use image::{GenericImageView, Pixel};
use serde::Deserialize;

use render_poc::config::RenderSpec;



fn run_test_case(spec_name: &str) {
    let spec_path = format!("tests/fixtures/{}", spec_name);
    let binary_path = env!("CARGO_BIN_EXE_render-poc");
    
    // Determine the base folder name (e.g., "01_identity" from "01_identity.json")
    let spec_base = spec_name.strip_suffix(".json").unwrap_or(spec_name);
    let debug_base_dir = format!("tests/fixtures/outputs/debug/{}", spec_base);
    
    println!("Running binary: {} with spec: {} and debug dir: {}", binary_path, spec_path, debug_base_dir);

    // Parse spec first to get output path and composition parameters
    let spec_file = std::fs::File::open(&spec_path).expect("Failed to open spec JSON file");
    let spec: RenderSpec = serde_json::from_reader(spec_file).expect("Failed to parse spec JSON file");
    let output_path = Path::new(spec.output.path());
    
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
    let in_resized = if let Some(mut input_path) = spec.get_input_path() {
        if input_path.starts_with("http://") || input_path.starts_with("https://") {
            input_path = render_poc::download::fetch_remote_url(&input_path).expect("Failed to fetch remote test asset");
        }
        let in_img = image::open(&input_path).expect("Failed to open input image");
        in_img.resize_exact(spec.composition.width, spec.composition.height, image::imageops::FilterType::Nearest)
    } else {
        image::DynamicImage::ImageRgba8(image::ImageBuffer::new(spec.composition.width, spec.composition.height))
    };


    
    // Assertions for each frame in review.json
    for review in &review_frames {
        let frame_path = run_folder_path.join(&review.file);
        assert!(frame_path.exists(), "Frame file {:?} does not exist", frame_path);

        let out_img = image::open(frame_path).expect("Failed to open debug frame image");
        assert_eq!(out_img.width(), spec.composition.width, "Debug frame width mismatch");
        assert_eq!(out_img.height(), spec.composition.height, "Debug frame height mismatch");

        let step_usize = 5usize;
        let step_u32 = step_usize as u32;
        let num_samples = (((spec.composition.height + step_u32 - 1) / step_u32) * ((spec.composition.width + step_u32 - 1) / step_u32)) as f64;


        // Determine active properties for this frame
        let mut check_grayscale = None;
        let mut check_dim = None;
        let mut check_bright = None;
        let mut is_hsl_adjust = false;
        let mut is_blur_glow = false;
        let mut is_film_effects = false;
        let mut is_depth_blur = false;
        let mut is_fluid_flow = false;
        let mut is_pixelation = false;
        let mut is_chromatic_aberration = false;
        let is_identity = spec_name == "01_identity.json";

        if spec_name == "02_all_filters.json" {
            let t = review.timestamp;
            if t >= 0.0 && t <= 2.0 {
                check_grayscale = Some(true);
            } else if t > 2.0 && t <= 4.0 {
                check_dim = Some(true);
            } else if t > 4.0 && t <= 6.0 {
                check_bright = Some(true);
            } else if t > 6.0 && t <= 8.0 {
                check_grayscale = Some(true);
                check_dim = Some(true);
            } else if t > 8.0 && t <= 10.0 {
                is_hsl_adjust = true;
            } else if t > 10.0 && t <= 12.0 {
                is_blur_glow = true;
            } else if t > 12.0 && t <= 14.0 {
                is_film_effects = true;
            } else if t > 14.0 && t <= 16.0 {
                is_depth_blur = true;
            } else if t > 16.0 && t <= 18.0 {
                is_fluid_flow = true;
            } else if t > 18.0 && t <= 20.0 {
                is_pixelation = true;
            } else if t > 20.0 && t <= 22.0 {
                is_chromatic_aberration = true;
            }
        } else if spec_name == "03_image_placement.json" {
            let t = review.timestamp;
            if t >= 0.0 && t <= 2.0 {
                // fit: check left and right borders are transparent black
                let left_pixel = out_img.get_pixel(10, 300).to_rgba();
                let right_pixel = out_img.get_pixel(790, 300).to_rgba();
                assert_eq!(left_pixel[3], 0, "fit left border not transparent: {:?}", left_pixel);
                assert_eq!(right_pixel[3], 0, "fit right border not transparent: {:?}", right_pixel);
            } else if t > 2.0 && t <= 4.0 {
                // fill: check left and right borders are opaque
                let left_pixel = out_img.get_pixel(10, 300).to_rgba();
                let right_pixel = out_img.get_pixel(790, 300).to_rgba();
                assert!(left_pixel[3] > 0, "fill left border is empty");
                assert!(right_pixel[3] > 0, "fill right border is empty");
            } else if t > 4.0 && t <= 6.0 {
                // stretch
                let left_pixel = out_img.get_pixel(10, 300).to_rgba();
                assert!(left_pixel[3] > 0);
            } else if t > 6.0 && t <= 8.0 {
                // natural
                let left_pixel = out_img.get_pixel(10, 300).to_rgba();
                assert!(left_pixel[3] > 0);
            }
        } else if spec_name == "04_transitions.json" {
            let t = review.timestamp;
            if (t - 2.0).abs() < 0.1 {
                // Midpoint of fade
                let pixel = out_img.get_pixel(400, 400).to_rgba();
                assert!(pixel[0] > 50 && pixel[1] > 50, "Fade midpoint not yellow/mix: R={}, G={}", pixel[0], pixel[1]);
            } else if (t - 4.0).abs() < 0.1 {
                // Midpoint of wipe
                let top_pixel = out_img.get_pixel(400, 100).to_rgba();
                let bottom_pixel = out_img.get_pixel(400, 700).to_rgba();
                assert!(top_pixel[1] > 150 && top_pixel[2] < 100, "Wipe top not green: {:?}", top_pixel);
                assert!(bottom_pixel[2] > 150 && bottom_pixel[1] < 100, "Wipe bottom not blue: {:?}", bottom_pixel);
            }
        } else if spec_name == "06_movie_audio.json" {
            if review.explanation.contains("Grayscale is ON") {
                check_grayscale = Some(true);
            } else if review.explanation.contains("Grayscale is OFF") {
                check_grayscale = Some(false);
            }
            if review.explanation.contains("Brightness at peak") {
                check_bright = Some(true);
            } else if review.explanation.contains("Brightness at trough") {
                check_dim = Some(true);
            }
        } else if spec_name == "07_ripple_relative.json" {
            if review.explanation.contains("Track 'main_track' - Clip 'clip_grayscale' starts") {
                check_grayscale = Some(true);
            } else if review.explanation.contains("Track 'main_track' - Clip 'clip_dim' starts") {
                check_dim = Some(true);
            }
        } else if spec_name == "09_tactile_transitions.json" {
            let t = review.timestamp;
            if (t - 2.0).abs() < 0.1 {
                // Flash peak luma check
                let mut total_luma = 0.0;
                let mut count = 0.0;
                for y in (0..800).step_by(20) {
                    for x in (0..800).step_by(20) {
                        let p = out_img.get_pixel(x, y).to_rgba();
                        total_luma += (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
                        count += 1.0;
                    }
                }
                let avg_luma = total_luma / count;
                assert!(avg_luma > 200.0, "Flash peak luma too low: {}", avg_luma);
            } else if (t - 4.0).abs() < 0.1 {
                // Focus blur midpoint - should be a mix of green and blue
                let pixel = out_img.get_pixel(400, 400).to_rgba();
                assert!(pixel[1] > 30 && pixel[2] > 30, "Focus midpoint not mixed green/blue: {:?}", pixel);
            } else if (t - 6.0).abs() < 0.1 {
                // Slide switch settled on white/grey
                let pixel = out_img.get_pixel(400, 400).to_rgba();
                assert!(pixel[0] > 100 && (pixel[0] as i32 - pixel[1] as i32).abs() < 20, "Slide switch not settled on white/grey: {:?}", pixel);
            }
        } else if spec_name == "10_dynamic_transitions.json" {
            let t = review.timestamp;
            if (t - 2.0).abs() < 0.1 {
                // Fade midpoint (red to green)
                let pixel = out_img.get_pixel(400, 400).to_rgba();
                assert!(pixel[0] > 50 && pixel[1] > 50, "Fade midpoint not mixed red/green: {:?}", pixel);
            } else if (t - 4.0).abs() < 0.1 {
                // Focus blur midpoint (green to blue)
                let pixel = out_img.get_pixel(400, 400).to_rgba();
                assert!(pixel[1] > 30 && pixel[2] > 30, "Focus midpoint not mixed green/blue: {:?}", pixel);
            } else if (t - 6.0).abs() < 0.1 {
                // Slide switch settled on white/grey
                let pixel = out_img.get_pixel(400, 400).to_rgba();
                assert!(pixel[0] > 100 && (pixel[0] as i32 - pixel[1] as i32).abs() < 20, "Slide switch not settled on white/grey: {:?}", pixel);
            }
        } else if spec_name == "11_text_transitions.json" {
            let t = review.timestamp;
            if t < 0.05 {
                // Entrance starting: no text should be visible yet.
            } else if (t - 1.5).abs() < 0.1 {
                // Midpoint of clip: text is fully visible.
                let t0_review = review_frames.iter().find(|r| r.timestamp < 0.05)
                    .expect("Missing t=0.0 frame in review.json");
                let t0_path = run_folder_path.join(&t0_review.file);
                let t0_img = image::open(t0_path).expect("Failed to open t=0.0 frame image");

                let mut diff_count = 0;
                let mut has_yellow_diff = false;
                let mut has_white_diff = false;

                for y in (0..spec.composition.height).step_by(step_usize) {
                    for x in (0..spec.composition.width).step_by(step_usize) {
                        let out_pixel = out_img.get_pixel(x, y).to_rgba();
                        let t0_pixel = t0_img.get_pixel(x, y).to_rgba();

                        let r_diff = (out_pixel[0] as i32 - t0_pixel[0] as i32).abs();
                        let g_diff = (out_pixel[1] as i32 - t0_pixel[1] as i32).abs();
                        let b_diff = (out_pixel[2] as i32 - t0_pixel[2] as i32).abs();

                        if r_diff > 10 || g_diff > 10 || b_diff > 10 {
                            diff_count += 1;

                            let r = out_pixel[0];
                            let g = out_pixel[1];
                            let b = out_pixel[2];

                            // Yellow/gold text color: [1.0, 0.9, 0.1]
                            if r > 200 && g > 180 && b < 100 {
                                has_yellow_diff = true;
                            }
                            // White text color: [1.0, 1.0, 1.0]
                            if r > 220 && g > 220 && b > 220 {
                                has_white_diff = true;
                            }
                        }
                    }
                }

                let diff_ratio = diff_count as f64 / num_samples;
                println!("t=1.5 text transition comparison: diff_ratio={:.4}, has_yellow={}, has_white={}", diff_ratio, has_yellow_diff, has_white_diff);

                assert!(diff_ratio > 0.01 && diff_ratio < 0.20, "Midpoint diff ratio {:.4} out of expected bounds (0.01 - 0.20)", diff_ratio);
                assert!(has_yellow_diff, "Midpoint frame does not contain yellow text pixels");
                assert!(has_white_diff, "Midpoint frame does not contain white text pixels");
            } else if t > 2.95 {
                // Exit finished: text should be fully invisible.
                let t0_review = review_frames.iter().find(|r| r.timestamp < 0.05)
                    .expect("Missing t=0.0 frame in review.json");
                let t0_path = run_folder_path.join(&t0_review.file);
                let t0_img = image::open(t0_path).expect("Failed to open t=0.0 frame image");

                let mut diff_count = 0;
                for y in (0..spec.composition.height).step_by(step_usize) {
                    for x in (0..spec.composition.width).step_by(step_usize) {
                        let out_pixel = out_img.get_pixel(x, y).to_rgba();
                        let t0_pixel = t0_img.get_pixel(x, y).to_rgba();
                        for c in 0..3 {
                            if (out_pixel[c] as i32 - t0_pixel[c] as i32).abs() > 2 {
                                diff_count += 1;
                            }
                        }
                    }
                }
                let diff_ratio = diff_count as f64 / (num_samples * 3.0);
                assert!(diff_ratio < 0.01, "Text visible at end of exit transition or background mismatch (diff_ratio={:.4})", diff_ratio);
            }
        } else if spec_name == "12_layout_comprehensive.json" {
            let t = review.timestamp;
            if (t - 3.0).abs() < 0.1 || (t - 5.0).abs() < 0.1 || (t - 7.0).abs() < 0.1 {
                let t1_review = review_frames.iter().find(|r| (r.timestamp - 1.0).abs() < 0.1)
                    .expect("Missing t=1.0 frame in review.json");
                let t1_path = run_folder_path.join(&t1_review.file);
                let t1_img = image::open(t1_path).expect("Failed to open t=1.0 frame image");

                let mut diff_count = 0;
                for y in (0..spec.composition.height).step_by(step_usize) {
                    for x in (0..spec.composition.width).step_by(step_usize) {
                        let out_pixel = out_img.get_pixel(x, y).to_rgba();
                        let t1_pixel = t1_img.get_pixel(x, y).to_rgba();
                        for c in 0..3 {
                            if (out_pixel[c] as i32 - t1_pixel[c] as i32).abs() > 5 {
                                diff_count += 1;
                            }
                        }
                    }
                }
                let diff_ratio = diff_count as f64 / (num_samples * 3.0);
                println!("12_layout_comprehensive stage frame comparison (t={:.2} vs t=1.0): diff_ratio={:.4}", t, diff_ratio);
                assert!(diff_ratio > 0.01, "Layout frame at t={:.2} is identical to t=1.0 (diff_ratio={:.4})", t, diff_ratio);
            }
        }



        // Perform pixel checks
        let mut total_in_luma: u64 = 0;
        let mut total_out_luma: u64 = 0;
        let mut has_color = false;


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
fn test_02_all_filters() {
    run_test_case("02_all_filters.json");
}

#[test]
fn test_03_image_placement() {
    run_test_case("03_image_placement.json");
}

#[test]
fn test_04_transitions() {
    run_test_case("04_transitions.json");
}

#[test]
fn test_05_complex_features() {
    run_test_case("05_complex_features.json");
}

#[test]
fn test_06_movie_audio() {
    run_test_case("06_movie_audio.json");
    
    // Verify audio stream presence using ffprobe
    let output_path = Path::new("tests/fixtures/outputs/06_movie_audio.mp4");
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
fn test_07_ripple_relative() {
    run_test_case("07_ripple_relative.json");
}

#[test]
fn test_08_text_layout() {
    run_test_case("08_text_layout.json");
}

#[test]
fn test_09_tactile_transitions() {
    run_test_case("09_tactile_transitions.json");
}

#[test]
fn test_10_dynamic_transitions() {
    run_test_case("10_dynamic_transitions.json");
}

#[test]
fn test_11_text_transitions() {
    run_test_case("11_text_transitions.json");
}

#[test]
fn test_12_layout_comprehensive() {
    run_test_case("12_layout_comprehensive.json");
}

#[test]
fn test_13_remote_assets() {
    run_test_case("13_remote_assets.json");
    
    // Verify audio stream presence using ffprobe
    let output_path = Path::new("tests/fixtures/outputs/13_remote_assets.mp4");
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
fn test_14_trim_start() {
    run_test_case("14_trim_start.json");
    
    // Verify audio stream presence using ffprobe
    let output_path = Path::new("tests/fixtures/outputs/14_trim_start.mp4");
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
#[ignore] // Requires network access — makes a real (failing) S3 request with fake credentials.
// Run explicitly with: cargo test test_15_remote_upload -- --ignored
fn test_15_remote_upload() {
    let spec_path = "tests/fixtures/15_remote_upload.json";
    let binary_path = env!("CARGO_BIN_EXE_render-poc");
    let debug_base_dir = "tests/fixtures/outputs/debug/15_remote_upload";

    println!("Running binary: {} with spec: {} and debug dir: {}", binary_path, spec_path, debug_base_dir);

    // Clean up old debug folder if it exists
    let debug_base_path = Path::new(&debug_base_dir);
    if debug_base_path.exists() {
        std::fs::remove_dir_all(debug_base_path).expect("Failed to clean up old debug base directory");
    }

    // Execute the rendering binary
    let output = Command::new(binary_path)
        .arg("-i")
        .arg(&spec_path)
        .arg("--debug")
        .arg(&debug_base_dir)
        .output()
        .expect("Failed to run rendering binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined_output = format!("STDOUT:\n{}\nSTDERR:\n{}", stdout, stderr);
    println!("Binary combined output:\n{}", combined_output);

    // The upload MUST fail because of the fake credentials (InvalidAccessKeyId, 403, or invalid S3 connection)
    assert!(!output.status.success(), "Binary execution should have failed due to fake credentials/bucket");

    // We expect the log/stderr/stdout to indicate a failed upload
    assert!(
        combined_output.contains("Upload failed") || combined_output.contains("S3 PUT failed") || combined_output.contains("panic"),
        "The output did not contain expected upload failure message. Output:\n{}",
        combined_output
    );

    println!("test_15_remote_upload successfully verified end-to-end flow attempted S3 upload and failed as expected.");
}






