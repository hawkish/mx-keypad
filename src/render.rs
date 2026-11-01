//! Render key faces into the JPEG tiles used by the keypad.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use image::{codecs::jpeg::JpegEncoder, ExtendedColorType, ImageEncoder};
use resvg::{tiny_skia, usvg};
use serde::Deserialize;
use thiserror::Error;

use crate::image::KeyImageJpeg;

/// An RGB colour, parsed from six hexadecimal digits with an optional `#`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Rgb(pub u8, pub u8, pub u8);

impl FromStr for Rgb {
    type Err = RenderError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value.strip_prefix('#').unwrap_or(value);
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RenderError::Colour(value.to_owned()));
        }
        let colour =
            u32::from_str_radix(hex, 16).map_err(|_| RenderError::Colour(value.to_owned()))?;
        Ok(Self(
            (colour >> 16) as u8,
            (colour >> 8) as u8,
            colour as u8,
        ))
    }
}

impl TryFrom<String> for Rgb {
    type Error = RenderError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl Rgb {
    fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// An embedded icon name or a path to an SVG, PNG, or JPEG.
#[derive(Debug, Clone, PartialEq)]
pub enum IconSource {
    Builtin(String),
    File(PathBuf),
}

/// What a key displays.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub icon: Option<IconSource>,
    pub label: Option<String>,
    pub background: Rgb,
    pub foreground: Rgb,
    pub glyph_scale: f32,
}

impl Default for Face {
    fn default() -> Self {
        Self {
            icon: None,
            label: None,
            background: Rgb(0, 0, 0),
            foreground: Rgb(255, 255, 255),
            glyph_scale: 0.55,
        }
    }
}

/// Why a key face could not be rendered.
#[derive(Debug, Error)]
pub enum RenderError {
    #[error("unknown builtin icon: {0}")]
    UnknownIcon(String),
    #[error("cannot read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid SVG {what}: {message}")]
    Svg { what: String, message: String },
    #[error("cannot decode image {}: {message}", path.display())]
    Raster { path: PathBuf, message: String },
    #[error("cannot rasterize a {0}-pixel tile")]
    TileSize(u16),
    #[error("cannot encode tile: {0}")]
    Encode(String),
    #[error("invalid colour: {0}")]
    Colour(String),
}

macro_rules! icons {
    ($($name:literal),+ $(,)?) => {
        static ICONS: &[(&str, &[u8])] = &[
            $(($name, include_bytes!(concat!("../icons/", $name, ".svg")))),+
        ];
    };
}

icons!(
    "arrow-left",
    "arrow-right",
    "ban",
    "battery-low",
    "bell",
    "bluetooth",
    "bolt",
    "book-open",
    "bug",
    "calendar",
    "camera",
    "chevron-left",
    "chevron-right",
    "chevrons-down",
    "chevrons-left",
    "chevrons-right",
    "chevrons-up",
    "circle-arrow-left",
    "circle-arrow-right",
    "circle-dot",
    "clipboard-paste",
    "copy",
    "dot",
    "file",
    "folder",
    "gauge",
    "globe",
    "grid-3x3",
    "heart",
    "keyboard",
    "layers",
    "layout-grid",
    "list-checks",
    "lock",
    "monitor",
    "moon",
    "mouse-left",
    "mouse-pointer-click",
    "mouse-right",
    "mouse",
    "move",
    "palette",
    "pencil",
    "play",
    "redo-2",
    "refresh-cw",
    "rotate-ccw",
    "rotate-cw",
    "save",
    "scissors",
    "scroll-text",
    "search",
    "settings",
    "skip-back",
    "skip-forward",
    "square-arrow-left",
    "square-arrow-right",
    "square-plus",
    "square-terminal",
    "square-x",
    "star",
    "terminal",
    "undo-2",
    "unifying",
    "usb",
    "user",
    "volume-1",
    "volume-2",
    "volume-x",
    "x",
);

/// Find an embedded icon by file stem.
pub fn builtin_icon(name: &str) -> Option<&'static [u8]> {
    ICONS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, svg)| *svg)
}

/// Iterate over all embedded icon names.
pub fn builtin_icon_names() -> impl Iterator<Item = &'static str> {
    ICONS.iter().map(|(name, _)| *name)
}

