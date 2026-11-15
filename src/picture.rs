//! Decoding image documents.
//!
//! A picture is decoded once into [`RenderImage`] frames for the reader and
//! into RGBA pixels for the printer. Raster formats (JPEG, PNG, BMP, WebP, ICO
//! and GIF) go through the `image` crate; SVG is parsed and rasterized with
//! `resvg`, so vector drawings stay sharp when they are fitted to the window.

use std::io::Read as _;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use gpui_kit::RenderImage;
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, Frame, ImageFormat, RgbaImage};
use resvg::{tiny_skia, usvg};

/// Longest side a raster picture is kept at, so a huge photo cannot exhaust
/// memory. Larger pictures are scaled down when they are loaded.
const MAX_SIDE: f32 = 8192.0;
/// Longest side an SVG is rasterized to. Small drawings are enlarged so they
/// stay sharp when fitted to the window; large ones stay within [`MAX_SIDE`].
const MIN_SVG_SIDE: f32 = 1024.0;
const MAX_SVG_SIDE: f32 = 4096.0;
/// Animated GIFs are decoded entirely into memory, so their frame count and the
/// amount of pixels they occupy are both capped.
const MAX_GIF_FRAMES: usize = 256;
const MAX_GIF_PIXELS: usize = 64_000_000;

/// The system font database used to resolve text inside SVGs. It is only built
/// the first time an SVG is opened.
static SVG_FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut database = usvg::fontdb::Database::new();
    database.load_system_fonts();
    Arc::new(database)
});

/// A decoded picture, ready to be shown and printed.
pub struct Picture {
    /// The frames as GPUI draws them, in premultiplied BGRA.
    frames: Arc<RenderImage>,
    /// The first frame in straight RGBA, which the printer draws.
    pub pixels: Arc<RgbaImage>,
    /// Size of the decoded image in pixels, used to fit it to the window.
    pub width: f32,
    pub height: f32,
    /// Size declared by the file, which is what the status line reports. It
    /// differs from `width`/`height` for SVG, which is rasterized at a larger
    /// size to stay sharp.
    pub natural_width: f32,
    pub natural_height: f32,
    /// A short name of the format, shown in the status line.
    pub format: &'static str,
}

impl Picture {
    /// Decode `bytes`, which came from a file in `base`.
    ///
    /// `base` is the directory of the document and resolves images referenced
    /// by an SVG.
    pub fn load(bytes: Vec<u8>, base: &Path) -> Result<Self, String> {
        let bytes = decompress(bytes)?;
        if looks_like_svg(&bytes) {
            return load_svg(&bytes, base);
        }

        let format = image::guess_format(&bytes).map_err(|_| {
            String::from("无法识别的图片格式，支持 JPEG、PNG、BMP、WebP、ICO、GIF 与 SVG")
        })?;
        let (frames, pixels) = if format == ImageFormat::Gif {
            decode_gif(&bytes)?
        } else {
            let decoded = image::load_from_memory(&bytes)
                .map_err(|error| format!("无法解码这张图片：{error}"))?;
            let pixels = limit_side(decoded.to_rgba8());
            let mut display = pixels.clone();
            to_render_pixels(&mut display);
            (vec![Frame::new(display)], pixels)
        };

        Self::new(frames, pixels, format_name(format), None)
    }

    /// The image as GPUI draws it, which may hold several animation frames.
    pub fn image(&self) -> Arc<RenderImage> {
        self.frames.clone()
    }

    pub fn frame_count(&self) -> usize {
        self.frames.frame_count()
    }

    fn new(
        frames: Vec<Frame>,
        pixels: RgbaImage,
        format: &'static str,
        natural: Option<(f32, f32)>,
    ) -> Result<Self, String> {
        let (width, height) = pixels.dimensions();
        if width == 0 || height == 0 {
            return Err(String::from("这张图片没有可显示的内容"));
        }
        let (natural_width, natural_height) = natural.unwrap_or((width as f32, height as f32));

        Ok(Self {
            frames: Arc::new(RenderImage::new(frames)),
            pixels: Arc::new(pixels),
            width: width as f32,
            height: height as f32,
            natural_width,
            natural_height,
            format,
        })
    }
}

