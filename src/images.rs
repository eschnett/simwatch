//! Loading thumbnails on a background thread, with strict size limits.

use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::thread;

use image::{DynamicImage, ImageError, ImageReader, RgbaImage};
use resvg::{tiny_skia, usvg};

use crate::config::ImageTheme;
use crate::discover::confined_path;
use crate::model::SimId;
use crate::recolor;

/// Image files larger than this are not read
pub const MAX_IMAGE_BYTES: u64 = 1024 * 1024;
/// Images with more pixels than this in either direction are not decoded
pub const MAX_IMAGE_DIM: u32 = 1024;
/// SVG images are drawn this many pixels wide or high, whichever is larger
pub const SVG_SIZE: u32 = 512;
/// The only font for text in SVG images: DejaVu Sans, reduced to Latin,
/// Greek, punctuation, arrows and mathematical operators
const FONT: &[u8] = include_bytes!("../assets/DejaVuSans-subset.ttf");
const FONT_FAMILY: &str = "DejaVu Sans";

/// Identifies an image: the simulation and the path given in its status file
pub type ImageKey = (SimId, String);

/// Fetches the bytes of an image from a remote host: host, simulation
/// directory, and the path given in the status file
pub type Fetch = Box<dyn Fn(&str, &Path, &str) -> Result<Vec<u8>, String> + Send>;

/// Read an image file of a simulation without decoding it
pub fn read_image_bytes(sim_dir: &Path, file: &str) -> Result<Vec<u8>, String> {
    let path = confined_path(sim_dir, file)
        .ok_or_else(|| "missing, or outside the simulation directory".to_string())?;
    let too_large = |len: u64| {
        format!(
            "image too large ({} KiB, max {} KiB)",
            len.div_ceil(1024),
            MAX_IMAGE_BYTES / 1024
        )
    };
    let len = fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if len > MAX_IMAGE_BYTES {
        return Err(too_large(len));
    }
    let mut bytes = Vec::new();
    File::open(&path)
        .and_then(|f| f.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(bytes)
}

/// Decode an image, refusing images that are too large
pub fn decode_image(bytes: &[u8]) -> Result<DynamicImage, String> {
    if is_svg(bytes) {
        return render_svg(bytes).map(DynamicImage::ImageRgba8);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DIM);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(|e| match e {
        ImageError::Limits(_) => {
            format!("image too large (max {MAX_IMAGE_DIM}×{MAX_IMAGE_DIM} pixels)")
        }
        e => e.to_string(),
    })
}

pub fn load_image(sim_dir: &Path, file: &str) -> Result<DynamicImage, String> {
    decode_image(&read_image_bytes(sim_dir, file)?)
}

/// SVG files are XML; no raster format starts with `<`
fn is_svg(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<')
}

/// Options for parsing SVG: the built-in font only, and no files other than
/// the SVG file itself
fn svg_options() -> usvg::Options<'static> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(FONT.to_vec());
        db.set_serif_family(FONT_FAMILY);
        db.set_sans_serif_family(FONT_FAMILY);
        db.set_monospace_family(FONT_FAMILY);
        db.set_cursive_family(FONT_FAMILY);
        db.set_fantasy_family(FONT_FAMILY);
        Arc::new(db)
    });
    let embedded = usvg::ImageHrefResolver::default_data_resolver();
    usvg::Options {
        font_family: FONT_FAMILY.into(),
        fontdb: fontdb.clone(),
        image_href_resolver: usvg::ImageHrefResolver {
            // Embedded images (`data:` URLs) must be nested SVG or small
            // raster images
            resolve_data: Box::new(move |mime, data, opts| {
                let small = ImageReader::new(Cursor::new(&data[..]))
                    .with_guessed_format()
                    .ok()
                    .and_then(|r| r.into_dimensions().ok())
                    .is_some_and(|(w, h)| w <= MAX_IMAGE_DIM && h <= MAX_IMAGE_DIM);
                (small || is_svg(&data))
                    .then(|| embedded(mime, data, opts))
                    .flatten()
            }),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    }
}

/// Draw an SVG image so that its larger side is `SVG_SIZE` pixels
fn render_svg(bytes: &[u8]) -> Result<RgbaImage, String> {
    let tree = usvg::Tree::from_data(bytes, &svg_options()).map_err(|e| format!("SVG: {e}"))?;
    let size = tree.size();
    let scale = SVG_SIZE as f32 / size.width().max(size.height());
    let w = ((size.width() * scale).round() as u32).clamp(1, SVG_SIZE);
    let h = ((size.height() * scale).round() as u32).clamp(1, SVG_SIZE);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("SVG: empty image")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let pixels = pixmap.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    });
    Ok(RgbaImage::from_raw(w, h, pixels.collect()).expect("pixmap size"))
}

/// A background thread that loads and decodes images on request, and adapts
/// them to the terminal's theme
pub struct Loader {
    tx: Sender<ImageKey>,
    pub rx: Receiver<(ImageKey, Result<DynamicImage, String>)>,
}

impl Loader {
    /// `fetch` gets the bytes of images of remote simulations
    pub fn start(fetch: Fetch, theme: ImageTheme) -> Loader {
        let (tx, req_rx) = channel::<ImageKey>();
        let (res_tx, rx) = channel();
        thread::Builder::new()
            .name("simwatch-images".into())
            .spawn(move || {
                for key in req_rx {
                    let ((host, dir), file) = &key;
                    let img = match host {
                        None => load_image(dir, file),
                        Some(h) => fetch(h, dir, file).and_then(|b| decode_image(&b)),
                    }
                    .map(|img| DynamicImage::ImageRgb8(recolor::adapt(img, theme)));
                    if res_tx.send((key, img)).is_err() {
                        return;
                    }
                }
            })
            .expect("spawning image thread");
        Loader { tx, rx }
    }

