use std::collections::HashMap;
use crate::config::{LayoutNode, evaluate_float, evaluate_vec4, evaluate_padding};

/// Fully evaluated layout tree for a specific frame time.
pub struct ResolvedNode {
    pub r#type: ResolvedNodeType,
    pub padding: [f32; 4], // [top, right, bottom, left]
    pub frame: Rect,       // calculated outer rect (x, y, width, height) relative to parent
    pub measured_size: Size, // preferred outer size of this node
}

pub enum ResolvedNodeType {
    VStack {
        spacing: f32,
        alignment: String, // "left", "center", "right"
        children: Vec<ResolvedNode>,
    },
    HStack {
        spacing: f32,
        alignment: String, // "top", "center", "bottom"
        children: Vec<ResolvedNode>,
    },
    ZStack {
        alignment: String, // "center", "top_left", etc.
        children: Vec<ResolvedNode>,
    },
    Spacer {
        size: Option<f32>,
    },
    Text {
        text: String,
        font: String,
        font_size: f32,
        color: [f32; 4],
        axes: HashMap<String, f32>,
        alignment: String, // "left", "center", "right"
        lines: Vec<String>, // pre-calculated wrapped lines
        ascent: f32,       // font ascent
        line_height: f32,  // font line height
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// Computes normalized coordinates for OpenType variation axes.
pub fn compute_normalized_coords(
    font: &swash::FontRef,
    axes_map: &HashMap<String, f32>,
) -> Vec<swash::NormalizedCoord> {
    let variations_iter = font.variations();
    let mut coords = vec![0i16; variations_iter.len()];
    for (i, axis) in font.variations().enumerate() {
        let tag_bytes = axis.tag().to_be_bytes();
        let tag_str = String::from_utf8_lossy(&tag_bytes).to_string();
        let user_val = axes_map.get(&tag_str).copied().unwrap_or(axis.default_value());
        let clamped = user_val.clamp(axis.min_value(), axis.max_value());

        let f = if clamped == axis.default_value() {
            0.0
        } else if clamped < axis.default_value() {
            let denom = axis.default_value() - axis.min_value();
            if denom > 0.0 {
                (clamped - axis.default_value()) / denom
            } else {
                0.0
            }
        } else {
            let denom = axis.max_value() - axis.default_value();
            if denom > 0.0 {
                (clamped - axis.default_value()) / denom
            } else {
                0.0
            }
        };
        coords[i] = (f * 16384.0).round().clamp(-16384.0, 16383.0) as i16;
    }
    coords
}

/// Converts a string into a swash::Tag.
pub fn to_swash_tag(s: &str) -> swash::Tag {
    let mut bytes = [0u8; 4];
    let s_bytes = s.as_bytes();
    for i in 0..4 {
        if i < s_bytes.len() {
            bytes[i] = s_bytes[i];
        } else {
            bytes[i] = b' ';
        }
    }
    u32::from_be_bytes(bytes)
}

/// Recursively resolves raw layout nodes to resolved layout nodes for a given timestamp.
pub fn resolve_layout_node(
    node: &LayoutNode,
    clip_time: f32,
    duration: f32,
    comp_width: u32,
    comp_height: u32,
) -> ResolvedNode {
    let padding = node
        .padding
        .as_ref()
        .map(|v| {
            evaluate_padding(
                v,
                clip_time,
                duration,
                comp_width,
                comp_height,
                [0.0, 0.0, 0.0, 0.0],
            )
        })
        .unwrap_or([0.0, 0.0, 0.0, 0.0]);

    let resolved_type = match node.r#type.as_str() {
        "vstack" | "VStack" => {
            let spacing = node
                .spacing
                .as_ref()
                .map(|v| evaluate_float(v, clip_time, duration, comp_width, comp_height, 0.0))
                .unwrap_or(0.0);
            let alignment = node.alignment.clone().unwrap_or_else(|| "center".to_string());
            let children = node
                .children
                .as_ref()
                .map(|list| {
                    list.iter()
                        .map(|child| {
                            resolve_layout_node(
                                child,
                                clip_time,
                                duration,
                                comp_width,
                                comp_height,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            ResolvedNodeType::VStack {
                spacing,
                alignment,
                children,
            }
        }
        "hstack" | "HStack" => {
            let spacing = node
                .spacing
                .as_ref()
                .map(|v| evaluate_float(v, clip_time, duration, comp_width, comp_height, 0.0))
                .unwrap_or(0.0);
            let alignment = node.alignment.clone().unwrap_or_else(|| "center".to_string());
            let children = node
                .children
                .as_ref()
                .map(|list| {
                    list.iter()
                        .map(|child| {
                            resolve_layout_node(
                                child,
                                clip_time,
                                duration,
                                comp_width,
                                comp_height,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            ResolvedNodeType::HStack {
                spacing,
                alignment,
                children,
            }
        }
        "zstack" | "ZStack" => {
            let alignment = node.alignment.clone().unwrap_or_else(|| "center".to_string());
            let children = node
                .children
                .as_ref()
                .map(|list| {
                    list.iter()
                        .map(|child| {
                            resolve_layout_node(
                                child,
                                clip_time,
                                duration,
                                comp_width,
                                comp_height,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            ResolvedNodeType::ZStack {
                alignment,
                children,
            }
        }
        "spacer" | "Spacer" => {
            let size = node
                .size
                .as_ref()
                .map(|v| evaluate_float(v, clip_time, duration, comp_width, comp_height, 0.0));
            ResolvedNodeType::Spacer { size }
        }
        "text" | "Text" => {
            let text_str = match &node.text {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(val) => val.to_string(),
                None => "".to_string(),
            };
            let font = node.font.clone().unwrap_or_default();
            let font_size = node
                .font_size
                .as_ref()
                .map(|v| evaluate_float(v, clip_time, duration, comp_width, comp_height, 16.0))
                .unwrap_or(16.0);
            let color = node
                .color
                .as_ref()
                .map(|v| {
                    evaluate_vec4(
                        v,
                        clip_time,
                        duration,
                        comp_width,
                        comp_height,
                        [1.0, 1.0, 1.0, 1.0],
                    )
                })
                .unwrap_or([1.0, 1.0, 1.0, 1.0]);

            let mut resolved_axes = HashMap::new();
            if let Some(ref axes_map) = node.axes {
                for (k, v) in axes_map {
                    let axis_val =
                        evaluate_float(v, clip_time, duration, comp_width, comp_height, 0.0);
                    resolved_axes.insert(k.clone(), axis_val);
                }
            }
            let alignment = node.alignment.clone().unwrap_or_else(|| "left".to_string());

            ResolvedNodeType::Text {
                text: text_str,
                font,
                font_size,
                color,
                axes: resolved_axes,
                alignment,
                lines: Vec::new(),
                ascent: 0.0,
                line_height: 0.0,
            }
        }
        other => {
            log::warn!("Unknown LayoutNode type: {}", other);
            ResolvedNodeType::Spacer { size: Some(0.0) }
        }
    };

    ResolvedNode {
        r#type: resolved_type,
        padding,
        frame: Rect::default(),
        measured_size: Size::default(),
    }
}

impl ResolvedNode {
    /// Measure Pass: Recursively determines the preferred size of this node.
    pub fn measure(
        &mut self,
        max_width: f32,
        max_height: f32,
        font_assets: &HashMap<String, Vec<u8>>,
    ) -> Size {
        let v_pad = self.padding[0] + self.padding[2];
        let h_pad = self.padding[1] + self.padding[3];
        let content_max_w = (max_width - h_pad).max(0.0);
        let content_max_h = (max_height - v_pad).max(0.0);

        let size = match &mut self.r#type {
            ResolvedNodeType::Spacer { size } => {
                let sz = size.unwrap_or(0.0);
                Size {
                    width: sz,
                    height: sz,
                }
            }
            ResolvedNodeType::Text {
                text,
                font: font_id,
                font_size,
                axes,
                lines,
                ascent,
                line_height,
                ..
            } => {
                let (w, h, wrapped_lines) =
                    measure_text(text, font_id, *font_size, axes, content_max_w, font_assets);
                *lines = wrapped_lines;
                
                // Get line metrics
                let mut font_ascent = *font_size * 0.8;
                let mut font_line_h = *font_size * 1.2;
                if let Some(font_bytes) = font_assets.get(font_id) {
                    if let Some(font_ref) = swash::FontRef::from_index(font_bytes, 0) {
                        let coords = compute_normalized_coords(&font_ref, axes);
                        let metrics = font_ref.metrics(&coords);
                        let scale = *font_size / metrics.units_per_em as f32;
                        font_ascent = metrics.ascent * scale;
                        font_line_h = (metrics.ascent + metrics.descent + metrics.leading) * scale;
                    }
                }
                *ascent = font_ascent;
                *line_height = font_line_h;

                Size {
                    width: w + h_pad,
                    height: h + v_pad,
                }
            }
            ResolvedNodeType::VStack {
                spacing,
                children,
                ..
            } => {
                let mut h_max = 0.0f32;
                let mut v_sum = 0.0f32;
                for (idx, child) in children.iter_mut().enumerate() {
                    let child_size = child.measure(content_max_w, content_max_h, font_assets);
                    h_max = h_max.max(child_size.width);
                    v_sum += child_size.height;
                    if idx > 0 {
                        v_sum += *spacing;
                    }
                }
                Size {
                    width: h_max + h_pad,
                    height: v_sum + v_pad,
                }
            }
            ResolvedNodeType::HStack {
                spacing,
                children,
                ..
            } => {
                let mut h_sum = 0.0f32;
                let mut v_max = 0.0f32;
                for (idx, child) in children.iter_mut().enumerate() {
                    let child_size = child.measure(content_max_w, content_max_h, font_assets);
                    h_sum += child_size.width;
                    v_max = v_max.max(child_size.height);
                    if idx > 0 {
                        h_sum += *spacing;
                    }
                }
                Size {
                    width: h_sum + h_pad,
                    height: v_max + v_pad,
                }
            }
            ResolvedNodeType::ZStack { children, .. } => {
                let mut h_max = 0.0f32;
                let mut v_max = 0.0f32;
                for child in children {
                    let child_size = child.measure(content_max_w, content_max_h, font_assets);
                    h_max = h_max.max(child_size.width);
                    v_max = v_max.max(child_size.height);
                }
                Size {
                    width: h_max + h_pad,
                    height: v_max + v_pad,
                }
            }
        };

        self.measured_size = size;
        size
    }

    /// Arrange Pass: Position each node within the parent bounds.
    pub fn arrange(&mut self, x: f32, y: f32, width: f32, height: f32) {
        self.frame = Rect { x, y, width, height };

        let content_x = x + self.padding[3];
        let content_y = y + self.padding[0];
        let content_w = (width - (self.padding[1] + self.padding[3])).max(0.0);
        let content_h = (height - (self.padding[0] + self.padding[2])).max(0.0);

        match &mut self.r#type {
            ResolvedNodeType::Spacer { .. } | ResolvedNodeType::Text { .. } => {}
            ResolvedNodeType::VStack {
                spacing,
                alignment,
                children,
            } => {
                let mut fixed_h = 0.0f32;
                let mut flex_count = 0;
                for (idx, child) in children.iter().enumerate() {
                    if is_flexible_spacer(child) {
                        flex_count += 1;
                    } else {
                        fixed_h += child.measured_size.height;
                    }
                    if idx > 0 {
                        fixed_h += *spacing;
                    }
                }

                let remaining_h = (content_h - fixed_h).max(0.0);
                let spacer_h = if flex_count > 0 {
                    remaining_h / flex_count as f32
                } else {
                    0.0
                };

                let mut current_y = content_y;
                for child in children {
                    let child_h = if is_flexible_spacer(child) {
                        spacer_h
                    } else {
                        child.measured_size.height
                    };
                    let child_w = child.measured_size.width.min(content_w);

                    let child_x = match alignment.as_str() {
                        "left" => content_x,
                        "right" => content_x + content_w - child_w,
                        _ => content_x + (content_w - child_w) * 0.5, // center
                    };

                    child.arrange(child_x, current_y, child_w, child_h);
                    current_y += child_h + *spacing;
                }
            }
            ResolvedNodeType::HStack {
                spacing,
                alignment,
                children,
            } => {
                let mut fixed_w = 0.0f32;
                let mut flex_count = 0;
                for (idx, child) in children.iter().enumerate() {
                    if is_flexible_spacer(child) {
                        flex_count += 1;
                    } else {
                        fixed_w += child.measured_size.width;
                    }
                    if idx > 0 {
                        fixed_w += *spacing;
                    }
                }

                let remaining_w = (content_w - fixed_w).max(0.0);
                let spacer_w = if flex_count > 0 {
                    remaining_w / flex_count as f32
                } else {
                    0.0
                };

                let mut current_x = content_x;
                for child in children {
                    let child_w = if is_flexible_spacer(child) {
                        spacer_w
                    } else {
                        child.measured_size.width
                    };
                    let child_h = child.measured_size.height.min(content_h);

                    let child_y = match alignment.as_str() {
                        "top" => content_y,
                        "bottom" => content_y + content_h - child_h,
                        _ => content_y + (content_h - child_h) * 0.5, // center
                    };

                    child.arrange(current_x, child_y, child_w, child_h);
                    current_x += child_w + *spacing;
                }
            }
            ResolvedNodeType::ZStack {
                alignment,
                children,
            } => {
                for child in children {
                    let child_w = child.measured_size.width.min(content_w);
                    let child_h = child.measured_size.height.min(content_h);

                    let (child_x, child_y) = match alignment.as_str() {
                        "top_left" | "top-left" => (content_x, content_y),
                        "top_right" | "top-right" => (content_x + content_w - child_w, content_y),
                        "bottom_left" | "bottom-left" => (content_x, content_y + content_h - child_h),
                        "bottom_right" | "bottom-right" => (content_x + content_w - child_w, content_y + content_h - child_h),
                        "top" => (content_x + (content_w - child_w) * 0.5, content_y),
                        "bottom" => (content_x + (content_w - child_w) * 0.5, content_y + content_h - child_h),
                        "left" => (content_x, content_y + (content_h - child_h) * 0.5),
                        "right" => (content_x + content_w - child_w, content_y + (content_h - child_h) * 0.5),
                        _ => (
                            content_x + (content_w - child_w) * 0.5,
                            content_y + (content_h - child_h) * 0.5,
                        ), // center
                    };

                    child.arrange(child_x, child_y, child_w, child_h);
                }
            }
        }
    }

    /// Rasterize Pass: Render all text nodes onto the output RGBA buffer.
    pub fn rasterize(
        &self,
        dest: &mut image::RgbaImage,
        font_assets: &HashMap<String, Vec<u8>>,
    ) {
        match &self.r#type {
            ResolvedNodeType::Spacer { .. } => {}
            ResolvedNodeType::VStack { children, .. }
            | ResolvedNodeType::HStack { children, .. }
            | ResolvedNodeType::ZStack { children, .. } => {
                for child in children {
                    child.rasterize(dest, font_assets);
                }
            }
            ResolvedNodeType::Text {
                text,
                font: font_id,
                font_size,
                color,
                axes,
                alignment,
                lines,
                ascent,
                line_height,
            } => {
                if text.is_empty() || lines.is_empty() {
                    return;
                }
                
                let font_bytes = font_assets.get(font_id);
                if font_bytes.is_none() {
                    return;
                }
                let font_bytes = font_bytes.unwrap();
                let font = match swash::FontRef::from_index(font_bytes, 0) {
                    Some(f) => f,
                    None => return,
                };

                let coords = compute_normalized_coords(&font, axes);
                let charmap = font.charmap();
                let glyph_metrics = font.glyph_metrics(&coords);
                let font_metrics = font.metrics(&coords);
                let scale_factor = *font_size / font_metrics.units_per_em as f32;

                let mut scale_context = swash::scale::ScaleContext::new();
                let mut scaler = scale_context
                    .builder(font)
                    .size(*font_size)
                    .hint(true)
                    .variations(axes.iter().map(|(k, &v)| (to_swash_tag(k), v)))
                    .build();

                let cx = self.frame.x + self.padding[3];
                let cy = self.frame.y + self.padding[0];
                let cw = self.frame.width - (self.padding[1] + self.padding[3]);
                let dest_w = dest.width() as i32;
                let dest_h = dest.height() as i32;

                for (line_idx, line_str) in lines.iter().enumerate() {
                    let line_y = cy + line_idx as f32 * *line_height + *ascent;
                    
                    // Measure line width to align
                    let mut line_w = 0.0;
                    for c in line_str.chars() {
                        let gid = charmap.map(c);
                        line_w += glyph_metrics.advance_width(gid) * scale_factor;
                    }

                    let mut pen_x = match alignment.as_str() {
                        "right" => cx + cw - line_w,
                        "center" => cx + (cw - line_w) * 0.5,
                        _ => cx, // left
                    };

                    for c in line_str.chars() {
                        let gid = charmap.map(c);
                        let advance = glyph_metrics.advance_width(gid) * scale_factor;

                        let render_img = swash::scale::Render::new(&[
                            swash::scale::Source::ColorOutline(0),
                            swash::scale::Source::Outline,
                        ])
                        .render(&mut scaler, gid);

                        if let Some(g_img) = render_img {
                            let gx = (pen_x + g_img.placement.left as f32).round() as i32;
                            let gy = (line_y - g_img.placement.top as f32).round() as i32;
                            
                            let mask = &g_img.data;
                            let gw = g_img.placement.width as i32;
                            let gh = g_img.placement.height as i32;

                            for my in 0..gh {
                                for mx in 0..gw {
                                    let px = gx + mx;
                                    let py = gy + my;

                                    if px >= 0 && px < dest_w && py >= 0 && py < dest_h {
                                        let mask_val = mask[(my * gw + mx) as usize];
                                        if mask_val > 0 {
                                            let alpha = mask_val as f32 / 255.0 * color[3];
                                            let existing_pixel = dest.get_pixel(px as u32, py as u32);
                                            let dr = existing_pixel[0] as f32 / 255.0;
                                            let dg = existing_pixel[1] as f32 / 255.0;
                                            let db = existing_pixel[2] as f32 / 255.0;
                                            let da = existing_pixel[3] as f32 / 255.0;

                                            let out_a = alpha + da * (1.0 - alpha);
                                            if out_a > 0.0 {
                                                let out_r = (color[0] * alpha + dr * da * (1.0 - alpha)) / out_a;
                                                let out_g = (color[1] * alpha + dg * da * (1.0 - alpha)) / out_a;
                                                let out_b = (color[2] * alpha + db * da * (1.0 - alpha)) / out_a;

                                                dest.put_pixel(
                                                    px as u32,
                                                    py as u32,
                                                    image::Rgba([
                                                        (out_r * 255.0).round().clamp(0.0, 255.0) as u8,
                                                        (out_g * 255.0).round().clamp(0.0, 255.0) as u8,
                                                        (out_b * 255.0).round().clamp(0.0, 255.0) as u8,
                                                        (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
                                                    ]),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        pen_x += advance;
                    }
                }
            }
        }
    }
}

fn is_flexible_spacer(node: &ResolvedNode) -> bool {
    match &node.r#type {
        ResolvedNodeType::Spacer { size } => size.is_none(),
        _ => false,
    }
}

/// Helper function to measure text wrapping.
fn measure_text(
    text: &str,
    font_id: &str,
    font_size: f32,
    axes: &HashMap<String, f32>,
    max_width: f32,
    font_assets: &HashMap<String, Vec<u8>>,
) -> (f32, f32, Vec<String>) {
    if text.is_empty() {
        return (0.0, 0.0, Vec::new());
    }

    let font_bytes = font_assets.get(font_id);
    if font_bytes.is_none() {
        // Fallback: estimate
        let char_w = font_size * 0.5;
        let line_h = font_size * 1.2;
        let mut lines = Vec::new();
        let mut current_line = String::new();
        let mut current_w = 0.0;
        let mut max_observed_w: f32 = 0.0;

        for word in text.split_whitespace() {
            let word_w = word.len() as f32 * char_w;
            let space_w = char_w;

            if current_line.is_empty() {
                current_line.push_str(word);
                current_w = word_w;
            } else if current_w + space_w + word_w <= max_width || max_width <= 0.0 {
                current_line.push(' ');
                current_line.push_str(word);
                current_w += space_w + word_w;
            } else {
                lines.push(current_line);
                max_observed_w = max_observed_w.max(current_w);
                current_line = word.to_string();
                current_w = word_w;
            }
        }
        if !current_line.is_empty() {
            lines.push(current_line);
            max_observed_w = max_observed_w.max(current_w);
        }
        let total_h = lines.len() as f32 * line_h;
        return (max_observed_w, total_h, lines);
    }

    let font_bytes = font_bytes.unwrap();
    let font = match swash::FontRef::from_index(font_bytes, 0) {
        Some(f) => f,
        None => return (0.0, 0.0, Vec::new()),
    };

    let coords = compute_normalized_coords(&font, axes);
    let charmap = font.charmap();
    let glyph_metrics = font.glyph_metrics(&coords);
    let font_metrics = font.metrics(&coords);
    let scale_factor = font_size / font_metrics.units_per_em as f32;
    let line_height_px =
        (font_metrics.ascent + font_metrics.descent + font_metrics.leading) * scale_factor;

    let paragraphs: Vec<&str> = text.split('\n').collect();
    let mut final_lines = Vec::new();
    let mut max_observed_w: f32 = 0.0;

    for para in paragraphs {
        let mut current_line = String::new();
        let mut current_w = 0.0;

        let words: Vec<&str> = para.split(' ').collect();
        for (word_idx, &word) in words.iter().enumerate() {
            if word.is_empty() && word_idx > 0 {
                continue;
            }

            let mut word_w = 0.0;
            for c in word.chars() {
                let gid = charmap.map(c);
                word_w += glyph_metrics.advance_width(gid) * scale_factor;
            }

            let space_gid = charmap.map(' ');
            let space_w = glyph_metrics.advance_width(space_gid) * scale_factor;

            if current_line.is_empty() {
                current_line.push_str(word);
                current_w = word_w;
            } else if current_w + space_w + word_w <= max_width || max_width <= 0.0 {
                current_line.push(' ');
                current_line.push_str(word);
                current_w += space_w + word_w;
            } else {
                final_lines.push(current_line);
                max_observed_w = max_observed_w.max(current_w);
                current_line = word.to_string();
                current_w = word_w;
            }
        }
        if !current_line.is_empty() {
            final_lines.push(current_line);
            max_observed_w = max_observed_w.max(current_w);
        }
    }

    let total_h = final_lines.len() as f32 * line_height_px;
    (max_observed_w, total_h, final_lines)
}