/// Whether `bytes` look like one of the supported image formats, so a file
/// without an extension can still be opened as a picture.
pub fn looks_like_image(bytes: &[u8]) -> bool {
    looks_like_svg(bytes) || image::guess_format(bytes).is_ok()
}

/// SVG is an XML format, so it cannot be recognized by the magic bytes of the
/// `image` crate.
pub fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{FEFF}').trim_start();
    let lower = text.to_ascii_lowercase();
    lower.starts_with("<svg")
        || ((lower.starts_with("<?xml")
            || lower.starts_with("<!doctype")
            || lower.starts_with("<!--"))
            && lower.contains("<svg"))
}

fn decompress(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0x1F, 0x8B]) {
        return Ok(bytes);
    }

    let mut decoder = flate2::read::GzDecoder::new(bytes.as_slice());
    let mut decoded = Vec::new();
    decoder
        .read_to_end(&mut decoded)
        .map_err(|error| format!("无法解压这张图片：{error}"))?;
    Ok(decoded)
}

fn decode_gif(bytes: &[u8]) -> Result<(Vec<Frame>, RgbaImage), String> {
    let decoder = GifDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("无法解码这张 GIF：{error}"))?;

    let mut decoded = Vec::new();
    let mut total_pixels = 0usize;
    for frame in decoder.into_frames() {
        let frame = frame.map_err(|error| format!("无法解码这张 GIF：{error}"))?;
        let (width, height) = frame.buffer().dimensions();
        let frame_pixels = width as usize * height as usize;
        if !decoded.is_empty()
            && (decoded.len() >= MAX_GIF_FRAMES || total_pixels + frame_pixels > MAX_GIF_PIXELS)
        {
            break;
        }

        total_pixels += frame_pixels;
        decoded.push(frame);
    }

    let Some(first) = decoded.first() else {
        return Err(String::from("这张 GIF 没有可显示的画面"));
    };

    let pixels = limit_side(first.buffer().clone());
    let frames = decoded.iter().map(render_frame).collect();
    Ok((frames, pixels))
}

fn load_svg(bytes: &[u8], base: &Path) -> Result<Picture, String> {
    let mut options = usvg::Options::default();
    options.resources_dir = Some(base.to_path_buf());
    options.fontdb = SVG_FONTS.clone();

    let tree = usvg::Tree::from_data(bytes, &options)
        .map_err(|error| format!("无法解析这个 SVG：{error}"))?;
    let size = tree.size();
    let natural_width = size.width().max(1.0);
    let natural_height = size.height().max(1.0);

    // Rasterize small drawings at a larger size so they stay sharp when fitted
    // to the window, and large ones at a size that still fits in memory.
    let longest = natural_width.max(natural_height);
    let target = (longest * 2.0).clamp(MIN_SVG_SIDE, MAX_SVG_SIDE);
    let scale = target / longest;
    let width = (natural_width * scale).round().max(1.0) as u32;
    let height = (natural_height * scale).round().max(1.0) as u32;

    let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
        return Err(String::from("这个 SVG 的尺寸无效"));
    };
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    // resvg produces premultiplied RGBA: the reader wants BGRA, while the
    // printer takes straight RGBA.
    let mut data = pixmap.take();
    let mut display = data.clone();
    swap_red_blue(&mut display);
    unpremultiply(&mut data);

    let frame = Frame::new(
        RgbaImage::from_raw(width, height, display)
            .ok_or_else(|| String::from("无法读取这个 SVG 的像素"))?,
    );
    let pixels = RgbaImage::from_raw(width, height, data)
        .ok_or_else(|| String::from("无法读取这个 SVG 的像素"))?;

    Picture::new(
        vec![frame],
        pixels,
        "SVG",
        Some((natural_width, natural_height)),
    )
}

/// Convert one decoded frame for GPUI and keep the delay of an animated image.
fn render_frame(frame: &Frame) -> Frame {
    let mut buffer = limit_side(frame.buffer().clone());
    let delay = frame.delay();
    to_render_pixels(&mut buffer);
    Frame::from_parts(buffer, 0, 0, delay)
}

