//! Adapting thumbnails to a light or dark terminal.
//!
//! Every image is made opaque, on its own background colour. If that does not
//! match the configured theme, the image is split into that background and
//! ink of full strength with a coverage, as in GIMP's "colour to alpha". The
//! ink is recoloured and drawn on white or black: grey ink becomes its
//! opposite, and coloured ink keeps its hue at a lightness that reads well on
//! the new background. Anti-aliased edges stay smooth this way.

use std::collections::HashMap;

use image::{DynamicImage, Rgb, RgbImage, RgbaImage};

use crate::config::ImageTheme;

/// Make an image opaque and flip it if it does not suit the theme
pub fn adapt(img: DynamicImage, theme: ImageTheme) -> RgbImage {
    let img = img.into_rgba8();
    let bg = background(&img);
    let mut out = flatten(&img, bg);
    let dark = lightness(bg) < 0.5;
    let flip = match theme {
        ImageTheme::Light => dark,
        ImageTheme::Dark => !dark,
        ImageTheme::AsIs => false,
    };
    if flip {
        // Plots have few distinct colours
        let mut cache: HashMap<[u8; 3], [u8; 3]> = HashMap::new();
        for p in out.pixels_mut() {
            p.0 = *cache
                .entry(p.0)
                .or_insert_with(|| flip_pixel(p.0, bg, dark));
        }
    }
    out
}

/// The background an image was drawn for: the most common colour of its
/// border; or, if the border is transparent, white if the ink next to the
/// transparent area is more often near black than near white, and black
/// otherwise
fn background(img: &RgbaImage) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let mut counts: HashMap<[u8; 3], usize> = HashMap::new();
    let mut transparent = 0;
    let border = (0..w)
        .flat_map(|x| [(x, 0), (x, h - 1)])
        .chain((0..h).flat_map(|y| [(0, y), (w - 1, y)]));
    for (x, y) in border {
        let [r, g, b, a] = img.get_pixel(x, y).0;
        if a >= 128 {
            *counts.entry([r, g, b]).or_default() += 1;
        } else {
            transparent += 1;
        }
    }
    let opaque: usize = counts.values().sum();
    if opaque >= transparent
        && let Some((c, _)) = counts.into_iter().max_by_key(|&(c, n)| (n, c))
    {
        return c;
    }
    // Count the ink that touches the transparent background: text and axes
    // are near black or near white, coloured lines are neither. (White boxes
    // with black outlines count as black.)
    let clear = |x: u32, y: u32| x >= w || y >= h || img.get_pixel(x, y).0[3] < 128;
    let (mut black, mut white, mut lsum, mut wsum) = (0.0, 0.0, 0.0, 0.0);
    for (x, y, p) in img.enumerate_pixels() {
        let [r, g, b, a] = p.0;
        let edge = a >= 128
            && (clear(x.wrapping_sub(1), y)
                || clear(x + 1, y)
                || clear(x, y.wrapping_sub(1))
                || clear(x, y + 1));
        if !edge {
            continue;
        }
        let l = lightness([r, g, b]);
        if l < 0.25 {
            black += 1.0;
        } else if l > 0.85 {
            white += 1.0;
        }
        lsum += l;
        wsum += 1.0;
    }
    let drawn_for_dark = if black != white {
        white > black
    } else {
        wsum > 0.0 && lsum / wsum > 0.5
    };
    if drawn_for_dark { [0; 3] } else { [255; 3] }
}

/// Composite onto a background colour, in sRGB like most viewers
fn flatten(img: &RgbaImage, bg: [u8; 3]) -> RgbImage {
    RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let [r, g, b, a] = img.get_pixel(x, y).0;
        let a = a as u32;
        let mix = |c: u8, bg: u8| ((c as u32 * a + bg as u32 * (255 - a) + 127) / 255) as u8;
        Rgb([mix(r, bg[0]), mix(g, bg[1]), mix(b, bg[2])])
    })
}