    pub fn request(&self, key: ImageKey) {
        let _ = self.tx.send(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn limits() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        RgbImage::from_pixel(40, 30, Rgb([1, 2, 3]))
            .save(d.join("small.png"))
            .unwrap();
        // Compresses well, so it is small on disk but has too many pixels
        RgbImage::from_pixel(2000, 10, Rgb([0, 0, 0]))
            .save(d.join("wide.png"))
            .unwrap();
        // Noise does not compress, so the file is large
        let mut noisy = RgbImage::new(1000, 1000);
        for (i, p) in noisy.pixels_mut().enumerate() {
            let x = (i as u32).wrapping_mul(2654435761);
            *p = Rgb([x as u8, (x >> 8) as u8, (x >> 16) as u8]);
        }
        noisy.save(d.join("big.png")).unwrap();
        fs::write(d.join("junk.png"), "not an image").unwrap();

        let img = load_image(d, "small.png").unwrap();
        assert_eq!((img.width(), img.height()), (40, 30));
        assert!(load_image(d, "wide.png").unwrap_err().contains("pixels"));
        assert!(load_image(d, "big.png").unwrap_err().contains("KiB"));
        assert!(load_image(d, "junk.png").is_err());
        assert!(
            load_image(d, "missing.png")
                .unwrap_err()
                .contains("missing")
        );
        assert!(load_image(d, "../small.png").is_err());
    }

    #[test]
    fn formats() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let img = RgbImage::from_pixel(40, 30, Rgb([1, 2, 3]));
        img.save(d.join("a.webp")).unwrap();
        img.save(d.join("a.ppm")).unwrap();
        // Binary PPM written by hand, as a simulation code might
        let mut ppm = b"P6\n# comment\n2 1\n255\n".to_vec();
        ppm.extend([255, 0, 0, 0, 0, 255]);
        fs::write(d.join("b.pnm"), ppm).unwrap();

        for f in ["a.webp", "a.ppm"] {
            let img = load_image(d, f).unwrap();
            assert_eq!((img.width(), img.height()), (40, 30), "{f}");
        }
        let img = load_image(d, "b.pnm").unwrap().into_rgb8();
        assert_eq!(img.get_pixel(1, 0).0, [0, 0, 255]);
    }

    /// The bounding box of the pixels that are not transparent
    fn ink(img: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
        let pts: Vec<_> = img
            .enumerate_pixels()
            .filter(|p| p.2[3] > 0)
            .map(|p| (p.0, p.1))
            .collect();
        Some((
            pts.iter().map(|p| p.0).min()?,
            pts.iter().map(|p| p.1).min()?,
            pts.iter().map(|p| p.0).max()?,
            pts.iter().map(|p| p.1).max()?,
        ))
    }

    #[test]
    fn svg() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        // Only a viewBox: drawn at SVG_SIZE anyway
        let svg = r#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50">
  <rect x="50" y="0" width="50" height="50" fill="red"/>
</svg>"#;
        fs::write(d.join("a.svg"), svg).unwrap();
        let img = load_image(d, "a.svg").unwrap().into_rgba8();
        assert_eq!(img.dimensions(), (SVG_SIZE, SVG_SIZE / 2));
        assert_eq!(img.get_pixel(0, 0).0[3], 0);
        assert_eq!(img.get_pixel(SVG_SIZE - 1, 0).0, [255, 0, 0, 255]);

        // Text is drawn with the built-in font, whatever family is asked for
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
  <text x="10" y="60" font-family="Helvetica, Arial" font-size="30">t = 4.5 αβ ≤</text>
</svg>"#;
        let img = decode_image(svg.as_bytes()).unwrap().into_rgba8();
        assert_eq!(img.dimensions(), (SVG_SIZE, SVG_SIZE / 2));
        let (x0, _, x1, _) = ink(&img).expect("text drawn");
        assert!(x1 - x0 > SVG_SIZE / 2);

        // Files outside the SVG are not read, even next to it
        RgbImage::from_pixel(10, 10, Rgb([0, 255, 0]))
            .save(d.join("secret.png"))
            .unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10">
  <image href="secret.png" width="10" height="10"/>
  <image xlink:href="{}" width="10" height="10"/>
</svg>"#,
            d.join("secret.png").display()
        );
        fs::write(d.join("b.svg"), svg).unwrap();
        assert_eq!(ink(&load_image(d, "b.svg").unwrap().into_rgba8()), None);

        // Embedded images are drawn unless they are too large
        let embed = |w: u32| {
            let mut png = Vec::new();
            RgbImage::from_pixel(w, 10, Rgb([0, 255, 0]))
                .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            let b64 = base64(&png);
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="data:image/png;base64,{b64}" width="10" height="10"/></svg>"#
            );
            ink(&decode_image(svg.as_bytes()).unwrap().into_rgba8())
        };
        assert!(embed(10).is_some());
        assert_eq!(embed(MAX_IMAGE_DIM + 1), None);

        assert!(decode_image(b"<svg").unwrap_err().contains("SVG"));
        assert!(decode_image(b"  <html></html>").is_err());
    }

    fn base64(data: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
            for i in 0..4 {
                s.push(if i <= c.len() {
                    A[(n >> (18 - 6 * i)) as usize & 63] as char
                } else {
                    '='
                });
            }
        }
        s
    }
}
