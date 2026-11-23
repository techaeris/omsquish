#![allow(dead_code)]
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum ImageFormatType {
    Jpeg,
    Png,
    Webp,
    Bmp,
    Tiff,
    Gif,
    Other(String),
}

impl ImageFormatType {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::Webp,
            "bmp" => Self::Bmp,
            "tif" | "tiff" => Self::Tiff,
            "gif" => Self::Gif,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn to_extension(&self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Gif => "gif",
            Self::Other(_) => "jpg",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
            Self::Bmp => "BMP",
            Self::Tiff => "TIFF",
            Self::Gif => "GIF",
            Self::Other(_) => "Image",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutputFormat {
    KeepOriginal,
    Jpeg,
    Png,
    Webp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompressionMode {
    Quality,
    TargetSize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResizeMode {
    Dimensions,
    Percentage,
    Width,
    Height,
}

#[derive(Debug, Clone)]
pub struct CompressionSettings {
    pub mode: CompressionMode,
    pub jpeg_quality: u8,
    pub jpeg_progressive: bool,
    pub png_quality: u8,
    pub png_optimization_level: u8,
    pub webp_quality: u8,
    pub webp_lossless: bool,
    pub lossless: bool,
    pub keep_metadata: bool,
    pub target_size_kb: u32,

    // Resize
    pub resize_enabled: bool,
    pub resize_mode: ResizeMode,
    pub resize_width: u32,
    pub resize_height: u32,
    pub resize_percentage: u32,
    pub keep_aspect_ratio: bool,
    pub do_not_enlarge: bool,

    // Output
    pub same_folder_as_input: bool,
    pub custom_output_folder: Option<PathBuf>,
    pub output_suffix: String,
    pub output_format: OutputFormat,
    pub skip_if_bigger: bool,
    pub keep_dates: bool,
}

impl Default for CompressionSettings {
    fn default() -> Self {
        Self {
            mode: CompressionMode::Quality,
            jpeg_quality: 80,
            jpeg_progressive: true,
            png_quality: 80,
            png_optimization_level: 3,
            webp_quality: 80,
            webp_lossless: false,
            lossless: false,
            keep_metadata: true,
            target_size_kb: 500,

            resize_enabled: false,
            resize_mode: ResizeMode::Dimensions,
            resize_width: 1920,
            resize_height: 1080,
            resize_percentage: 50,
            keep_aspect_ratio: true,
            do_not_enlarge: true,

            same_folder_as_input: true,
            custom_output_folder: None,
            output_suffix: "_compressed".to_string(),
            output_format: OutputFormat::KeepOriginal,
            skip_if_bigger: true,
            keep_dates: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessStatus {
    Ready,
    Processing,
    Finished,
    Skipped(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: usize,
    pub file_path: Option<PathBuf>,
    pub filename: String,
    pub format: ImageFormatType,
    pub original_size: u64,
    pub original_dimensions: (u32, u32),
    pub compressed_size: Option<u64>,
    pub compressed_dimensions: Option<(u32, u32)>,
    pub status: ProcessStatus,
    pub output_path: Option<PathBuf>,

    // Cached raw image for preview
    pub preview_original_bytes: Option<Vec<u8>>,
    pub preview_compressed_bytes: Option<Vec<u8>>,
}

impl QueueItem {
    pub fn savings_bytes(&self) -> i64 {
        if let Some(comp) = self.compressed_size {
            self.original_size as i64 - comp as i64
        } else {
            0
        }
    }

    pub fn savings_percentage(&self) -> f32 {
        if let Some(comp) = self.compressed_size {
            if self.original_size > 0 {
                let diff = self.original_size as f64 - comp as f64;
                (diff / self.original_size as f64 * 100.0) as f32
            } else {
                0.0
            }
        } else {
            0.0
        }
    }
}
