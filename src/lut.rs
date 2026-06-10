//! LUT (color look-up table) loading.
//!
//! Parses `.cube` (Adobe / DaVinci Resolve) and HALD CLUT image files into a 2D
//! "strip" atlas texture that `library/effects/lut.wgsl` samples with manual
//! trilinear interpolation.
//!
//! ## Atlas layout
//!
//! The atlas is a horizontal strip of `size` blue-slices laid left-to-right.
//! Each slice is `size × size` pixels: red varies across X, green down Y. The
//! full atlas is therefore `size*size` wide and `size` tall, and the shader
//! recovers `size` from the texture's height (`textureDimensions(..).y`). For a
//! grid entry `(r, g, b)` the atlas pixel is `(b*size + r, g)`.

use std::path::Path;

/// A parsed 3D LUT: `size³` RGB entries indexed `r + g*size + b*size*size`.
struct Lut3d {
    size: usize,
    /// Flat RGB triples in `[0, 1]`, length `size³ * 3`.
    data: Vec<f32>,
}

impl Lut3d {
    fn get(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        let i = (r + g * self.size + b * self.size * self.size) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }
}

/// Loads a LUT file (`.cube` or HALD image) and returns its strip atlas.
///
/// Panics with a descriptive message on unreadable or malformed input — the
/// engine treats asset-load failures as fatal, matching `load_asset_images`.
pub fn load_lut_atlas(path: &Path) -> image::RgbaImage {
    let is_cube = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("cube"))
        .unwrap_or(false);

    let lut = if is_cube {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("Failed to read LUT file {:?}: {}", path, e));
        parse_cube(&text)
            .unwrap_or_else(|e| panic!("Failed to parse .cube LUT {:?}: {}", path, e))
    } else {
        let img = image::open(path)
            .unwrap_or_else(|e| panic!("Failed to open HALD LUT {:?}: {}", path, e))
            .to_rgba8();
        parse_hald(&img).unwrap_or_else(|e| panic!("Failed to parse HALD LUT {:?}: {}", path, e))
    };

    build_atlas(&lut)
}

/// Parses an Adobe `.cube` file (1D or 3D). Domain is assumed to be `[0, 1]`.
fn parse_cube(text: &str) -> Result<Lut3d, String> {
    let mut size_3d: Option<usize> = None;
    let mut size_1d: Option<usize> = None;
    let mut rows: Vec<[f32; 3]> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let head = tok.next().unwrap();
        match head {
            "LUT_3D_SIZE" => {
                size_3d = Some(parse_usize(tok.next(), "LUT_3D_SIZE")?);
            }
            "LUT_1D_SIZE" => {
                size_1d = Some(parse_usize(tok.next(), "LUT_1D_SIZE")?);
            }
            // Metadata / domain directives we intentionally ignore (domain is [0,1]).
            "TITLE" | "DOMAIN_MIN" | "DOMAIN_MAX" | "LUT_3D_INPUT_RANGE"
            | "LUT_1D_INPUT_RANGE" => {}
            _ => {
                // A data row: three floats. Anything else is unexpected.
                let r: f32 = head.parse().map_err(|_| format!("bad value '{}'", head))?;
                let g: f32 = tok
                    .next()
                    .ok_or("data row missing green")?
                    .parse()
                    .map_err(|_| "bad green value".to_string())?;
                let b: f32 = tok
                    .next()
                    .ok_or("data row missing blue")?
                    .parse()
                    .map_err(|_| "bad blue value".to_string())?;
                rows.push([r, g, b]);
            }
        }
    }

    if let Some(n) = size_3d {
        if rows.len() != n * n * n {
            return Err(format!(
                "LUT_3D_SIZE {} expects {} rows, found {}",
                n,
                n * n * n,
                rows.len()
            ));
        }
        // .cube 3D data is ordered with red varying fastest — identical to our
        // internal index, so the flattened rows map straight through.
        let mut data = Vec::with_capacity(rows.len() * 3);
        for row in &rows {
            data.extend_from_slice(row);
        }
        return Ok(Lut3d { size: n, data });
    }

    if let Some(n) = size_1d {
        if rows.len() != n {
            return Err(format!(
                "LUT_1D_SIZE {} expects {} rows, found {}",
                n,
                n,
                rows.len()
            ));
        }
        // Expand the per-channel curve into a full 3D grid: each output channel
        // is looked up independently against the same 1D table.
        let mut data = Vec::with_capacity(n * n * n * 3);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    data.push(rows[r][0]);
                    data.push(rows[g][1]);
                    data.push(rows[b][2]);
                }
            }
        }
        return Ok(Lut3d { size: n, data });
    }

    Err("missing LUT_3D_SIZE / LUT_1D_SIZE directive".to_string())
}

