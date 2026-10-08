//! Loading thumbnails on a background thread, with strict size limits.

use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use image::{DynamicImage, ImageError, ImageReader};

use crate::discover::confined_path;
use crate::model::SimId;

/// Image files larger than this are not read
pub const MAX_IMAGE_BYTES: u64 = 1024 * 1024;
/// Images with more pixels than this in either direction are not decoded
pub const MAX_IMAGE_DIM: u32 = 1024;

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

/// A background thread that loads and decodes images on request
pub struct Loader {
    tx: Sender<ImageKey>,
    pub rx: Receiver<(ImageKey, Result<DynamicImage, String>)>,
}

impl Loader {
    /// `fetch` gets the bytes of images of remote simulations
    pub fn start(fetch: Fetch) -> Loader {
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
                    };
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
}
