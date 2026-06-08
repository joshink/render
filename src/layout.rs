use std::collections::HashMap;
use crate::config::{LayoutNode, evaluate_float, evaluate_vec4, evaluate_padding};

// ---------------------------------------------------------------------------
// Layout data types
// ---------------------------------------------------------------------------

/// Cross-axis alignment for stack layouts and text.
#[derive(Clone, Debug, PartialEq)]
pub enum Alignment {
    Left,
    Center,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Alignment {
    /// Parses an alignment string (case-insensitive) into an enum variant,
    /// falling back to `default` for unrecognised values.
    pub fn from_str_or(s: &str, default: Alignment) -> Alignment {
        match s.to_ascii_lowercase().as_str() {
            "left" => Alignment::Left,
            "center" => Alignment::Center,
            "right" => Alignment::Right,
            "top" => Alignment::Top,
            "bottom" => Alignment::Bottom,
            "top_left" | "top-left" | "topleft" => Alignment::TopLeft,
            "top_right" | "top-right" | "topright" => Alignment::TopRight,
            "bottom_left" | "bottom-left" | "bottomleft" => Alignment::BottomLeft,
            "bottom_right" | "bottom-right" | "bottomright" => Alignment::BottomRight,
            other => {
                log::warn!("Unknown alignment '{}', using default", other);
                default
            }
        }
    }
}

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
        alignment: Alignment,
        children: Vec<ResolvedNode>,
    },
    HStack {
        spacing: f32,
        alignment: Alignment,
        children: Vec<ResolvedNode>,
    },
    ZStack {
        alignment: Alignment,
        children: Vec<ResolvedNode>,
    },
    Spacer {
        size: Option<f32>,
    },
    Text {
        font: String,
        font_size: f32,
        color: [f32; 4],
        axes: HashMap<String, f32>,
        alignment: Alignment,
        lines: Vec<String>,    // pre-calculated wrapped lines
        ascent: f32,           // font ascent in pixels
        line_height: f32,      // font line height in pixels
        normalized_coords: Vec<swash::NormalizedCoord>, // cached variation coords
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

// ---------------------------------------------------------------------------
// Resolution (JSON → layout tree)
// ---------------------------------------------------------------------------

/// Recursively resolves child layout nodes for a given timestamp.
fn resolve_children(
    node: &LayoutNode,
    clip_time: f32,
    duration: f32,
    comp_width: u32,
    comp_height: u32,
) -> Vec<ResolvedNode> {
    node.children
        .as_ref()
        .map(|list| {
            list.iter()
                .map(|child| resolve_layout_node(child, clip_time, duration, comp_width, comp_height))
                .collect()
        })
        .unwrap_or_default()
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

    let node_type_lower = node.r#type.to_ascii_lowercase();
    let resolved_type = match node_type_lower.as_str() {
        "vstack" => {
            let spacing = node
                .spacing
                .as_ref()
                .map(|v| evaluate_float(v, clip_time, duration, comp_width, comp_height, 0.0))
                .unwrap_or(0.0);
            let alignment = node
                .alignment
                .as_deref()
                .map(|s| Alignment::from_str_or(s, Alignment::Center))
                .unwrap_or(Alignment::Center);
            let children = resolve_children(node, clip_time, duration, comp_width, comp_height);
            ResolvedNodeType::VStack {
                spacing,
                alignment,
                children,
            }
        }
        "hstack" => {
            let spacing = node
                .spacing
                .as_ref()
                .map(|v| evaluate_float(v, clip_time, duration, comp_width, comp_height, 0.0))
                .unwrap_or(0.0);
            let alignment = node
                .alignment
                .as_deref()
                .map(|s| Alignment::from_str_or(s, Alignment::Center))
                .unwrap_or(Alignment::Center);
            let children = resolve_children(node, clip_time, duration, comp_width, comp_height);
            ResolvedNodeType::HStack {
                spacing,
                alignment,
                children,
            }
        }
        "zstack" => {
            let alignment = node
                .alignment
                .as_deref()
                .map(|s| Alignment::from_str_or(s, Alignment::Center))
                .unwrap_or(Alignment::Center);
            let children = resolve_children(node, clip_time, duration, comp_width, comp_height);
            ResolvedNodeType::ZStack {
                alignment,
                children,
            }
        }
        "spacer" => {
            let size = node
                .size
                .as_ref()
                .map(|v| evaluate_float(v, clip_time, duration, comp_width, comp_height, 0.0));
            ResolvedNodeType::Spacer { size }
        }
        "text" => {
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
            let alignment = node
                .alignment
                .as_deref()
                .map(|s| Alignment::from_str_or(s, Alignment::Left))
                .unwrap_or(Alignment::Left);

            ResolvedNodeType::Text {
                font,
                font_size,
                color,
                axes: resolved_axes,
                alignment,
                lines: if text_str.is_empty() { Vec::new() } else { vec![text_str] },
                ascent: 0.0,
                line_height: 0.0,
                normalized_coords: Vec::new(),
            }
        }
        other => {
            log::error!("Unknown LayoutNode type: '{}', treating as zero-size spacer", other);
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

// ---------------------------------------------------------------------------
// Measure / Arrange / Rasterize
// ---------------------------------------------------------------------------

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
                font: font_id,
                font_size,
                axes,
                lines,
                ascent,
                line_height,
                normalized_coords,
                ..
            } => {
                // `lines` was seeded with the raw text in resolve; extract it
                // for wrapping, then replace with the wrapped result.
                let raw_text = lines.first().cloned().unwrap_or_default();
                let (w, h, wrapped_lines) =
                    measure_text(&raw_text, font_id, *font_size, axes, content_max_w, font_assets);
                *lines = wrapped_lines;

                // Get line metrics from the font and cache normalized coords
                let mut font_ascent = *font_size * 0.8;
                let mut font_line_h = *font_size * 1.2;
                let mut coords = Vec::new();
                if let Some(font_bytes) = font_assets.get(font_id) {
                    if let Some(font_ref) = swash::FontRef::from_index(font_bytes, 0) {
                        coords = compute_normalized_coords(&font_ref, axes);
                        let metrics = font_ref.metrics(&coords);
                        let scale = *font_size / metrics.units_per_em as f32;
                        font_ascent = metrics.ascent * scale;
                        // descent is negative in OpenType; subtract to get the
                        // full line-to-line distance.
                        font_line_h = (metrics.ascent - metrics.descent + metrics.leading) * scale;
                    }
                }
                *ascent = font_ascent;
                *line_height = font_line_h;
                *normalized_coords = coords;

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
                // Compute space consumed by fixed (non-flexible) children plus
                // inter-child gaps. Flexible spacers share the remainder.
                let mut fixed_h = 0.0f32;
                let mut flex_count = 0u32;
                for child in children.iter() {
                    if is_flexible_spacer(child) {
                        flex_count += 1;
                    } else {
                        fixed_h += child.measured_size.height;
                    }
                }
                // N children ⇒ N-1 spacing gaps (between every pair, whether
                // or not the child is flexible).
                let gap_count = children.len().saturating_sub(1);
                fixed_h += gap_count as f32 * *spacing;

                let remaining_h = (content_h - fixed_h).max(0.0);
                let spacer_h = if flex_count > 0 {
                    remaining_h / flex_count as f32
                } else {
                    0.0
                };

                let mut current_y = content_y;
                let last_idx = children.len().saturating_sub(1);
                for (idx, child) in children.iter_mut().enumerate() {
                    let child_h = if is_flexible_spacer(child) {
                        spacer_h
                    } else {
                        child.measured_size.height
                    };
                    let child_w = child.measured_size.width.min(content_w);

                    let child_x = match alignment {
                        Alignment::Left => content_x,
                        Alignment::Right => content_x + content_w - child_w,
                        _ => content_x + (content_w - child_w) * 0.5, // center
                    };

                    child.arrange(child_x, current_y, child_w, child_h);
                    current_y += child_h;
                    if idx < last_idx {
                        current_y += *spacing;
                    }
                }
            }
            ResolvedNodeType::HStack {
                spacing,
                alignment,
                children,
            } => {
                let mut fixed_w = 0.0f32;
                let mut flex_count = 0u32;
                for child in children.iter() {
                    if is_flexible_spacer(child) {
                        flex_count += 1;
                    } else {
                        fixed_w += child.measured_size.width;
                    }
                }
                let gap_count = children.len().saturating_sub(1);
                fixed_w += gap_count as f32 * *spacing;

                let remaining_w = (content_w - fixed_w).max(0.0);
                let spacer_w = if flex_count > 0 {
                    remaining_w / flex_count as f32
                } else {
                    0.0
                };

                let mut current_x = content_x;
                let last_idx = children.len().saturating_sub(1);
                for (idx, child) in children.iter_mut().enumerate() {
                    let child_w = if is_flexible_spacer(child) {
                        spacer_w
                    } else {
                        child.measured_size.width
                    };
                    let child_h = child.measured_size.height.min(content_h);

                    let child_y = match alignment {
                        Alignment::Top => content_y,
                        Alignment::Bottom => content_y + content_h - child_h,
                        _ => content_y + (content_h - child_h) * 0.5, // center
                    };

                    child.arrange(current_x, child_y, child_w, child_h);
                    current_x += child_w;
                    if idx < last_idx {
                        current_x += *spacing;
                    }
                }
            }
            ResolvedNodeType::ZStack {
                alignment,
                children,
            } => {
                for child in children {
                    let child_w = child.measured_size.width.min(content_w);
                    let child_h = child.measured_size.height.min(content_h);

                    let (child_x, child_y) = match alignment {
                        Alignment::TopLeft => (content_x, content_y),
                        Alignment::TopRight => (content_x + content_w - child_w, content_y),
                        Alignment::BottomLeft => (content_x, content_y + content_h - child_h),
                        Alignment::BottomRight => (content_x + content_w - child_w, content_y + content_h - child_h),
                        Alignment::Top => (content_x + (content_w - child_w) * 0.5, content_y),
                        Alignment::Bottom => (content_x + (content_w - child_w) * 0.5, content_y + content_h - child_h),
                        Alignment::Left => (content_x, content_y + (content_h - child_h) * 0.5),
                        Alignment::Right => (content_x + content_w - child_w, content_y + (content_h - child_h) * 0.5),
                        Alignment::Center => (
                            content_x + (content_w - child_w) * 0.5,
                            content_y + (content_h - child_h) * 0.5,
                        ),
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
                font: font_id,
                font_size,
                color,
                axes,
                alignment,
                lines,
                ascent,
                line_height,
                normalized_coords,
                ..
            } => {
                if lines.is_empty() {
                    return;
                }

                let Some(font_bytes) = font_assets.get(font_id) else {
                    return;
                };
                let Some(font) = swash::FontRef::from_index(font_bytes, 0) else {
                    return;
                };

                // Use cached normalized coords if available, otherwise recompute
                let owned_coords;
                let coords: &[swash::NormalizedCoord] = if !normalized_coords.is_empty() {
                    normalized_coords.as_slice()
                } else {
                    owned_coords = compute_normalized_coords(&font, axes);
                    &owned_coords
                };

                let charmap = font.charmap();
                let glyph_metrics = font.glyph_metrics(coords);
                let font_metrics = font.metrics(coords);
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

                    // Measure line width for alignment
                    let mut line_w = 0.0;
                    for c in line_str.chars() {
                        let gid = charmap.map(c);
                        line_w += glyph_metrics.advance_width(gid) * scale_factor;
                    }

                    let mut pen_x = match alignment {
                        Alignment::Right => cx + cw - line_w,
                        Alignment::Center => cx + (cw - line_w) * 0.5,
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

                            let gw = g_img.placement.width as i32;
                            let gh = g_img.placement.height as i32;

                            // Detect color vs mask glyph by data length.
                            // Mask glyphs have 1 byte/pixel (w*h bytes total),
                            // color glyphs have 4 bytes/pixel (w*h*4 bytes).
                            let data = &g_img.data;
                            let expected_mask_len = (gw * gh) as usize;
                            let is_color = data.len() >= expected_mask_len * 4 && expected_mask_len > 0;

                            for my in 0..gh {
                                for mx in 0..gw {
                                    let px = gx + mx;
                                    let py = gy + my;

                                    if px < 0 || px >= dest_w || py < 0 || py >= dest_h {
                                        continue;
                                    }

                                    let (src_r, src_g, src_b, src_a) = if is_color {
                                        // RGBA color glyph: 4 bytes per pixel
                                        let base = (my * gw + mx) as usize * 4;
                                        if base + 3 >= data.len() {
                                            continue;
                                        }
                                        (
                                            data[base] as f32 / 255.0,
                                            data[base + 1] as f32 / 255.0,
                                            data[base + 2] as f32 / 255.0,
                                            data[base + 3] as f32 / 255.0 * color[3],
                                        )
                                    } else {
                                        // Alpha mask: 1 byte per pixel, tinted by text color
                                        let mask_val = data[(my * gw + mx) as usize];
                                        if mask_val == 0 {
                                            continue;
                                        }
                                        let alpha = mask_val as f32 / 255.0 * color[3];
                                        (color[0], color[1], color[2], alpha)
                                    };

                                    if src_a <= 0.0 {
                                        continue;
                                    }

                                    // Porter-Duff source-over compositing
                                    let existing_pixel = dest.get_pixel(px as u32, py as u32);
                                    let dr = existing_pixel[0] as f32 / 255.0;
                                    let dg = existing_pixel[1] as f32 / 255.0;
                                    let db = existing_pixel[2] as f32 / 255.0;
                                    let da = existing_pixel[3] as f32 / 255.0;

                                    let out_a = src_a + da * (1.0 - src_a);
                                    if out_a > 0.0 {
                                        let out_r = (src_r * src_a + dr * da * (1.0 - src_a)) / out_a;
                                        let out_g = (src_g * src_a + dg * da * (1.0 - src_a)) / out_a;
                                        let out_b = (src_b * src_a + db * da * (1.0 - src_a)) / out_a;

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

                        pen_x += advance;
                    }
                }
            }
        }
    }
}

fn is_flexible_spacer(node: &ResolvedNode) -> bool {
    matches!(&node.r#type, ResolvedNodeType::Spacer { size: None })
}

// ---------------------------------------------------------------------------
// Text measurement / word-wrapping
// ---------------------------------------------------------------------------

/// Measures text and returns `(width, height, wrapped_lines)`.
///
/// Uses glyph-level metrics from the loaded font when available, falling back
/// to a monospace approximation when the font asset is missing.  Both paths
/// use `split_whitespace()` for consistent whitespace normalisation (collapses
/// runs of spaces, trims leading/trailing whitespace).
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

    let Some(font_bytes) = font_assets.get(font_id) else {
        // Fallback: estimate with monospace approximation
        let char_w = font_size * 0.5;
        let line_h = font_size * 1.2;
        let (max_w, lines) = wrap_paragraphs_approx(text, char_w, max_width);
        let total_h = lines.len() as f32 * line_h;
        return (max_w, total_h, lines);
    };

    let Some(font) = swash::FontRef::from_index(font_bytes, 0) else {
        return (0.0, 0.0, Vec::new());
    };

    let coords = compute_normalized_coords(&font, axes);
    let charmap = font.charmap();
    let glyph_metrics = font.glyph_metrics(&coords);
    let font_metrics = font.metrics(&coords);
    let scale_factor = font_size / font_metrics.units_per_em as f32;
    // descent is negative in OpenType; subtract to get the full line height.
    let line_height_px =
        (font_metrics.ascent - font_metrics.descent + font_metrics.leading) * scale_factor;

    // Pre-compute the space advance (constant for the entire text block)
    let space_gid = charmap.map(' ');
    let space_w = glyph_metrics.advance_width(space_gid) * scale_factor;

    let paragraphs: Vec<&str> = text.split('\n').collect();
    let mut final_lines = Vec::new();
    let mut max_observed_w: f32 = 0.0;

    for para in paragraphs {
        let mut current_line = String::new();
        let mut current_w = 0.0;

        for word in para.split_whitespace() {
            let mut word_w = 0.0;
            for c in word.chars() {
                let gid = charmap.map(c);
                word_w += glyph_metrics.advance_width(gid) * scale_factor;
            }

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

/// Word-wrapping helper for the fallback (no font loaded) path.
///
/// Splits on `\n` for paragraphs, then `split_whitespace()` within each
/// paragraph for consistent whitespace handling with the glyph-aware path.
fn wrap_paragraphs_approx(text: &str, char_w: f32, max_width: f32) -> (f32, Vec<String>) {
    let mut lines = Vec::new();
    let mut max_observed_w: f32 = 0.0;

    for para in text.split('\n') {
        let mut current_line = String::new();
        let mut current_w = 0.0;

        for word in para.split_whitespace() {
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
    }

    (max_observed_w, lines)
}