/// Recolour one pixel of an image on background `bg` (dark if `dark`) for
/// the opposite background
fn flip_pixel(c: [u8; 3], bg: [u8; 3], dark: bool) -> [u8; 3] {
    let c = c.map(|x| x as f64 / 255.0);
    let bg = bg.map(|x| x as f64 / 255.0);
    // The coverage: how far the pixel is from the background, towards white
    // (on a dark background) or black (on a light one)
    let mut alpha: f64 = 0.0;
    for i in 0..3 {
        let a = if dark {
            (c[i] - bg[i]) / (1.0 - bg[i])
        } else {
            (bg[i] - c[i]) / bg[i]
        };
        if a.is_finite() {
            alpha = alpha.max(a);
        }
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let new_bg = if dark { 1.0 } else { 0.0 };
    if alpha < 1e-6 {
        return [(new_bg * 255.0) as u8; 3];
    }
    let ink = std::array::from_fn(|i| bg[i] + (c[i] - bg[i]) / alpha);
    let ink = recolour_ink(ink, dark);
    ink.map(|x| ((alpha * x + (1.0 - alpha) * new_bg).clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Lightness of coloured ink: at most this on white, at least this on black
const INK_LIGHTNESS: f64 = 0.6;
/// Ink with at least this OKLab chroma counts as fully coloured
const FULL_CHROMA: f64 = 0.15;

/// Recolour ink (sRGB, 0 to 1) drawn for a dark background (if `dark`) for
/// the opposite one
fn recolour_ink(ink: [f64; 3], dark: bool) -> [f64; 3] {
    let [l, a, b] = oklab_f(ink);
    let grey = 1.0 - l;
    let coloured = if dark {
        l.min(INK_LIGHTNESS)
    } else {
        l.max(INK_LIGHTNESS)
    };
    let w = (a.hypot(b) / FULL_CHROMA).min(1.0);
    let l = w * coloured + (1.0 - w) * grey;
    // Reduce the chroma until the colour fits into sRGB
    let fits = |s: f64| {
        linear_rgb([l, s * a, s * b])
            .iter()
            .all(|x| (-1e-9..=1.0 + 1e-9).contains(x))
    };
    let s = if fits(1.0) {
        1.0
    } else {
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..16 {
            let mid = (lo + hi) / 2.0;
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    };
    linear_rgb([l, s * a, s * b]).map(|x| to_srgb(x.clamp(0.0, 1.0)))
}

fn lightness(rgb: [u8; 3]) -> f64 {
    oklab(rgb)[0]
}

fn to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn oklab(rgb: [u8; 3]) -> [f64; 3] {
    oklab_f(rgb.map(|c| c as f64 / 255.0))
}

/// sRGB (0 to 1) to OKLab (Björn Ottosson's matrices)
fn oklab_f(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(to_linear);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// OKLab to linear sRGB, possibly outside [0, 1]
fn linear_rgb([l, a, b]: [f64; 3]) -> [f64; 3] {
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn hue(rgb: [u8; 3]) -> f64 {
        let [_, a, b] = oklab(rgb);
        b.atan2(a)
    }

    #[test]
    fn flip_colours() {
        let black = [0; 3];
        let white = [255; 3];
        let slate = [17, 20, 24];
        // The background becomes exactly white or black
        assert_eq!(flip_pixel(black, black, true), white);
        assert_eq!(flip_pixel(slate, slate, true), white);
        assert_eq!(flip_pixel(white, white, false), black);
        assert_eq!(flip_pixel(white, slate, true), black);
        assert_eq!(flip_pixel(black, white, false), white);
        let grey = flip_pixel([64; 3], black, true);
        assert!(grey[0] > 160 && grey[0] == grey[1] && grey[1] == grey[2]);

        // Coloured ink keeps its hue at a lightness that reads well
        for c in [
            [255, 0, 0],
            [255, 255, 0],
            [0, 255, 255],
            [255, 165, 0],
            [0, 255, 0],
        ] {
            let f = flip_pixel(c, black, true);
            assert!((hue(f) - hue(c)).abs() < 0.15, "{c:?} → {f:?}");
            assert!(lightness(f) <= INK_LIGHTNESS + 0.01, "{c:?} → {f:?}");
        }
        for c in [[0, 0, 255], [139, 0, 0], [0, 128, 0]] {
            let f = flip_pixel(c, white, false);
            assert!((hue(f) - hue(c)).abs() < 0.15, "{c:?} → {f:?}");
            assert!(lightness(f) >= INK_LIGHTNESS - 0.01, "{c:?} → {f:?}");
        }
        // An anti-aliased edge lies between the line and the new background
        let line = flip_pixel([255, 255, 0], black, true);
        let edge = flip_pixel([128, 128, 0], black, true);
        assert!(lightness(line) < lightness(edge) && lightness(edge) < 1.0);
        assert!((hue(edge) - hue(line)).abs() < 0.15);
    }

    #[test]
    fn themes() {
        let mut dark = RgbaImage::from_pixel(20, 10, Rgba([0, 0, 0, 255]));
        dark.put_pixel(5, 5, Rgba([255, 255, 255, 255]));
        let dark = DynamicImage::ImageRgba8(dark);
        let out = adapt(dark.clone(), ImageTheme::Dark);
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0]);
        let out = adapt(dark.clone(), ImageTheme::AsIs);
        assert_eq!(out.get_pixel(5, 5).0, [255, 255, 255]);
        let out = adapt(dark, ImageTheme::Light);
        assert_eq!(out.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(out.get_pixel(5, 5).0, [0, 0, 0]);

        let light = DynamicImage::ImageRgb8(RgbImage::from_pixel(20, 10, Rgb([250, 250, 250])));
        assert_eq!(
            adapt(light.clone(), ImageTheme::Light).get_pixel(0, 0).0,
            [250; 3]
        );
        assert_eq!(adapt(light, ImageTheme::Dark).get_pixel(0, 0).0, [0; 3]);
    }

    #[test]
    fn transparent() {
        // Black ink on a transparent background was drawn for white
        let mut img = RgbaImage::from_pixel(20, 10, Rgba([0, 0, 0, 0]));
        for x in 2..18 {
            img.put_pixel(x, 5, Rgba([0, 0, 0, 255]));
            img.put_pixel(x, 6, Rgba([255, 160, 0, 255]));
        }
        let img = DynamicImage::ImageRgba8(img);
        let out = adapt(img.clone(), ImageTheme::AsIs);
        assert_eq!(out.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(out.get_pixel(5, 5).0, [0, 0, 0]);
        let out = adapt(img, ImageTheme::Dark);
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0]);
        assert_eq!(out.get_pixel(5, 5).0, [255, 255, 255]);

        // A large white box with a black outline was drawn for white
        let mut img = RgbaImage::from_pixel(20, 10, Rgba([0, 0, 0, 0]));
        for x in 1..19 {
            for y in 1..9 {
                let edge = x == 1 || x == 18 || y == 1 || y == 8;
                let c = if edge { 0 } else { 255 };
                img.put_pixel(x, y, Rgba([c, c, c, 255]));
            }
        }
        let out = adapt(DynamicImage::ImageRgba8(img), ImageTheme::AsIs);
        assert_eq!(out.get_pixel(0, 0).0, [255, 255, 255]);

        // White ink was drawn for black
        let mut img = RgbaImage::from_pixel(20, 10, Rgba([0, 0, 0, 0]));
        img.put_pixel(5, 5, Rgba([255, 255, 255, 128]));
        let out = adapt(DynamicImage::ImageRgba8(img), ImageTheme::AsIs);
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0]);
        assert_eq!(out.get_pixel(5, 5).0, [128, 128, 128]);
    }
}