/// GPUI expects premultiplied BGRA, while the decoded pixels are straight
/// RGBA.
pub(crate) fn to_render_pixels(pixels: &mut RgbaImage) {
    let data: &mut [u8] = pixels;
    for pixel in data.as_chunks_mut::<4>().0 {
        let alpha = pixel[3] as u32;
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * alpha + 127) / 255) as u8;
        }
        pixel.swap(0, 2);
    }
}

fn swap_red_blue(data: &mut [u8]) {
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
}

fn unpremultiply(data: &mut [u8]) {
    for pixel in data.as_chunks_mut::<4>().0 {
        let alpha = pixel[3] as u32;
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

/// Scale a picture down when its longest side is larger than [`MAX_SIDE`].
fn limit_side(pixels: RgbaImage) -> RgbaImage {
    let (width, height) = pixels.dimensions();
    let longest = width.max(height) as f32;
    if longest <= MAX_SIDE {
        return pixels;
    }

    let scale = MAX_SIDE / longest;
    let width = ((width as f32 * scale).round() as u32).max(1);
    let height = ((height as f32 * scale).round() as u32).max(1);
    image::imageops::resize(
        &pixels,
        width,
        height,
        image::imageops::FilterType::Triangle,
    )
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Png => "PNG",
        ImageFormat::Bmp => "BMP",
        ImageFormat::WebP => "WebP",
        ImageFormat::Ico => "ICO",
        ImageFormat::Gif => "GIF",
        _ => "图片",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_png_sample() {
        let picture = Picture::load(
            include_bytes!("../samples/sample-image.png").to_vec(),
            Path::new("samples"),
        )
        .expect("the sample image should load");

        assert_eq!(picture.format, "PNG");
        assert_eq!(picture.frame_count(), 1);
        assert!(picture.width > 0.0 && picture.height > 0.0);
    }

    #[test]
    fn rasterizes_svg() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="16">
            <rect width="32" height="16" fill="#f00"/>
        </svg>"##;
        let picture = Picture::load(svg.to_vec(), Path::new(".")).expect("the SVG should load");

        assert_eq!(picture.format, "SVG");
        assert_eq!(picture.frame_count(), 1);
        assert_eq!(
            (picture.natural_width, picture.natural_height),
            (32.0, 16.0)
        );
        // The drawing keeps its 2:1 proportions when it is enlarged.
        assert!((picture.width / picture.height - 2.0).abs() < 0.01);
    }

    #[test]
    fn frames_are_premultiplied_bgra() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2">
            <rect width="2" height="2" fill="#ff0000"/>
        </svg>"##;
        let picture = Picture::load(svg.to_vec(), Path::new(".")).expect("the SVG should load");
        let image = picture.image();
        let bytes = image.as_bytes(0).expect("the frame exists");

        // GPUI textures are BGRA, so an opaque red pixel is 00 00 FF FF.
        assert_eq!(&bytes[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn loads_the_ico_asset() {
        let picture = Picture::load(
            include_bytes!("../assets/icon.ico").to_vec(),
            Path::new("assets"),
        )
        .expect("the icon should load");

        assert_eq!(picture.format, "ICO");
        assert!(picture.width >= 16.0 && picture.height >= 16.0);
    }

    #[test]
    fn keeps_every_gif_frame() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Rgba, RgbaImage};

        let frames = [Rgba([255, 0, 0, 255]), Rgba([0, 0, 255, 255])]
            .into_iter()
            .map(|color| {
                Frame::from_parts(
                    RgbaImage::from_pixel(2, 2, color),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                )
            })
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        GifEncoder::new(&mut bytes)
            .encode_frames(frames)
            .expect("encode the GIF");

        let picture = Picture::load(bytes, Path::new(".")).expect("the GIF should load");
        assert_eq!(picture.format, "GIF");
        assert_eq!(picture.frame_count(), 2);
        assert_eq!(picture.pixels.get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
    }

    #[test]
    fn recognizes_gzipped_svg() {
        use flate2::write::GzEncoder;
        use std::io::Write as _;

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"/>"#;
        let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(svg).expect("compress");
        let bytes = encoder.finish().expect("compress");

        let picture = Picture::load(bytes, Path::new(".")).expect("the gzipped SVG should load");
        assert_eq!(picture.format, "SVG");
    }
}