/// Render a square tile and encode it as a baseline quality-90 JPEG.
pub fn render_tile(face: &Face, size: u16) -> Result<KeyImageJpeg, RenderError> {
    let mut pixmap = tiny_skia::Pixmap::new(u32::from(size), u32::from(size))
        .ok_or(RenderError::TileSize(size))?;
    pixmap.fill(tiny_skia::Color::from_rgba8(
        face.background.0,
        face.background.1,
        face.background.2,
        255,
    ));

    let side = f32::from(size);
    let labelled = face.label.as_ref().is_some_and(|s| !s.is_empty());
    let glyph_area = if labelled { side * 0.72 } else { side };
    let fraction = if face.glyph_scale.is_finite() {
        face.glyph_scale.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let span = (side * fraction).min(glyph_area);

    if let Some(icon) = &face.icon {
        match icon {
            IconSource::Builtin(name) => {
                let svg =
                    builtin_icon(name).ok_or_else(|| RenderError::UnknownIcon(name.clone()))?;
                draw_svg(&mut pixmap, svg, name, face.foreground, span, glyph_area)?;
            }
            IconSource::File(path) => {
                let bytes = std::fs::read(path).map_err(|source| RenderError::Io {
                    path: path.clone(),
                    source,
                })?;
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
                {
                    draw_svg(
                        &mut pixmap,
                        &bytes,
                        &path.display().to_string(),
                        face.foreground,
                        span,
                        glyph_area,
                    )?;
                } else {
                    draw_raster(&mut pixmap, &bytes, path, span, glyph_area)?;
                }
            }
        }
    }
    if let Some(label) = face.label.as_deref().filter(|s| !s.is_empty()) {
        draw_label(&mut pixmap, label, face.foreground)?;
    }
    encode(&pixmap, size)
}

fn draw_svg(
    pixmap: &mut tiny_skia::Pixmap,
    bytes: &[u8],
    what: &str,
    colour: Rgb,
    span: f32,
    area: f32,
) -> Result<(), RenderError> {
    let svg = String::from_utf8_lossy(bytes).replace("currentColor", &colour.hex());
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).map_err(|error| {
        RenderError::Svg {
            what: what.to_owned(),
            message: error.to_string(),
        }
    })?;
    if span > 0.0 {
        let scale = span / tree.size().width().max(tree.size().height());
        let x = (pixmap.width() as f32 - tree.size().width() * scale) / 2.0;
        let y = (area - tree.size().height() * scale) / 2.0;
        let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(x, y);
        resvg::render(&tree, transform, &mut pixmap.as_mut());
    }
    Ok(())
}

fn draw_raster(
    pixmap: &mut tiny_skia::Pixmap,
    bytes: &[u8],
    path: &Path,
    span: f32,
    area: f32,
) -> Result<(), RenderError> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| RenderError::Raster {
            path: path.to_owned(),
            message: error.to_string(),
        })?
        .to_rgba8();
    if span < 1.0 || image.width() == 0 || image.height() == 0 {
        return Ok(());
    }
    let scale = span / image.width().max(image.height()) as f32;
    let width = ((image.width() as f32 * scale).round() as u32).max(1);
    let height = ((image.height() as f32 * scale).round() as u32).max(1);
    let resized =
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Lanczos3);
    let x = (pixmap.width() - width) / 2;
    let y = ((area - height as f32) / 2.0).max(0.0) as u32;
    let tile_width = pixmap.width() as usize;
    let pixels = pixmap.data_mut();
    for (px, py, rgba) in resized.enumerate_pixels() {
        let offset = ((y + py) as usize * tile_width + (x + px) as usize) * 4;
        let alpha = u32::from(rgba[3]);
        for channel in 0..3 {
            pixels[offset + channel] = ((u32::from(rgba[channel]) * alpha
                + u32::from(pixels[offset + channel]) * (255 - alpha)
                + 127)
                / 255) as u8;
        }
    }
    Ok(())
}

fn fontdb() -> &'static Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut options = usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        options.fontdb
    })
}

