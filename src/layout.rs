use std::collections::HashMap;
use crate::config::{LayoutNode, evaluate_float, evaluate_vec4, evaluate_padding, evaluate_scale_vec2};

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
        let default = axis.default_value();

        let f = if clamped == default {
            0.0
        } else {
            let denom = if clamped < default {
                default - axis.min_value()
            } else {
                axis.max_value() - default
            };
            if denom > 0.0 {
                (clamped - default) / denom
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
        clip_time: f32,
        clip_duration: f32,
        entrance: Option<&crate::config::TextTransition>,
        exit: Option<&crate::config::TextTransition>,
    ) {
        // Pre-pass: count text units
        let mut total_chars = 0;
        let mut total_words = 0;
        let mut total_lines = 0;
        self.count_text_units(&mut total_chars, &mut total_words, &mut total_lines);

        let mut state = RasterizeState {
            font_assets,
            clip_time,
            clip_duration,
            entrance,
            exit,
            char_index: 0,
            word_index: 0,
            line_index: 0,
            total_chars,
            total_words,
            total_lines,
        };

        self.rasterize_rec(dest, &mut state);
    }

    fn rasterize_rec(
        &self,
        dest: &mut image::RgbaImage,
        state: &mut RasterizeState,
    ) {
        match &self.r#type {
            ResolvedNodeType::Spacer { .. } => {}
            ResolvedNodeType::VStack { children, .. }
            | ResolvedNodeType::HStack { children, .. }
            | ResolvedNodeType::ZStack { children, .. } => {
                for child in children {
                    child.rasterize_rec(dest, state);
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

                let Some(font_bytes) = state.font_assets.get(font_id) else {
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

                for (line_idx, line_str) in lines.iter().enumerate() {
                    let current_line_idx = state.line_index;
                    state.line_index += 1;

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

                    let mut in_word = false; // Reset word boundary per line

                    for c in line_str.chars() {
                        // Word boundary detection
                        let is_word_char = !c.is_whitespace();
                        if is_word_char {
                            if !in_word {
                                in_word = true;
                                if state.char_index > 0 || state.word_index > 0 {
                                    state.word_index += 1;
                                }
                            }
                        } else {
                            in_word = false;
                        }

                        let cur_char_idx = state.char_index;
                        if !c.is_whitespace() {
                            state.char_index += 1;
                        }

                        let gid = charmap.map(c);
                        let advance = glyph_metrics.advance_width(gid) * scale_factor;

                        // Space characters don't render, just advance pen
                        if c.is_whitespace() {
                            pen_x += advance;
                            continue;
                        }

                        // Calculate transition progress and parameters for this character
                        let p = get_character_progress(cur_char_idx, state.word_index, current_line_idx, state);

                        let active_start_transform = if state.clip_time < state.clip_duration * 0.5 {
                            state.entrance.and_then(|e| e.start_transform.as_ref())
                        } else {
                            state.exit.and_then(|e| e.start_transform.as_ref())
                        };

                        let start_pos_offset = active_start_transform.and_then(|t| t.position_offset).unwrap_or([0.0, 0.0]);
                        let start_scale = evaluate_scale_vec2(&active_start_transform.and_then(|t| t.scale.clone()), [1.0, 1.0]);
                        let start_rotation = active_start_transform.and_then(|t| t.rotation).unwrap_or(0.0);
                        let start_opacity = active_start_transform.and_then(|t| t.opacity).unwrap_or(0.0);

                        // Interpolate
                        let dx = (1.0 - p) * start_pos_offset[0];
                        let dy = (1.0 - p) * start_pos_offset[1];
                        let sx = start_scale[0] + p * (1.0 - start_scale[0]);
                        let sy = start_scale[1] + p * (1.0 - start_scale[1]);
                        let rot = (1.0 - p) * start_rotation;
                        let alpha = (start_opacity + p * (1.0 - start_opacity)) * color[3];

                        if alpha <= 0.0 {
                            pen_x += advance;
                            continue;
                        }

                        rasterize_glyph(
                            gid,
                            pen_x,
                            line_y,
                            alpha,
                            dx,
                            dy,
                            sx,
                            sy,
                            rot,
                            color,
                            &mut scaler,
                            dest,
                        );

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

fn rasterize_glyph(
    gid: u16,
    pen_x: f32,
    line_y: f32,
    alpha: f32,
    dx: f32,
    dy: f32,
    sx: f32,
    sy: f32,
    rot: f32,
    color: &[f32; 4],
    scaler: &mut swash::scale::Scaler,
    dest: &mut image::RgbaImage,
) {
    let render_img = swash::scale::Render::new(&[
        swash::scale::Source::ColorOutline(0),
        swash::scale::Source::Outline,
    ])
    .render(scaler, gid);

    if let Some(g_img) = render_img {
        let gx = pen_x + g_img.placement.left as f32;
        let gy = line_y - g_img.placement.top as f32;

        let gw = g_img.placement.width as f32;
        let gh = g_img.placement.height as f32;

        if gw > 0.0 && gh > 0.0 {
            let glyph_cx = gx + gw * 0.5;
            let glyph_cy = gy + gh * 0.5;

            let cx_prime = glyph_cx + dx;
            let cy_prime = glyph_cy + dy;

            // Bounding box of the transformed glyph
            let corners = [
                (gx, gy),
                (gx + gw, gy),
                (gx, gy + gh),
                (gx + gw, gy + gh),
            ];

            let mut tx_min = f32::MAX;
            let mut tx_max = f32::MIN;
            let mut ty_min = f32::MAX;
            let mut ty_max = f32::MIN;

            let rad = rot.to_radians();
            let cos_theta = rad.cos();
            let sin_theta = rad.sin();

            for &(x, y) in &corners {
                let rx = x - glyph_cx;
                let ry = y - glyph_cy;
                let sx_val = rx * sx;
                let sy_val = ry * sy;
                let rot_x = sx_val * cos_theta - sy_val * sin_theta;
                let rot_y = sx_val * sin_theta + sy_val * cos_theta;
                let tx = rot_x + cx_prime;
                let ty = rot_y + cy_prime;

                if tx < tx_min { tx_min = tx; }
                if tx > tx_max { tx_max = tx; }
                if ty < ty_min { ty_min = ty; }
                if ty > ty_max { ty_max = ty; }
            }

            let dest_w = dest.width() as i32;
            let dest_h = dest.height() as i32;

            let start_x = (tx_min.floor() as i32).clamp(0, dest_w);
            let end_x = (tx_max.ceil() as i32).clamp(0, dest_w);
            let start_y = (ty_min.floor() as i32).clamp(0, dest_h);
            let end_y = (ty_max.ceil() as i32).clamp(0, dest_h);

            let data = &g_img.data;
            let expected_mask_len = (gw as i32 * gh as i32) as usize;
            let is_color = data.len() >= expected_mask_len * 4 && expected_mask_len > 0;

            let cos_neg_theta = rad.cos();
            let sin_neg_theta = (-rad).sin();
            let sx_inv = if sx.abs() > 1e-5 { 1.0 / sx } else { 0.0 };
            let sy_inv = if sy.abs() > 1e-5 { 1.0 / sy } else { 0.0 };

            for py in start_y..end_y {
                for px in start_x..end_x {
                    let rx = px as f32 + 0.5 - cx_prime;
                    let ry = py as f32 + 0.5 - cy_prime;

                    let rot_x = rx * cos_neg_theta - ry * sin_neg_theta;
                    let rot_y = rx * sin_neg_theta + ry * cos_neg_theta;

                    let orig_x_rel = rot_x * sx_inv;
                    let orig_y_rel = rot_y * sy_inv;

                    let mx = orig_x_rel + gw * 0.5;
                    let my = orig_y_rel + gh * 0.5;

                    if mx >= 0.0 && mx < gw && my >= 0.0 && my < gh {
                        let x_floor = mx.floor();
                        let y_floor = my.floor();
                        let x_fract = mx - x_floor;
                        let y_fract = my - y_floor;

                        let x0 = (x_floor as i32).clamp(0, gw as i32 - 1) as usize;
                        let x1 = ((x_floor + 1.0) as i32).clamp(0, gw as i32 - 1) as usize;
                        let y0 = (y_floor as i32).clamp(0, gh as i32 - 1) as usize;
                        let y1 = ((y_floor + 1.0) as i32).clamp(0, gh as i32 - 1) as usize;

                        let (src_r, src_g, src_b, src_a) = if is_color {
                            let get_color_pixel = |x: usize, y: usize| -> (f32, f32, f32, f32) {
                                let base = (y * gw as usize + x) * 4;
                                (
                                    data[base] as f32 / 255.0,
                                    data[base + 1] as f32 / 255.0,
                                    data[base + 2] as f32 / 255.0,
                                    data[base + 3] as f32 / 255.0,
                                )
                            };
                            let p00 = get_color_pixel(x0, y0);
                            let p10 = get_color_pixel(x1, y0);
                            let p01 = get_color_pixel(x0, y1);
                            let p11 = get_color_pixel(x1, y1);

                            let r0 = p00.0 * (1.0 - x_fract) + p10.0 * x_fract;
                            let r1 = p01.0 * (1.0 - x_fract) + p11.0 * x_fract;
                            let r = r0 * (1.0 - y_fract) + r1 * y_fract;

                            let g0 = p00.1 * (1.0 - x_fract) + p10.1 * x_fract;
                            let g1 = p01.1 * (1.0 - x_fract) + p11.1 * x_fract;
                            let g = g0 * (1.0 - y_fract) + g1 * y_fract;

                            let b0 = p00.2 * (1.0 - x_fract) + p10.2 * x_fract;
                            let b1 = p01.2 * (1.0 - x_fract) + p11.2 * x_fract;
                            let b = b0 * (1.0 - y_fract) + b1 * y_fract;

                            let a0 = p00.3 * (1.0 - x_fract) + p10.3 * x_fract;
                            let a1 = p01.3 * (1.0 - x_fract) + p11.3 * x_fract;
                            let a = a0 * (1.0 - y_fract) + a1 * y_fract;

                            (r, g, b, a * alpha)
                        } else {
                            let v00 = data[y0 * gw as usize + x0] as f32;
                            let v10 = data[y0 * gw as usize + x1] as f32;
                            let v01 = data[y1 * gw as usize + x0] as f32;
                            let v11 = data[y1 * gw as usize + x1] as f32;

                            let v0 = v00 * (1.0 - x_fract) + v10 * x_fract;
                            let v1 = v01 * (1.0 - x_fract) + v11 * x_fract;
                            let mask_val = v0 * (1.0 - y_fract) + v1 * y_fract;

                            let a = mask_val / 255.0 * alpha;
                            (color[0], color[1], color[2], a)
                        };

                        if src_a <= 0.0 {
                            continue;
                        }

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
        }
    }
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
fn wrap_words<F: Fn(&str) -> f32>(
    text: &str,
    word_width: F,
    space_width: f32,
    max_width: f32,
) -> (f32, Vec<String>) {
    let paragraphs: Vec<&str> = text.split('\n').collect();
    let mut final_lines = Vec::new();
    let mut max_observed_w: f32 = 0.0;

    for para in paragraphs {
        let mut current_line = String::new();
        let mut current_w = 0.0;

        for word in para.split_whitespace() {
            let word_w = word_width(word);

            if current_line.is_empty() {
                current_line.push_str(word);
                current_w = word_w;
            } else if current_w + space_width + word_w <= max_width || max_width <= 0.0 {
                current_line.push(' ');
                current_line.push_str(word);
                current_w += space_width + word_w;
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

    (max_observed_w, final_lines)
}

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

    let (max_observed_w, final_lines) = wrap_words(
        text,
        |word| {
            let mut word_w = 0.0;
            for c in word.chars() {
                let gid = charmap.map(c);
                word_w += glyph_metrics.advance_width(gid) * scale_factor;
            }
            word_w
        },
        space_w,
        max_width,
    );

    let total_h = final_lines.len() as f32 * line_height_px;
    (max_observed_w, total_h, final_lines)
}

/// Word-wrapping helper for the fallback (no font loaded) path.
///
/// Splits on `\n` for paragraphs, then `split_whitespace()` within each
/// paragraph for consistent whitespace handling with the glyph-aware path.
fn wrap_paragraphs_approx(text: &str, char_w: f32, max_width: f32) -> (f32, Vec<String>) {
    wrap_words(
        text,
        |word| word.len() as f32 * char_w,
        char_w,
        max_width,
    )
}

impl ResolvedNode {
    pub fn count_text_units(&self, chars: &mut usize, words: &mut usize, lines_count: &mut usize) {
        match &self.r#type {
            ResolvedNodeType::Spacer { .. } => {}
            ResolvedNodeType::VStack { children, .. }
            | ResolvedNodeType::HStack { children, .. }
            | ResolvedNodeType::ZStack { children, .. } => {
                for child in children {
                    child.count_text_units(chars, words, lines_count);
                }
            }
            ResolvedNodeType::Text { lines, .. } => {
                let mut in_word = false;
                for line in lines {
                    *lines_count += 1;
                    for c in line.chars() {
                        if !c.is_whitespace() {
                            *chars += 1;
                            if !in_word {
                                in_word = true;
                                *words += 1;
                            }
                        } else {
                            in_word = false;
                        }
                    }
                }
            }
        }
    }
}

struct RasterizeState<'a> {
    font_assets: &'a HashMap<String, Vec<u8>>,
    clip_time: f32,
    clip_duration: f32,
    entrance: Option<&'a crate::config::TextTransition>,
    exit: Option<&'a crate::config::TextTransition>,
    char_index: usize,
    word_index: usize,
    line_index: usize,
    total_chars: usize,
    total_words: usize,
    total_lines: usize,
}

fn apply_easing(t: f32, easing: &str) -> f32 {
    match easing.to_ascii_lowercase().as_str() {
        "ease_in" | "ease-in" => t * t,
        "ease_out" | "ease-out" => t * (2.0 - t),
        "ease_in_out" | "ease-in-out" => t * t * (3.0 - 2.0 * t),
        _ => t, // "linear" or unknown
    }
}

fn get_character_progress(
    char_index: usize,
    word_index: usize,
    line_index: usize,
    state: &RasterizeState,
) -> f32 {
    let mut p = 1.0;

    if let Some(ent) = state.entrance {
        // _total is unused here because entrance transitions sequence characters forwards purely from
        // their index offset. Exit transitions, however, need the total count to stagger elements in reverse.
        let (idx, _total) = match ent.granularity.to_ascii_lowercase().as_str() {
            "letter" | "character" => (char_index, state.total_chars),
            "word" => (word_index, state.total_words),
            "line" => (line_index, state.total_lines),
            _ => (char_index, state.total_chars),
        };
        let delay_offset = idx as f32 * ent.delay;
        let start_time = delay_offset;
        let end_time = start_time + ent.duration.max(0.001);
        let p_raw = if state.clip_time <= start_time {
            0.0
        } else if state.clip_time >= end_time {
            1.0
        } else {
            (state.clip_time - start_time) / ent.duration.max(0.001)
        };
        p = apply_easing(p_raw, &ent.easing);
    }

    if let Some(ex) = state.exit {
        let (idx, total) = match ex.granularity.to_ascii_lowercase().as_str() {
            "letter" | "character" => (char_index, state.total_chars),
            "word" => (word_index, state.total_words),
            "line" => (line_index, state.total_lines),
            _ => (char_index, state.total_chars),
        };
        let m_factor = (total.saturating_sub(1) - idx) as f32;
        let start_time = (state.clip_duration - ex.duration - m_factor * ex.delay).max(0.0);
        let end_time = start_time + ex.duration.max(0.001);
        let p_raw = if state.clip_time <= start_time {
            0.0
        } else if state.clip_time >= end_time {
            1.0
        } else {
            (state.clip_time - start_time) / ex.duration.max(0.001)
        };
        let p_exit = apply_easing(p_raw, &ex.easing);
        p *= 1.0 - p_exit;
    }

    p
}

