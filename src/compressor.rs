use crate::types::{
    CompressionMode, CompressionSettings, ImageFormatType, OutputFormat,
};
use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageFormat};
use std::io::Cursor;
use std::path::{Path, PathBuf};

pub struct CompressionResult {
    pub compressed_bytes: Vec<u8>,
    pub dimensions: (u32, u32),
    pub output_format: ImageFormatType,
}

pub fn resize_image(
    img: &DynamicImage,
    settings: &CompressionSettings,
) -> (DynamicImage, (u32, u32)) {
    let (orig_w, orig_h) = img.dimensions();
    if !settings.resize_enabled {
        return (img.clone(), (orig_w, orig_h));
    }

    let (mut target_w, mut target_h) = match settings.resize_mode {
        crate::types::ResizeMode::Dimensions => {
            let tw = settings.resize_width.max(1);
            let th = settings.resize_height.max(1);
            if settings.keep_aspect_ratio {
                let ratio = (tw as f64 / orig_w as f64).min(th as f64 / orig_h as f64);
                let rw = (orig_w as f64 * ratio).round() as u32;
                let rh = (orig_h as f64 * ratio).round() as u32;
                (rw.max(1), rh.max(1))
            } else {
                (tw, th)
            }
        }
        crate::types::ResizeMode::Percentage => {
            let pct = (settings.resize_percentage.max(1) as f64) / 100.0;
            let rw = (orig_w as f64 * pct).round() as u32;
            let rh = (orig_h as f64 * pct).round() as u32;
            (rw.max(1), rh.max(1))
        }
        crate::types::ResizeMode::Width => {
            let tw = settings.resize_width.max(1);
            if settings.keep_aspect_ratio {
                let ratio = tw as f64 / orig_w as f64;
                let rh = (orig_h as f64 * ratio).round() as u32;
                (tw, rh.max(1))
            } else {
                (tw, orig_h)
            }
        }
        crate::types::ResizeMode::Height => {
            let th = settings.resize_height.max(1);
            if settings.keep_aspect_ratio {
                let ratio = th as f64 / orig_h as f64;
                let rw = (orig_w as f64 * ratio).round() as u32;
                (rw.max(1), th)
            } else {
                (orig_w, th)
            }
        }
    };

    if settings.do_not_enlarge {
        if target_w > orig_w || target_h > orig_h {
            target_w = orig_w;
            target_h = orig_h;
        }
    }

    if target_w == orig_w && target_h == orig_h {
        (img.clone(), (orig_w, orig_h))
    } else {
        let resized = img.resize_exact(target_w, target_h, FilterType::Lanczos3);
        (resized, (target_w, target_h))
    }
}

pub fn compress_single_format(
    img: &DynamicImage,
    target_format: &ImageFormatType,
    quality: u8,
    settings: &CompressionSettings,
) -> Result<Vec<u8>, String> {
    match target_format {
        ImageFormatType::Jpeg => {
            let mut out = Vec::new();
            let mut encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
            encoder
                .encode_image(img)
                .map_err(|e| format!("JPEG encoding failed: {}", e))?;
            Ok(out)
        }
        ImageFormatType::Webp => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let encoder = webp::Encoder::from_rgba(&rgba, w, h);
            let memory = if settings.webp_lossless || settings.lossless {
                encoder.encode_lossless()
            } else {
                encoder.encode(quality.clamp(1, 100) as f32)
            };
            Ok(memory.to_vec())
        }
        ImageFormatType::Png => {
            let mut raw_png = Vec::new();
            img.write_to(&mut Cursor::new(&mut raw_png), ImageFormat::Png)
                .map_err(|e| format!("PNG initial encoding failed: {}", e))?;

            let level = settings.png_optimization_level.min(6);
            let mut opts = oxipng::Options::from_preset(level);
            opts.fix_errors = true;
            if !settings.keep_metadata {
                opts.strip = oxipng::StripChunks::All;
            }

            match oxipng::optimize_from_memory(&raw_png, &opts) {
                Ok(opt) => Ok(opt),
                Err(_) => Ok(raw_png),
            }
        }
        ImageFormatType::Bmp => {
            let mut out = Vec::new();
            img.write_to(&mut Cursor::new(&mut out), ImageFormat::Bmp)
                .map_err(|e| format!("BMP encoding failed: {}", e))?;
            Ok(out)
        }
        ImageFormatType::Tiff => {
            let mut out = Vec::new();
            img.write_to(&mut Cursor::new(&mut out), ImageFormat::Tiff)
                .map_err(|e| format!("TIFF encoding failed: {}", e))?;
            Ok(out)
        }
        ImageFormatType::Gif => {
            let mut out = Vec::new();
            img.write_to(&mut Cursor::new(&mut out), ImageFormat::Gif)
                .map_err(|e| format!("GIF encoding failed: {}", e))?;
            Ok(out)
        }
        ImageFormatType::Other(_) => {
            let mut out = Vec::new();
            let mut encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
            encoder
                .encode_image(img)
                .map_err(|e| format!("Fallback encoding failed: {}", e))?;
            Ok(out)
        }
    }
}