fn draw_label(pixmap: &mut tiny_skia::Pixmap, label: &str, colour: Rgb) -> Result<(), RenderError> {
    // XML markup is not allowed in labels; discard characters XML cannot represent.
    let text = label.chars().filter(|&c| matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'))
        .fold(String::new(), |mut out, c| {
            match c {
                '&' => out.push_str("&amp;"), '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"), _ => out.push(c),
            }
            out
        });
    let side = pixmap.width();
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{side}\" height=\"{side}\"><text x=\"50%\" y=\"88%\" text-anchor=\"middle\" font-family=\"sans-serif\" font-size=\"{}\" fill=\"{}\">{text}</text></svg>",
        side as f32 * 0.16, colour.hex(),
    );
    let options = usvg::Options {
        fontdb: Arc::clone(fontdb()),
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(&svg, &options).map_err(|error| RenderError::Svg {
        what: "label".to_owned(),
        message: error.to_string(),
    })?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    Ok(())
}

fn encode(pixmap: &tiny_skia::Pixmap, size: u16) -> Result<KeyImageJpeg, RenderError> {
    let rgb: Vec<u8> = pixmap
        .data()
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let mut jpeg = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut jpeg, 90)
        .write_image(
            &rgb,
            u32::from(size),
            u32::from(size),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| RenderError::Encode(error.to_string()))?;
    let bytes = jpeg.into_inner();
    let len = bytes.len();
    KeyImageJpeg::new(bytes)
        .ok_or_else(|| RenderError::Encode(format!("{len} bytes do not fit the image write")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bell() -> Face {
        Face {
            icon: Some(IconSource::Builtin("bell".into())),
            ..Face::default()
        }
    }

    #[test]
    fn blank_tile_has_correct_dimensions_and_colour() {
        let face = Face {
            background: Rgb(204, 0, 0),
            ..Face::default()
        };
        let jpeg = render_tile(&face, 118).unwrap();
        let decoded = image::load_from_memory(jpeg.as_bytes()).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (118, 118));
        let pixel = decoded.get_pixel(59, 59);
        assert!((i16::from(pixel[0]) - 204).abs() <= 8 && pixel[1] <= 8 && pixel[2] <= 8);
    }

    #[test]
    fn bell_has_foreground_and_fits_one_packet() {
        let jpeg = render_tile(&bell(), 118).unwrap();
        println!("bell icon: {} bytes", jpeg.as_bytes().len());
        assert!(jpeg.as_bytes().len() <= 4075);
        let decoded = image::load_from_memory(jpeg.as_bytes()).unwrap().to_rgb8();
        assert!(decoded
            .pixels()
            .any(|p| p[0] > 200 && p[1] > 200 && p[2] > 200));
        let labelled = Face {
            label: Some("Bell".into()),
            ..bell()
        };
        let jpeg = render_tile(&labelled, 118).unwrap();
        println!("bell icon and label: {} bytes", jpeg.as_bytes().len());
    }

    #[test]
    fn unknown_icon_is_an_error() {
        let face = Face {
            icon: Some(IconSource::Builtin("not-an-icon".into())),
            ..Face::default()
        };
        assert!(matches!(
            render_tile(&face, 118),
            Err(RenderError::UnknownIcon(_))
        ));
    }

    #[test]
    fn png_file_renders_with_alpha() {
        let path =
            std::env::temp_dir().join(format!("mx-keypad-render-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(12, 24, image::Rgba([255, 0, 0, 128]))
            .save(&path)
            .unwrap();
        let face = Face {
            icon: Some(IconSource::File(path.clone())),
            ..Face::default()
        };
        let jpeg = render_tile(&face, 118).unwrap();
        std::fs::remove_file(path).unwrap();
        let decoded = image::load_from_memory(jpeg.as_bytes()).unwrap().to_rgb8();
        let center = decoded.get_pixel(59, 59);
        assert!((120..=136).contains(&center[0]) && center[1] < 8 && center[2] < 8);
    }

    #[test]
    fn rgb_parsing() {
        assert_eq!("#0a0B0c".parse::<Rgb>().unwrap(), Rgb(10, 11, 12));
        assert_eq!("0a0B0c".parse::<Rgb>().unwrap(), Rgb(10, 11, 12));
        assert!("#12345".parse::<Rgb>().is_err());
        assert!("zzzzzz".parse::<Rgb>().is_err());
        assert_eq!(
            serde_json::from_str::<Rgb>("\"#0a0B0c\"").unwrap(),
            Rgb(10, 11, 12)
        );
        assert!(serde_json::from_str::<Rgb>("\"#12345\"").is_err());
    }

    #[test]
    fn label_is_escaped_and_renders_with_available_fonts() {
        let face = Face {
            label: Some("Bell & <1>\u{0}\u{fffe}".into()),
            ..Face::default()
        };
        let jpeg = render_tile(&face, 118).unwrap();
        if fontdb().faces().next().is_some() {
            let decoded = image::load_from_memory(jpeg.as_bytes()).unwrap().to_rgb8();
            assert!(decoded.pixels().any(|p| p[0] > 100));
        }
    }

    #[test]
    fn rejects_zero_size() {
        assert!(matches!(
            render_tile(&Face::default(), 0),
            Err(RenderError::TileSize(0))
        ));
    }

    #[test]
    fn all_builtin_icons_render() {
        assert_eq!(builtin_icon_names().count(), 70);
        for name in builtin_icon_names() {
            let face = Face {
                icon: Some(IconSource::Builtin(name.into())),
                ..Face::default()
            };
            render_tile(&face, 118).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
