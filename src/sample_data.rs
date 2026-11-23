use crate::compressor::compress_single_format;
use crate::types::{CompressionSettings, ImageFormatType, ProcessStatus, QueueItem};
use image::{ImageBuffer, Rgb, Rgba};
use std::io::Cursor;

pub fn generate_sample_images() -> Vec<QueueItem> {
    let mut items = Vec::new();
    let default_settings = CompressionSettings::default();

    // 1. Mountain Sunset landscape (JPEG, 1280x720)
    let w1 = 1280;
    let h1 = 720;
    let mut img1 = ImageBuffer::new(w1, h1);
    for (x, y, pixel) in img1.enumerate_pixels_mut() {
        let xf = x as f32 / w1 as f32;
        let yf = y as f32 / h1 as f32;
        let r = ((1.0 - yf) * 230.0 + (1.0 - xf) * 25.0) as u8;
        let g = ((1.0 - yf) * 110.0 + xf * 40.0) as u8;
        let b = (yf * 180.0 + (xf * 3.14).sin() * 50.0) as u8;
        *pixel = Rgb([r, g, b]);
    }
    let dyn_img1 = image::DynamicImage::ImageRgb8(img1);
    let mut buf1 = Vec::new();
    let _ = dyn_img1.write_to(&mut Cursor::new(&mut buf1), image::ImageFormat::Jpeg);
    let orig_size1 = buf1.len() as u64;

    let comp_bytes1 = compress_single_format(
        &dyn_img1,
        &ImageFormatType::Jpeg,
        80,
        &default_settings,
    )
    .unwrap_or_else(|_| buf1.clone());
    let comp_size1 = comp_bytes1.len() as u64;

    items.push(QueueItem {
        id: 1,
        file_path: None,
        filename: "sunset_landscape.jpg".to_string(),
        format: ImageFormatType::Jpeg,
        original_size: orig_size1,
        original_dimensions: (w1, h1),
        compressed_size: Some(comp_size1),
        compressed_dimensions: Some((w1, h1)),
        status: ProcessStatus::Finished,
        output_path: None,
        preview_original_bytes: Some(buf1),
        preview_compressed_bytes: Some(comp_bytes1),
    });

    // 2. UI Screenshot / Pattern (PNG, 800x600)
    let w2 = 800;
    let h2 = 600;
    let mut img2 = ImageBuffer::new(w2, h2);
    for (x, y, pixel) in img2.enumerate_pixels_mut() {
        let grid = ((x / 32) + (y / 32)) % 2 == 0;
        let r = if grid { 45 } else { 30 };
        let g = if grid { 55 } else { 38 };
        let b = if grid { 72 } else { 50 };
        *pixel = Rgba([r, g, b, 255]);
    }
    let dyn_img2 = image::DynamicImage::ImageRgba8(img2);
    let mut buf2 = Vec::new();
    let _ = dyn_img2.write_to(&mut Cursor::new(&mut buf2), image::ImageFormat::Png);
    let orig_size2 = buf2.len() as u64;

    let comp_bytes2 = compress_single_format(
        &dyn_img2,
        &ImageFormatType::Png,
        80,
        &default_settings,
    )
    .unwrap_or_else(|_| buf2.clone());
    let comp_size2 = comp_bytes2.len() as u64;

    items.push(QueueItem {
        id: 2,
        file_path: None,
        filename: "dashboard_screenshot.png".to_string(),
        format: ImageFormatType::Png,
        original_size: orig_size2,
        original_dimensions: (w2, h2),
        compressed_size: Some(comp_size2),
        compressed_dimensions: Some((w2, h2)),
        status: ProcessStatus::Finished,
        output_path: None,
        preview_original_bytes: Some(buf2),
        preview_compressed_bytes: Some(comp_bytes2),
    });

    // 3. Web Banner Graphic (WebP, 960x360)
    let w3 = 960;
    let h3 = 360;
    let mut img3 = ImageBuffer::new(w3, h3);
    for (x, _y, pixel) in img3.enumerate_pixels_mut() {
        let grad = (x as f32 / w3 as f32 * 255.0) as u8;
        *pixel = Rgb([grad, 120, 255 - grad]);
    }
    let dyn_img3 = image::DynamicImage::ImageRgb8(img3);
    let mut buf3 = Vec::new();
    let _ = dyn_img3.write_to(&mut Cursor::new(&mut buf3), image::ImageFormat::Jpeg);
    let orig_size3 = buf3.len() as u64;

    let comp_bytes3 = compress_single_format(
        &dyn_img3,
        &ImageFormatType::Webp,
        80,
        &default_settings,
    )
    .unwrap_or_else(|_| buf3.clone());
    let comp_size3 = comp_bytes3.len() as u64;

    items.push(QueueItem {
        id: 3,
        file_path: None,
        filename: "omarchy_hero_banner.webp".to_string(),
        format: ImageFormatType::Webp,
        original_size: orig_size3,
        original_dimensions: (w3, h3),
        compressed_size: Some(comp_size3),
        compressed_dimensions: Some((w3, h3)),
        status: ProcessStatus::Finished,
        output_path: None,
        preview_original_bytes: Some(buf3),
        preview_compressed_bytes: Some(comp_bytes3),
    });

    items
}