pub fn compress_image_data(
    input_bytes: &[u8],
    input_format: &ImageFormatType,
    settings: &CompressionSettings,
) -> Result<CompressionResult, String> {
    let img = image::load_from_memory(input_bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    let (processed_img, dimensions) = resize_image(&img, settings);

    let output_format = match settings.output_format {
        OutputFormat::KeepOriginal => input_format.clone(),
        OutputFormat::Jpeg => ImageFormatType::Jpeg,
        OutputFormat::Png => ImageFormatType::Png,
        OutputFormat::Webp => ImageFormatType::Webp,
    };

    let target_quality = match output_format {
        ImageFormatType::Jpeg => settings.jpeg_quality,
        ImageFormatType::Png => settings.png_quality,
        ImageFormatType::Webp => settings.webp_quality,
        _ => 80,
    };

    match settings.mode {
        CompressionMode::Quality => {
            let compressed = compress_single_format(
                &processed_img,
                &output_format,
                target_quality,
                settings,
            )?;
            Ok(CompressionResult {
                compressed_bytes: compressed,
                dimensions,
                output_format,
            })
        }
        CompressionMode::TargetSize => {
            let max_bytes = (settings.target_size_kb as usize) * 1024;
            // Binary search for optimal quality between 5 and 95
            let mut low = 5u8;
            let mut high = 95u8;
            let mut best_bytes = Vec::new();

            for _ in 0..6 {
                let mid = (low + high) / 2;
                let candidate =
                    compress_single_format(&processed_img, &output_format, mid, settings)?;
                let len = candidate.len();

                if len <= max_bytes {
                    best_bytes = candidate;
                    low = mid + 1; // Try higher quality
                } else {
                    if mid <= 5 {
                        best_bytes = candidate;
                        break;
                    }
                    high = mid - 1;
                }
            }

            if best_bytes.is_empty() {
                best_bytes =
                    compress_single_format(&processed_img, &output_format, low, settings)?;
            }

            Ok(CompressionResult {
                compressed_bytes: best_bytes,
                dimensions,
                output_format,
            })
        }
    }
}

pub fn compute_output_destination(
    original_path: Option<&Path>,
    settings: &CompressionSettings,
    target_format: &ImageFormatType,
) -> Option<PathBuf> {
    let orig = original_path?;
    let stem = orig.file_stem()?.to_str()?;
    let ext = target_format.to_extension();

    let target_filename = if settings.output_suffix.is_empty() {
        format!("{}.{}", stem, ext)
    } else {
        format!("{}{}.{}", stem, settings.output_suffix, ext)
    };

    if settings.same_folder_as_input {
        let parent = orig.parent().unwrap_or_else(|| Path::new("."));
        Some(parent.join(target_filename))
    } else if let Some(ref custom_dir) = settings.custom_output_folder {
        Some(custom_dir.join(target_filename))
    } else {
        let parent = orig.parent().unwrap_or_else(|| Path::new("."));
        Some(parent.join(target_filename))
    }
}