/// Parses a HALD CLUT image. The image is square with `size³` pixels in
/// red-fastest order, so `size = cbrt(width * height)`.
fn parse_hald(img: &image::RgbaImage) -> Result<Lut3d, String> {
    let total = (img.width() as usize) * (img.height() as usize);
    let size = (total as f64).cbrt().round() as usize;
    if size < 2 || size * size * size != total {
        return Err(format!(
            "image {}x{} ({} px) is not a cubic HALD CLUT",
            img.width(),
            img.height(),
            total
        ));
    }
    let w = img.width();
    let mut data = Vec::with_capacity(total * 3);
    // Pixels are stored row-major in red-fastest order — the flat index already
    // matches our internal layout, so we copy straight through.
    for i in 0..total as u32 {
        let px = img.get_pixel(i % w, i / w);
        data.push(px[0] as f32 / 255.0);
        data.push(px[1] as f32 / 255.0);
        data.push(px[2] as f32 / 255.0);
    }
    Ok(Lut3d { size, data })
}

/// Renders a [`Lut3d`] into its `size*size` × `size` strip atlas.
fn build_atlas(lut: &Lut3d) -> image::RgbaImage {
    let size = lut.size as u32;
    let mut atlas = image::RgbaImage::new(size * size, size);
    for b in 0..lut.size {
        for g in 0..lut.size {
            for r in 0..lut.size {
                let c = lut.get(r, g, b);
                let px = image::Rgba([to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), 255]);
                atlas.put_pixel(b as u32 * size + r as u32, g as u32, px);
            }
        }
    }
    atlas
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn parse_usize(tok: Option<&str>, name: &str) -> Result<usize, String> {
    tok.ok_or_else(|| format!("{} missing value", name))?
        .parse()
        .map_err(|_| format!("{} has invalid value", name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVERT_CUBE: &str = "\
# invert
TITLE \"Invert\"
LUT_3D_SIZE 2
1.0 1.0 1.0
0.0 1.0 1.0
1.0 0.0 1.0
0.0 0.0 1.0
1.0 1.0 0.0
0.0 1.0 0.0
1.0 0.0 0.0
0.0 0.0 0.0
";

    #[test]
    fn cube_3d_invert_atlas() {
        let lut = parse_cube(INVERT_CUBE).unwrap();
        assert_eq!(lut.size, 2);
        // Black input (r=g=b=0) inverts to white.
        assert_eq!(lut.get(0, 0, 0), [1.0, 1.0, 1.0]);
        // White input (r=g=b=1) inverts to black.
        assert_eq!(lut.get(1, 1, 1), [0.0, 0.0, 0.0]);

        let atlas = build_atlas(&lut);
        assert_eq!((atlas.width(), atlas.height()), (4, 2)); // size*size × size
        // (r,g,b) -> atlas pixel (b*size + r, g)
        assert_eq!(atlas.get_pixel(0, 0).0, [255, 255, 255, 255]); // (0,0,0)
        assert_eq!(atlas.get_pixel(3, 1).0, [0, 0, 0, 255]); // (1,1,1)
    }

    #[test]
    fn cube_1d_expands_per_channel() {
        // A 2-entry 1D curve that inverts each channel independently.
        let lut = parse_cube("LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
        assert_eq!(lut.size, 2);
        assert_eq!(lut.get(0, 0, 0), [1.0, 1.0, 1.0]);
        assert_eq!(lut.get(1, 1, 1), [0.0, 0.0, 0.0]);
        // Mixed input picks each channel's curve sample.
        assert_eq!(lut.get(1, 0, 1), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn hald_roundtrip_identity() {
        // Build a size-2 identity HALD (8 px, red-fastest) and parse it back.
        let size = 2usize;
        let total = size * size * size;
        let mut img = image::RgbaImage::new(total as u32, 1);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let i = (r + g * size + b * size * size) as u32;
                    let to8 = |v: usize| (v as f32 / (size - 1) as f32 * 255.0).round() as u8;
                    img.put_pixel(i, 0, image::Rgba([to8(r), to8(g), to8(b), 255]));
                }
            }
        }
        let lut = parse_hald(&img).unwrap();
        assert_eq!(lut.size, 2);
        assert_eq!(lut.get(0, 0, 0), [0.0, 0.0, 0.0]);
        assert_eq!(lut.get(1, 1, 1), [1.0, 1.0, 1.0]);
        assert_eq!(lut.get(1, 0, 0), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn cube_size_mismatch_errors() {
        assert!(parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err());
    }
}
